// SPDX-License-Identifier: GPL-3.0-only

//! 창 생성, 내용 채우기, callback 연결.
//!
//! 모든 창은 처음 필요할 때 만들고, 닫으면 [`super::release_later`]로 instance를 drop한다. 이미 열려
//! 있으면 새로 만들지 않고 앞으로 가져온다.

use std::rc::Rc;

use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, SharedString, VecModel};

use super::commands::AppCommand;
use super::tasks::{DetailReport, RepositoryChange};
use super::{App, EditorView, MainView, dispatch, hide_and_release, release_later, with_app};
use crate::git::refs;
use crate::git::repository::RepositoryId;
use crate::jobs::batch::{PlanTotals, ResultTotals};
use crate::storage;
use crate::storage::repository_store::RepositoryConfig;
use crate::storage::settings_store::{AppSettings, MaintenanceInterval};
use crate::ui::{
    AboutWindow, CleanupDialog, DetailItem, DetailSection, DetailWindow, DialogStage, MainWindow,
    RepositoryEditor, SettingsWindow,
};

/// 확인 dialog에 넣는 숫자 묶음. 확인 단계와 결과 단계가 같은 칸을 쓴다.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct DialogCounts {
    /// 저장소 수.
    repositories: usize,
    /// worktree 수.
    worktrees: usize,
    /// local branch 수.
    branches: usize,
    /// custom ref 수.
    custom_refs: usize,
    /// 건너뛴 항목 수.
    skipped: usize,
    /// 오류 수.
    errors: usize,
}

impl From<PlanTotals> for DialogCounts {
    /// 확인 단계 숫자 (삭제 예정).
    fn from(totals: PlanTotals) -> Self {
        Self {
            repositories: totals.repositories,
            worktrees: totals.worktrees,
            branches: totals.branches,
            custom_refs: totals.custom_refs,
            skipped: totals.skipped,
            errors: totals.errors,
        }
    }
}

impl From<ResultTotals> for DialogCounts {
    /// 결과 단계 숫자 (실제 처리).
    fn from(totals: ResultTotals) -> Self {
        Self {
            repositories: totals.repositories,
            worktrees: totals.worktrees,
            branches: totals.branches,
            custom_refs: totals.custom_refs,
            skipped: totals.skipped,
            errors: totals.errors,
        }
    }
}

/// Slint `int`(i32)로 바꾼다. 넘치는 값은 화면 표시용이므로 최댓값으로 자른다.
fn to_slint_int(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// 문자열 목록을 Slint model로 바꾼다.
fn string_model(lines: &[String]) -> ModelRc<SharedString> {
    let lines: Vec<SharedString> = lines.iter().map(|line| line.as_str().into()).collect();
    ModelRc::new(VecModel::from(lines))
}

/// 메인 창 줄 `index`의 저장소에 대해 `command`를 보낸다. 목록이 그 사이 바뀌어 없으면 무시한다.
fn dispatch_row(index: i32, command: fn(RepositoryId) -> AppCommand) {
    let mut id = None;
    with_app(|app| {
        id = usize::try_from(index)
            .ok()
            .and_then(|index| app.repositories.borrow().get(index).map(|e| e.config.id));
    });
    if let Some(id) = id {
        dispatch(command(id));
    }
}

/// 한 줄에 하나씩 쓴 입력을 목록으로 바꾼다. 앞뒤 공백과 빈 줄은 버린다.
fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

impl App {
    /// 메인 창을 보여준다. 이미 열려 있으면 앞으로 가져온다.
    pub(super) fn open_main_window(&self) {
        if let Err(error) = self.show_main_window() {
            log::error!("failed to open main window: {error}");
        }
    }

    /// 메인 창을 만들거나 앞으로 가져온다.
    fn show_main_window(&self) -> Result<(), slint::PlatformError> {
        if let Some(view) = self.main_window.borrow().as_ref() {
            return view.window.show();
        }
        let window = MainWindow::new()?;
        let rows = Rc::new(VecModel::from(self.repository_rows()));
        window.set_repositories(ModelRc::from(Rc::clone(&rows)));
        window.set_notice(self.notice.borrow().as_str().into());
        connect_main_window(&window);
        window.window().on_close_requested(|| {
            release_later(|app| &app.main_window);
            CloseRequestResponse::HideWindow
        });
        window.show()?;
        log::debug!("main window opened");
        *self.main_window.borrow_mut() = Some(MainView { window, rows });
        self.refresh_busy();
        Ok(())
    }

    /// 확인 dialog를 (없으면 만들어) `stage` 단계로 채워 보여준다.
    pub(super) fn show_cleanup_stage(
        &self,
        stage: DialogStage,
        heading: &str,
        counts: &DialogCounts,
        problems: &[String],
    ) {
        let dialog = match self.cleanup_dialog() {
            Ok(dialog) => dialog,
            Err(error) => {
                log::error!("failed to open cleanup dialog: {error}");
                return;
            }
        };
        dialog.set_stage(stage);
        dialog.set_heading(heading.into());
        dialog.set_repositories(to_slint_int(counts.repositories));
        dialog.set_worktrees(to_slint_int(counts.worktrees));
        dialog.set_branches(to_slint_int(counts.branches));
        dialog.set_custom_refs(to_slint_int(counts.custom_refs));
        dialog.set_skipped(to_slint_int(counts.skipped));
        dialog.set_errors(to_slint_int(counts.errors));
        dialog.set_problems(string_model(problems));
        if let Err(error) = dialog.show() {
            log::error!("failed to show cleanup dialog: {error}");
        }
    }

    /// 확인 dialog handle을 돌려준다. 없으면 새로 만들고 callback을 연결한다.
    fn cleanup_dialog(&self) -> Result<CleanupDialog, slint::PlatformError> {
        if let Some(dialog) = self.cleanup_dialog.borrow().as_ref() {
            return Ok(dialog.clone_strong());
        }
        let dialog = CleanupDialog::new()?;
        dialog.on_confirmed(|| with_app(App::start_cleanup));
        dialog.on_dismissed(|| with_app(|app| app.dismiss_cleanup_dialog(true)));
        dialog.on_cancel_run(|| dispatch(AppCommand::CancelJob));
        dialog.window().on_close_requested(|| {
            with_app(|app| app.dismiss_cleanup_dialog(false));
            CloseRequestResponse::HideWindow
        });
        *self.cleanup_dialog.borrow_mut() = Some(dialog.clone_strong());
        Ok(dialog)
    }

    /// 확인 dialog를 닫는다. 확인 전이면 계획을 버리고, 실행 중이면 작업은 계속하고 창만 닫는다.
    ///
    /// `hide`가 `false`면 창 닫기 버튼으로 이미 숨겨지는 중이라 직접 숨기지 않는다.
    fn dismiss_cleanup_dialog(&self, hide: bool) {
        let discarded = self.pending.borrow_mut().take();
        if discarded.is_some() {
            log::info!("cleanup cancelled before execution");
            self.refresh_busy();
            self.schedule_registration();
            self.resume_maintenance();
        }
        if hide {
            hide_and_release(|app| &app.cleanup_dialog);
        } else {
            release_later(|app| &app.cleanup_dialog);
        }
    }

    /// 상세 창을 `detail` 내용으로 보여준다. 이미 열려 있으면 내용을 바꾼다.
    pub(super) fn open_detail(&self, detail: &DetailReport) {
        let existing = self
            .detail_window
            .borrow()
            .as_ref()
            .map(ComponentHandle::clone_strong);
        let window = match existing {
            Some(window) => window,
            None => match DetailWindow::new() {
                Ok(window) => {
                    window.on_close_requested(|| hide_and_release(|app| &app.detail_window));
                    window.window().on_close_requested(|| {
                        release_later(|app| &app.detail_window);
                        CloseRequestResponse::HideWindow
                    });
                    *self.detail_window.borrow_mut() = Some(window.clone_strong());
                    window
                }
                Err(error) => {
                    log::error!("failed to open detail window: {error}");
                    return;
                }
            },
        };
        window.set_heading(detail.heading.as_str().into());
        window.set_subheading(detail.subheading.as_str().into());
        let sections: Vec<DetailSection> = detail
            .sections
            .iter()
            .map(|(title, lines)| {
                let items: Vec<DetailItem> = lines
                    .iter()
                    .map(|line| DetailItem {
                        title: line.title.as_str().into(),
                        subtitle: line.subtitle.as_str().into(),
                        note: line.note.as_str().into(),
                    })
                    .collect();
                DetailSection {
                    title: (*title).into(),
                    items: ModelRc::new(VecModel::from(items)),
                }
            })
            .collect();
        window.set_sections(ModelRc::new(VecModel::from(sections)));
        if let Err(error) = window.show() {
            log::error!("failed to show detail window: {error}");
        }
    }

    /// 저장소 편집 창을 연다. 다른 저장소를 편집 중이면 그 창을 새 대상으로 다시 채운다.
    pub(super) fn open_editor(&self, id: RepositoryId) {
        let Some(config) = self.find_config(id) else {
            return;
        };
        let existing = self.editor.borrow_mut().as_mut().map(|view| {
            view.id = id;
            view.window.clone_strong()
        });
        let window = match existing {
            Some(window) => window,
            None => match RepositoryEditor::new() {
                Ok(window) => {
                    window.on_save(|| with_app(App::save_editor));
                    window.on_cancel(|| hide_and_release(|app| &app.editor));
                    window.window().on_close_requested(|| {
                        release_later(|app| &app.editor);
                        CloseRequestResponse::HideWindow
                    });
                    *self.editor.borrow_mut() = Some(EditorView {
                        window: window.clone_strong(),
                        id,
                    });
                    window
                }
                Err(error) => {
                    log::error!("failed to open editor: {error}");
                    return;
                }
            },
        };
        window.set_name(config.name.as_str().into());
        window.set_path(config.path.display().to_string().into());
        window.set_enabled(config.enabled);
        window.set_remote(config.remote_name.as_str().into());
        window.set_base_override(
            config
                .base_branch_override
                .as_deref()
                .unwrap_or_default()
                .into(),
        );
        window.set_namespaces(config.custom_ref_namespaces.join("\n").into());
        window.set_error("".into());
        if let Err(error) = window.show() {
            log::error!("failed to show editor: {error}");
        }
    }

    /// 편집 창 입력을 검증하고 저장을 시작한다.
    fn save_editor(&self) {
        let form = self
            .editor
            .borrow()
            .as_ref()
            .map(|view| (view.id, view.window.clone_strong()));
        let Some((id, window)) = form else {
            return;
        };
        let Some(current) = self.find_config(id) else {
            self.set_editor_error("The repository is no longer registered.");
            return;
        };
        match validate_editor(&window, current) {
            Ok(config) => self.change_repositories(RepositoryChange::Update(config), true),
            Err(message) => self.set_editor_error(message),
        }
    }

    /// 편집 창 오류 문구를 바꾼다.
    pub(super) fn set_editor_error(&self, message: &str) {
        if let Some(view) = self.editor.borrow().as_ref() {
            view.window.set_error(message.into());
        }
    }

    /// 설정 창을 연다. 이미 열려 있으면 앞으로 가져온다.
    pub(super) fn open_settings(&self) {
        if let Some(window) = self.settings_window.borrow().as_ref() {
            if let Err(error) = window.show() {
                log::error!("failed to show settings: {error}");
            }
            return;
        }
        let window = match SettingsWindow::new() {
            Ok(window) => window,
            Err(error) => {
                log::error!("failed to open settings: {error}");
                return;
            }
        };
        fill_settings(&window, &self.settings.borrow());
        window.on_save(|| with_app(App::save_settings_form));
        window.on_cancel(|| hide_and_release(|app| &app.settings_window));
        window.on_open_data_dir(|| with_app(App::open_data_dir));
        window.window().on_close_requested(|| {
            release_later(|app| &app.settings_window);
            CloseRequestResponse::HideWindow
        });
        if let Err(error) = window.show() {
            log::error!("failed to show settings: {error}");
        }
        *self.settings_window.borrow_mut() = Some(window);
    }

    /// 설정 창 입력을 검증하고 저장을 시작한다.
    fn save_settings_form(&self) {
        let window = self
            .settings_window
            .borrow()
            .as_ref()
            .map(ComponentHandle::clone_strong);
        let Some(window) = window else {
            return;
        };
        let validated = validate_settings(&window, &self.settings.borrow());
        match validated {
            Ok(settings) => self.save_settings(settings),
            Err(message) => self.set_settings_error(message),
        }
    }

    /// 설정 창 오류 문구를 바꾼다.
    pub(super) fn set_settings_error(&self, message: &str) {
        if let Some(window) = self.settings_window.borrow().as_ref() {
            window.set_error(message.into());
        }
    }

    /// 설정 파일 폴더를 Finder/Explorer로 연다. 첫 실행이라 폴더가 없으면 만든다.
    fn open_data_dir(&self) {
        let Some(dir) = storage::app_data_dir() else {
            self.set_settings_error("The settings folder is unknown on this system.");
            return;
        };
        let opened = std::fs::create_dir_all(&dir)
            .and_then(|()| crate::platform::shell_open::open_folder(&dir));
        if let Err(error) = opened {
            self.set_settings_error(&format!("Could not open the settings folder: {error}"));
        }
    }

    /// About 창을 보여준다. 이미 열려 있으면 앞으로 가져온다.
    pub(super) fn open_about(&self) {
        if let Some(window) = self.about_window.borrow().as_ref() {
            if let Err(error) = window.show() {
                log::error!("failed to show about window: {error}");
            }
            return;
        }
        let window = match AboutWindow::new() {
            Ok(window) => window,
            Err(error) => {
                log::error!("failed to open about window: {error}");
                return;
            }
        };
        window.set_version(env!("CARGO_PKG_VERSION").into());
        window.on_close_requested(|| hide_and_release(|app| &app.about_window));
        window.window().on_close_requested(|| {
            release_later(|app| &app.about_window);
            CloseRequestResponse::HideWindow
        });
        if let Err(error) = window.show() {
            log::error!("failed to show about window: {error}");
        }
        *self.about_window.borrow_mut() = Some(window);
    }
}

/// 메인 창 버튼과 저장소 줄 callback을 [`AppCommand`]로 연결한다.
fn connect_main_window(window: &MainWindow) {
    // OS에서 받은 경로는 그대로 대기 목록에 넣고 검증은 worker에서 수행한다.
    window.window().on_winit_window_event(|_, event| {
        match event {
            WindowEvent::HoveredFile(_) => {
                with_app(|app| app.set_repository_drag_hover(true));
            }
            WindowEvent::HoveredFileCancelled => {
                with_app(|app| app.set_repository_drag_hover(false));
            }
            WindowEvent::DroppedFile(path) => with_app(|app| {
                app.set_repository_drag_hover(false);
                app.enqueue_repositories([path.clone()]);
            }),
            _ => {}
        }
        EventResult::Propagate
    });
    window.on_add_requested(|| dispatch(AppCommand::AddRepository));
    window.on_scan_all_requested(|| dispatch(AppCommand::ScanAll));
    window.on_update_all_requested(|| dispatch(AppCommand::UpdateAll));
    window.on_update_requested(|index| dispatch_row(index, AppCommand::UpdateRepository));
    window.on_clean_all_requested(|| dispatch(AppCommand::CleanAll));
    window.on_settings_requested(|| dispatch(AppCommand::OpenSettings));
    window.on_cancel_requested(|| dispatch(AppCommand::CancelJob));
    window.on_scan_requested(|index| dispatch_row(index, AppCommand::ScanRepository));
    window.on_clean_requested(|index| dispatch_row(index, AppCommand::CleanRepository));
    window.on_details_requested(|index| dispatch_row(index, AppCommand::ShowDetails));
    window.on_edit_requested(|index| dispatch_row(index, AppCommand::EditRepository));
    window.on_toggle_requested(|index| dispatch_row(index, AppCommand::ToggleRepository));
    window.on_open_folder_requested(|index| dispatch_row(index, AppCommand::OpenFolder));
    window.on_open_terminal_requested(|index| dispatch_row(index, AppCommand::OpenTerminal));
    window.on_remove_requested(|index| dispatch_row(index, AppCommand::RemoveRepository));
}

/// 편집 창 입력으로 새 설정을 만든다.
///
/// # Errors
///
/// 이름이나 remote가 비었거나, remote·base branch에 공백이 있거나, namespace가 `refs/remote/`
/// 아래가 아니면 사용자에게 보여줄 문구.
fn validate_editor(
    window: &RepositoryEditor,
    current: RepositoryConfig,
) -> Result<RepositoryConfig, &'static str> {
    let name = window.get_name().trim().to_owned();
    let remote = window.get_remote().trim().to_owned();
    let base = window.get_base_override().trim().to_owned();
    let namespaces = lines(&window.get_namespaces());
    if name.is_empty() {
        return Err("Name must not be empty.");
    }
    if remote.is_empty() || remote.contains(char::is_whitespace) {
        return Err("Remote must be a single name such as origin.");
    }
    if base.contains(char::is_whitespace) {
        return Err("Base branch must not contain spaces.");
    }
    // `refs/remotes/*` 등 정리하면 안 되는 namespace를 여기서 미리 막는다. planner도 다시 검사한다.
    if !namespaces
        .iter()
        .all(|namespace| refs::is_valid_custom_namespace(namespace))
    {
        return Err("Each namespace must be under refs/remote/ and end with /.");
    }
    Ok(RepositoryConfig {
        name,
        enabled: window.get_enabled(),
        remote_name: remote,
        base_branch_override: (!base.is_empty()).then_some(base),
        custom_ref_namespaces: namespaces,
        ..current
    })
}

/// 설정 창을 `settings` 값으로 채운다.
fn fill_settings(window: &SettingsWindow, settings: &AppSettings) {
    window.set_start_at_login(settings.start_at_login);
    window.set_start_minimized(settings.start_minimized);
    window.set_open_on_left_click(settings.open_on_left_click);
    window.set_git_path(
        settings
            .git_executable
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
            .into(),
    );
    window.set_default_remote(settings.default_remote.as_str().into());
    window.set_allow_remote_query(settings.allow_remote_query);
    window.set_protected_patterns(settings.protected_branch_patterns.join("\n").into());
    window.set_confirm_clean_all(settings.confirm_clean_all);
    window.set_auto_maintenance(settings.maintenance_interval_minutes.is_some());
    window.set_interval_minutes(
        settings
            .maintenance_interval_minutes
            .map_or(60, MaintenanceInterval::minutes)
            .to_string()
            .into(),
    );
    window.set_data_dir(
        storage::app_data_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default()
            .into(),
    );
    window.set_error("".into());
}

/// 설정 창 입력으로 새 설정을 만든다. 화면에 없는 값(알림 등)은 `current`를 유지한다.
///
/// # Errors
///
/// 기본 remote가 비었거나 공백이 있으면 사용자에게 보여줄 문구.
fn validate_settings(
    window: &SettingsWindow,
    current: &AppSettings,
) -> Result<AppSettings, &'static str> {
    let default_remote = window.get_default_remote().trim().to_owned();
    if default_remote.is_empty() || default_remote.contains(char::is_whitespace) {
        return Err("Default remote must be a single name such as origin.");
    }
    let maintenance_interval_minutes = if window.get_auto_maintenance() {
        Some(
            window
                .get_interval_minutes()
                .trim()
                .parse::<u32>()
                .ok()
                .and_then(MaintenanceInterval::new)
                .ok_or("Interval must be a whole number between 1 and 10080 minutes.")?,
        )
    } else {
        None
    };
    let git_path = window.get_git_path().trim().to_owned();
    Ok(AppSettings {
        start_at_login: window.get_start_at_login(),
        start_minimized: window.get_start_minimized(),
        open_on_left_click: window.get_open_on_left_click(),
        git_executable: (!git_path.is_empty()).then(|| git_path.into()),
        default_remote,
        allow_remote_query: window.get_allow_remote_query(),
        protected_branch_patterns: lines(&window.get_protected_patterns()),
        confirm_clean_all: window.get_confirm_clean_all(),
        notify_on_completion: current.notify_on_completion,
        maintenance_interval_minutes,
    })
}
