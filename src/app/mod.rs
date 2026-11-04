// SPDX-License-Identifier: GPL-3.0-only

//! application service: tray, 창들, background 작업을 하나의 상태로 묶는다 (PLAN 11장).
//!
//! tray가 application lifetime의 기준이다. 메인 창·확인 dialog·상세·편집·설정·About 창은 필요할
//! 때만 만들고, 닫으면 instance를 drop해 idle 메모리를 tray 수준으로 되돌린다.
//!
//! ```text
//! Scan       : worker(설정 로드 → 저장소 순차 Scan) → 저장소 요약·tray "Last result" 갱신
//! Clean      : worker(Scan) → 확인 dialog → worker(순차 Execute + 이력 저장) → 결과 표시
//! 목록 변경   : worker(repositories.json 읽기 → 변경 → 저장) → 목록 갱신
//! ```
//!
//! 한 번에 작업 하나만 실행한다. Clean 확인을 기다리는 동안에도 새 작업을 받지 않으므로 같은
//! 저장소에 두 작업이 겹치지 않는다 (PLAN 19장).

pub mod commands;
pub mod maintenance;
pub mod registration;
mod schedule;
pub mod state;
pub mod tasks;
pub mod text;
mod windows;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, PlatformError, VecModel};

use crate::error::Result as CleanupResult;
use crate::git::repository::RepositoryId;
use crate::jobs::cancellation::CancellationToken;
use crate::jobs::{self, Job};
use crate::storage::repository_store::RepositoryConfig;
use crate::storage::settings_store::{AppSettings, MaintenanceInterval};
use crate::ui::{
    AboutWindow, AppTray, CleanupDialog, DetailWindow, DialogStage, MainWindow, RepositoryEditor,
    RepositoryRowData, SettingsWindow,
};
use commands::AppCommand;
use registration::{RegistrationQueue, RegistrationReport};
use state::{RepositoryEntry, RepositorySummary, TrayStatus};
use tasks::{
    ChangeError, CleanReport, LoadedState, PendingCleanup, RepositoryChange, ScanReport, ScanScope,
};

/// UI thread에서만 쓰는 application 상태.
struct App {
    /// 앱 lifetime 동안 떠 있는 tray icon.
    tray: AppTray,
    /// 메인 창과 그 목록 model. 닫혀 있으면 `None`.
    main_window: RefCell<Option<MainView>>,
    /// Clean 확인·결과 dialog.
    cleanup_dialog: RefCell<Option<CleanupDialog>>,
    /// 저장소 상세 창.
    detail_window: RefCell<Option<DetailWindow>>,
    /// 저장소 설정 편집 창과 대상 저장소.
    editor: RefCell<Option<EditorView>>,
    /// 설정 창.
    settings_window: RefCell<Option<SettingsWindow>>,
    /// About 창.
    about_window: RefCell<Option<AboutWindow>>,
    /// tray tooltip에 보여줄 상태.
    status: Cell<TrayStatus>,
    /// 실행 중인 background 작업.
    job: RefCell<Option<RunningJob>>,
    /// 사용자 확인을 기다리는 정리 계획. dialog를 닫거나 실행을 시작하면 비운다.
    pending: RefCell<Option<PendingCleanup>>,
    /// 등록 저장소와 화면 요약. 시작 시 한 번 읽고 작업 결과로 갱신한다. 저장소 수에 비례해 작다.
    repositories: RefCell<Vec<RepositoryEntry>>,
    /// 화면 동작에 필요한 설정 사본 (좌클릭 동작, 설정 창 초기값). worker는 파일을 새로 읽는다.
    settings: RefCell<AppSettings>,
    /// 현재 진행 상황 문구. 나중에 연 메인 창에도 보여주기 위해 들고 있는다.
    progress: RefCell<String>,
    /// 메인 창 하단 안내 문구.
    notice: RefCell<String>,
    /// 작업 중 들어온 폴더 경로를 보관한다.
    registration_queue: RefCell<RegistrationQueue>,
    /// 드롭 이벤트를 한 묶음으로 모으는 일회성 timer.
    registration_timer: slint::Timer,
    /// 주기 작업의 남은 시간. 다른 작업 중에는 흐르지 않는다.
    schedule: RefCell<schedule::Schedule>,
    /// 주기 작업 하나만 예약하는 일회성 timer.
    maintenance_timer: slint::Timer,
    /// native 폴더 선택 창의 중복 열기를 막는다.
    folder_picker_open: Cell<bool>,
}

/// 메인 창과 목록 model. 창을 닫으면 함께 drop한다.
struct MainView {
    /// 창 instance.
    window: MainWindow,
    /// 저장소 목록 model. 바뀐 줄만 갱신하도록 재사용한다 (AGENTS 4.2).
    rows: Rc<VecModel<RepositoryRowData>>,
}

/// 편집 창과 편집 중인 저장소.
struct EditorView {
    /// 창 instance.
    window: RepositoryEditor,
    /// 편집 대상.
    id: RepositoryId,
}

/// 실행 중인 작업.
struct RunningJob {
    /// worker thread.
    job: Job,
    /// 사용자가 취소할 수 있는 작업인지 (Scan, Clean). 목록 저장 같은 짧은 작업은 취소하지 않는다.
    cancellable: bool,
}

/// Scan을 시작한 이유. 결과를 받은 뒤 할 일이 다르다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanPurpose {
    /// enabled 저장소 전체 요약.
    ScanAll,
    /// enabled 저장소 전체 정리.
    CleanAll,
    /// 저장소 하나 요약.
    Scan(RepositoryId),
    /// 저장소 하나 정리. Clean All 확인 설정과 상관없이 항상 확인한다.
    Clean(RepositoryId),
    /// 저장소 하나 상세 화면.
    Details(RepositoryId),
}

impl ScanPurpose {
    /// Scan 범위.
    fn scope(self) -> ScanScope {
        match self {
            Self::ScanAll | Self::CleanAll => ScanScope::Enabled,
            Self::Scan(id) | Self::Clean(id) | Self::Details(id) => ScanScope::Only(id),
        }
    }

    /// 결과로 정리 단계에 들어가는지.
    fn cleans(self) -> bool {
        matches!(self, Self::CleanAll | Self::Clean(_))
    }
}

thread_local! {
    /// UI thread의 application 상태. worker 결과 callback은 `Send`여야 해서 `Rc`를 잡을 수 없으므로,
    /// UI thread에서 실행될 때 여기서 꺼내 쓴다.
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// UI thread의 [`App`]에 `f`를 적용한다. 앱이 이미 종료 중이면 아무것도 하지 않는다.
///
/// `f` 실행 중에 Slint callback이 다시 들어와도 `APP` borrow와 겹치지 않도록 `Rc`를 꺼낸 뒤 호출한다.
fn with_app(f: impl FnOnce(&App)) {
    let app = APP.with(|slot| slot.borrow().clone());
    if let Some(app) = app {
        f(&app);
    }
}

/// worker thread에서 UI thread로 `f`를 넘긴다. event loop가 이미 끝났으면 버린다.
fn deliver_to_ui(f: impl FnOnce(&App) + Send + 'static) {
    if let Err(error) = slint::invoke_from_event_loop(move || with_app(f)) {
        log::debug!("dropping job result after event loop ended: {error}");
    }
}

/// worker thread의 진행 상황 문구를 UI thread로 넘기는 callback.
fn progress_to_ui(text: String) {
    deliver_to_ui(move |app| app.set_progress(text));
}

/// tray를 띄우고 Slint event loop를 실행한다. 사용자가 Quit을 선택하면 반환한다.
///
/// 시작 시 설정·저장소 목록만 읽고 Scan은 하지 않는다 (PLAN 28장). 종료할 때 실행 중인 작업이 있으면
/// 취소를 요청하고, 진행 중인 저장소가 끝날 때까지 기다린 뒤 반환한다. 삭제 도중에 process가 끝나
/// Git lock 파일이 남는 일을 막기 위해서다.
///
/// # Errors
///
/// Slint backend 초기화나 event loop 실행에 실패하면 [`PlatformError`].
pub fn run() -> Result<(), PlatformError> {
    let app = Rc::new(App {
        tray: AppTray::new()?,
        main_window: RefCell::new(None),
        cleanup_dialog: RefCell::new(None),
        detail_window: RefCell::new(None),
        editor: RefCell::new(None),
        settings_window: RefCell::new(None),
        about_window: RefCell::new(None),
        status: Cell::new(TrayStatus::Idle),
        job: RefCell::new(None),
        pending: RefCell::new(None),
        repositories: RefCell::new(Vec::new()),
        settings: RefCell::new(AppSettings::default()),
        progress: RefCell::new(String::new()),
        notice: RefCell::new(String::new()),
        registration_queue: RefCell::new(RegistrationQueue::default()),
        registration_timer: slint::Timer::default(),
        schedule: RefCell::new(schedule::Schedule::default()),
        maintenance_timer: slint::Timer::default(),
        folder_picker_open: Cell::new(false),
    });
    connect_tray(&app.tray);
    APP.with(|slot| *slot.borrow_mut() = Some(Rc::clone(&app)));
    app.start_job("load", false, |_| tasks::load_state(), App::finish_load);
    drop(app);

    log::info!("tray started");
    // 보이는 SystemTrayIcon이 있는 동안 loop가 유지되지만, 창을 모두 닫는 순간에도 확실히
    // 살아 있도록 quit_event_loop()가 불릴 때까지 실행한다.
    let result = slint::run_event_loop_until_quit();

    let app = APP.with(|slot| slot.borrow_mut().take());
    if let Some(running) = app.and_then(|app| app.job.borrow_mut().take()) {
        log::info!("waiting for the running job to stop");
        running.job.cancel();
        running.job.join();
    }
    log::info!("tray stopped");
    result
}

/// tray 메뉴 callback을 [`AppCommand`]로 바꿔 [`dispatch`]에 연결한다.
fn connect_tray(tray: &AppTray) {
    tray.on_icon_clicked(|| dispatch(AppCommand::TrayClicked));
    tray.on_open_requested(|| dispatch(AppCommand::OpenMainWindow));
    tray.on_scan_all_requested(|| dispatch(AppCommand::ScanAll));
    tray.on_update_all_requested(|| dispatch(AppCommand::UpdateAll));
    tray.on_clean_all_requested(|| dispatch(AppCommand::CleanAll));
    tray.on_settings_requested(|| dispatch(AppCommand::OpenSettings));
    tray.on_about_requested(|| dispatch(AppCommand::ShowAbout));
    tray.on_quit_requested(|| dispatch(AppCommand::Quit));
}

/// tray와 창에서 들어온 요청을 처리한다. UI thread에서만 호출된다.
fn dispatch(command: AppCommand) {
    log::debug!("command: {command:?}");
    with_app(|app| match command {
        AppCommand::OpenMainWindow => app.open_main_window(),
        AppCommand::TrayClicked => {
            let open = app.settings.borrow().open_on_left_click;
            if open {
                app.open_main_window();
            }
        }
        AppCommand::ScanAll => app.start_scan(ScanPurpose::ScanAll),
        AppCommand::UpdateAll => app.start_maintenance(ScanScope::Enabled, false),
        AppCommand::UpdateRepository(id) => app.start_maintenance(ScanScope::Only(id), false),
        AppCommand::CleanAll => app.start_scan(ScanPurpose::CleanAll),
        AppCommand::CancelJob => app.cancel_job(),
        AppCommand::AddRepository => app.pick_repository_folder(),
        AppCommand::ScanRepository(id) => app.start_scan(ScanPurpose::Scan(id)),
        AppCommand::CleanRepository(id) => app.start_scan(ScanPurpose::Clean(id)),
        AppCommand::ShowDetails(id) => app.start_scan(ScanPurpose::Details(id)),
        AppCommand::EditRepository(id) => app.open_editor(id),
        AppCommand::ToggleRepository(id) => {
            let enabled = app.find_config(id).map(|config| config.enabled);
            if let Some(enabled) = enabled {
                app.change_repositories(RepositoryChange::SetEnabled(id, !enabled), false);
            }
        }
        AppCommand::RemoveRepository(id) => {
            app.change_repositories(RepositoryChange::Remove(id), false);
        }
        AppCommand::OpenFolder(id) => app.open_repository_path(id, false),
        AppCommand::OpenTerminal(id) => app.open_repository_path(id, true),
        AppCommand::OpenSettings => app.open_settings(),
        AppCommand::ShowAbout => app.open_about(),
        AppCommand::Quit => app.quit(),
    });
}

impl App {
    /// 실행 중인 작업이 있거나 Clean 확인을 기다리는 중이면 `true`.
    fn is_busy(&self) -> bool {
        self.job.borrow().is_some() || self.pending.borrow().is_some()
    }

    /// tray 상태를 바꾸고 화면에 반영한다.
    fn set_status(&self, status: TrayStatus) {
        self.status.set(status);
        self.refresh_busy();
    }

    /// busy 여부, tooltip 상태, 진행 문구를 tray와 메인 창에 반영한다.
    ///
    /// `job`/`pending`을 바꾼 뒤에 호출해야 값이 맞는다.
    fn refresh_busy(&self) {
        let busy = self.is_busy();
        let status = self.status.get();
        self.tray.set_busy(busy);
        let label = if status == TrayStatus::Idle {
            ""
        } else {
            status.label()
        };
        self.tray.set_status(label.into());
        let can_cancel = self
            .job
            .borrow()
            .as_ref()
            .is_some_and(|running| running.cancellable);
        if let Some(view) = self.main_window.borrow().as_ref() {
            view.window.set_busy(busy);
            view.window
                .set_picking_folders(self.folder_picker_open.get());
            view.window.set_can_cancel(can_cancel);
            view.window
                .set_progress(self.progress.borrow().as_str().into());
        }
    }

    /// 진행 상황 문구를 바꾼다. 확인 dialog가 실행 단계면 거기에도 보여준다.
    fn set_progress(&self, text: String) {
        if let Some(dialog) = self.cleanup_dialog.borrow().as_ref() {
            dialog.set_progress(text.as_str().into());
        }
        *self.progress.borrow_mut() = text;
        self.refresh_busy();
    }

    /// tray 메뉴의 "Last result" 문구를 바꾼다.
    fn set_last_result(&self, text: &str) {
        log::info!("last result: {text}");
        self.tray.set_last_result(text.into());
    }

    /// 메인 창 하단 안내 문구를 바꾼다. 빈 문자열이면 숨긴다.
    fn set_notice(&self, text: &str) {
        if !text.is_empty() {
            log::info!("notice: {text}");
        }
        text.clone_into(&mut self.notice.borrow_mut());
        if let Some(view) = self.main_window.borrow().as_ref() {
            view.window.set_notice(text.into());
        }
    }

    /// `work`를 background에서 실행하고, 끝나면 UI thread에서 `finish`를 호출한다.
    ///
    /// 이미 작업이 실행 중이면 시작하지 않고 `false`를 돌려준다. `finish`의 두 번째 인자가
    /// `None`이면 worker가 panic했다.
    fn start_job<T: Send + 'static>(
        &self,
        name: &str,
        cancellable: bool,
        work: impl FnOnce(&CancellationToken) -> T + Send + 'static,
        finish: impl FnOnce(&App, Option<T>) + Send + 'static,
    ) -> bool {
        if self.job.borrow().is_some() {
            log::info!("{name} ignored: another job is running");
            return false;
        }
        self.pause_maintenance();
        let spawned = jobs::spawn(name, work, move |output| {
            deliver_to_ui(move |app| {
                app.finish_job();
                finish(app, output);
                app.schedule_registration();
                app.resume_maintenance();
            });
        });
        match spawned {
            Ok(job) => {
                *self.job.borrow_mut() = Some(RunningJob { job, cancellable });
                self.refresh_busy();
                true
            }
            Err(error) => {
                log::error!("failed to start {name}: {error}");
                self.set_notice(&format!("Could not start background work: {error}"));
                self.resume_maintenance();
                false
            }
        }
    }

    /// 끝난 작업을 정리한다. worker는 결과를 넘긴 직후 종료하므로 join은 바로 끝난다.
    fn finish_job(&self) {
        let running = self.job.borrow_mut().take();
        if let Some(running) = running {
            running.job.join();
        }
        self.progress.borrow_mut().clear();
        self.refresh_busy();
    }

    /// 진행 중인 Scan / Clean에 취소를 요청한다.
    fn cancel_job(&self) {
        let cancelled = self.job.borrow().as_ref().is_some_and(|running| {
            if running.cancellable {
                running.job.cancel();
            }
            running.cancellable
        });
        if cancelled {
            self.set_progress(String::from(
                "Cancelling after the current repository finishes…",
            ));
        }
    }

    /// 시작 시 읽은 설정과 저장소 목록을 반영한다.
    fn finish_load(&self, state: Option<LoadedState>) {
        let Some(state) = state else {
            self.set_status(TrayStatus::Error);
            return;
        };
        match state.settings {
            Ok(settings) => *self.settings.borrow_mut() = settings,
            Err(error) => {
                // 기본값으로 화면은 띄우지만, worker가 파일을 다시 읽으므로 정리 작업은 실행되지 않는다.
                log::error!("failed to load settings: {error}");
                self.set_last_result(&format!("Settings file is invalid: {error}"));
                self.set_status(TrayStatus::Error);
            }
        }
        match state.repositories {
            Ok(configs) => {
                log::info!("loaded {} registered repositories", configs.len());
                self.replace_repositories(configs);
                for entry in self.repositories.borrow_mut().iter_mut() {
                    entry.summary.last_cleanup = state.last_cleanup.get(&entry.config.id).copied();
                }
            }
            Err(error) => {
                log::error!("failed to load repositories: {error}");
                self.set_notice(&format!("Could not read the repository list: {error}"));
                self.set_status(TrayStatus::Error);
            }
        }
        self.configure_maintenance();
        self.refresh_rows();
        let start_minimized = self.settings.borrow().start_minimized;
        if !start_minimized {
            self.open_main_window();
        }
    }

    /// 저장소 목록을 `configs`로 바꾼다. 이미 있던 저장소의 요약은 id로 찾아 유지한다.
    fn replace_repositories(&self, configs: Vec<RepositoryConfig>) {
        let mut entries = self.repositories.borrow_mut();
        let mut previous: HashMap<RepositoryId, RepositorySummary> = entries
            .drain(..)
            .map(|entry| (entry.config.id, entry.summary))
            .collect();
        entries.extend(configs.into_iter().map(|config| RepositoryEntry {
            summary: previous.remove(&config.id).unwrap_or_default(),
            config,
        }));
    }

    /// `id` 저장소의 설정 사본.
    fn find_config(&self, id: RepositoryId) -> Option<RepositoryConfig> {
        self.repositories
            .borrow()
            .iter()
            .find(|entry| entry.config.id == id)
            .map(|entry| entry.config.clone())
    }

    /// `id` 저장소 요약을 `update`로 바꾼다.
    fn update_summary(&self, id: RepositoryId, update: impl FnOnce(&mut RepositorySummary)) {
        if let Some(entry) = self
            .repositories
            .borrow_mut()
            .iter_mut()
            .find(|entry| entry.config.id == id)
        {
            update(&mut entry.summary);
        }
    }

    /// Scan을 background에서 시작한다.
    ///
    /// 다른 작업이 진행 중이면 무시한다. Clean 확인 dialog가 떠 있으면 그 창을 앞으로 가져온다.
    fn start_scan(&self, purpose: ScanPurpose) {
        if self.is_busy() {
            log::info!("{purpose:?} ignored: another job is running");
            if let Some(dialog) = self.cleanup_dialog.borrow().as_ref()
                && let Err(error) = dialog.show()
            {
                log::error!("failed to show cleanup dialog: {error}");
            }
            return;
        }
        let scope = purpose.scope();
        let want_detail = matches!(purpose, ScanPurpose::Details(_));
        let started = self.start_job(
            "scan",
            true,
            move |cancel| tasks::scan(scope, want_detail, cancel, &mut progress_to_ui),
            move |app, report| app.finish_scan(purpose, report),
        );
        if started {
            self.set_notice("");
            self.set_status(TrayStatus::Scanning);
        }
    }

    /// Scan worker 결과를 받는다.
    fn finish_scan(&self, purpose: ScanPurpose, report: Option<CleanupResult<ScanReport>>) {
        let report = match report {
            Some(Ok(report)) => report,
            Some(Err(error)) => {
                log::error!("scan aborted: {error}");
                self.set_last_result(&format!("Scan failed: {error}"));
                self.set_notice(&format!("Scan failed: {error}"));
                self.set_status(TrayStatus::Error);
                return;
            }
            None => {
                self.set_last_result("Scan stopped by an internal error");
                self.set_status(TrayStatus::Error);
                return;
            }
        };
        self.replace_repositories(report.repositories);
        for summary in report.summaries {
            self.update_summary(summary.repository_id, |entry| {
                entry.base = summary.base;
                entry.outcome = summary.outcome;
                entry.last_scan = Some(report.finished_at);
            });
        }
        self.refresh_rows();

        let status = if report.totals.errors > 0 {
            TrayStatus::Error
        } else {
            TrayStatus::Idle
        };
        let summary = text::scan_summary(report.scanned, &report.totals);
        self.set_last_result(&summary);

        if let ScanPurpose::Details(_) = purpose {
            match report.detail {
                Some(detail) => self.open_detail(&detail),
                None => self.set_notice(&format!("Could not show details: {summary}")),
            }
        }
        if !purpose.cleans() {
            self.set_status(status);
            return;
        }
        let counts = windows::DialogCounts::from(report.totals);
        if report.totals.removable() == 0 {
            // 사용자가 Clean을 눌렀으니 지울 것이 없다는 것도 창으로 알려준다.
            self.show_cleanup_stage(DialogStage::Done, &summary, &counts, &report.problems);
            self.set_status(status);
            return;
        }
        let heading = format!("Clean up {} items?", report.totals.removable());
        *self.pending.borrow_mut() = Some(report.pending);
        let confirm = report.confirm_clean_all || matches!(purpose, ScanPurpose::Clean(_));
        if confirm {
            self.show_cleanup_stage(DialogStage::Confirm, &heading, &counts, &report.problems);
            self.set_status(status);
        } else {
            self.start_cleanup();
        }
    }

    /// 확인을 기다리던 계획을 background에서 실행한다.
    fn start_cleanup(&self) {
        let pending = self.pending.borrow_mut().take();
        let Some(pending) = pending else {
            return;
        };
        let started = self.start_job(
            "cleanup",
            true,
            move |cancel| tasks::clean(&pending, cancel, &mut progress_to_ui),
            App::finish_cleanup,
        );
        if started {
            if let Some(dialog) = self.cleanup_dialog.borrow().as_ref() {
                dialog.set_progress("".into());
                dialog.set_stage(DialogStage::Running);
            }
            self.set_status(TrayStatus::CleaningUp);
        } else {
            self.set_last_result("Could not start cleanup");
            if let Some(dialog) = self.cleanup_dialog.borrow().as_ref() {
                dialog.set_heading("Could not start cleanup".into());
                dialog.set_stage(DialogStage::Done);
            }
            self.set_status(TrayStatus::Error);
        }
    }

    /// Clean worker 결과를 받는다.
    ///
    /// 확인 dialog가 아직 열려 있으면 결과 단계로 바꾸고, 닫혔으면 tray 문구만 바꾼다.
    fn finish_cleanup(&self, report: Option<CleanReport>) {
        let has_dialog = self.cleanup_dialog.borrow().is_some();
        let Some(report) = report else {
            let text = "Cleanup stopped by an internal error";
            self.set_last_result(text);
            if has_dialog {
                let counts = windows::DialogCounts::default();
                self.show_cleanup_stage(DialogStage::Done, text, &counts, &[]);
            }
            self.set_status(TrayStatus::Error);
            return;
        };
        for (id, outcome) in report.summaries {
            self.update_summary(id, |summary| {
                summary.outcome = outcome;
                summary.last_cleanup = Some(report.finished_at);
            });
        }
        self.refresh_rows();
        let totals = report.totals;
        let summary = text::cleanup_summary(&totals, report.cancelled);
        self.set_last_result(&summary);
        if has_dialog {
            let counts = windows::DialogCounts::from(totals);
            self.show_cleanup_stage(DialogStage::Done, &summary, &counts, &report.problems);
        }
        // 오류는 Error, 재검증으로 남긴 항목은 사용자가 확인해 볼 수 있도록 Attention required.
        self.set_status(if totals.errors > 0 {
            TrayStatus::Error
        } else if totals.skipped > 0 {
            TrayStatus::AttentionRequired
        } else {
            TrayStatus::Idle
        });
    }

    /// native 다중 폴더 선택 창을 띄운다. 선택 중에는 같은 창을 다시 열지 않는다.
    fn pick_repository_folder(&self) {
        if self.is_busy() || self.folder_picker_open.replace(true) {
            return;
        }
        self.pause_maintenance();
        self.refresh_busy();
        let spawned = slint::spawn_local(async {
            let folders = rfd::AsyncFileDialog::new()
                .set_title("Add Repositories")
                .pick_folders()
                .await;
            with_app(|app| {
                app.folder_picker_open.set(false);
                app.refresh_busy();
                if let Some(folders) = folders {
                    app.enqueue_repositories(
                        folders
                            .into_iter()
                            .map(|folder| folder.path().to_path_buf()),
                    );
                } else {
                    app.schedule_registration();
                }
                app.resume_maintenance();
            });
        });
        if let Err(error) = spawned {
            self.folder_picker_open.set(false);
            self.refresh_busy();
            log::error!("failed to open folder picker: {error}");
            self.set_notice(
                "Could not open the folder picker. You can drop repository folders here.",
            );
            self.schedule_registration();
            self.resume_maintenance();
        }
    }

    /// 외부 드래그가 창에 들어오거나 나갔을 때 추가 안내를 표시한다.
    fn set_repository_drag_hover(&self, hovering: bool) {
        if let Some(view) = self.main_window.borrow().as_ref() {
            view.window.set_drag_hover(hovering);
        }
    }

    /// 다중 선택과 드롭 경로를 합친다. Git과 파일 시스템 검증은 worker에서만 수행한다.
    fn enqueue_repositories(&self, paths: impl IntoIterator<Item = PathBuf>) {
        self.registration_queue.borrow_mut().extend(paths);
        if self.is_busy() && !self.registration_queue.borrow().is_empty() {
            self.set_notice(
                "Repository folders queued. They will be added after the current operation.",
            );
        }
        self.schedule_registration();
    }

    /// 입력이 있을 때만 일회성 timer를 예약해 여러 파일 드롭 이벤트를 한 묶음으로 모은다.
    fn schedule_registration(&self) {
        if self.is_busy()
            || self.folder_picker_open.get()
            || self.registration_queue.borrow().is_empty()
        {
            return;
        }
        self.registration_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(100),
            || with_app(App::start_registration),
        );
    }

    /// 다른 작업이 끝났을 때 대기 경로를 worker에 넘긴다. 먼저 시작한 작업이 있으면 보관한다.
    fn start_registration(&self) {
        self.registration_queue.borrow_mut().start_ready(
            self.is_busy() || self.folder_picker_open.get(),
            |paths| {
                self.start_job(
                    "register",
                    false,
                    move |_| {
                        registration::add_repositories(
                            &tasks::data_dir()?,
                            &paths,
                            &mut progress_to_ui,
                        )
                    },
                    App::finish_registration,
                )
            },
        );
    }

    /// 일괄 등록 요약을 표시하고, 실패한 폴더가 있으면 이유를 상세 창에서 보여준다.
    fn finish_registration(&self, result: Option<CleanupResult<RegistrationReport>>) {
        let report = match result {
            Some(Ok(report)) => report,
            Some(Err(error)) => {
                self.set_notice(&format!("Could not add repositories: {error}"));
                return;
            }
            None => {
                self.set_notice("Repository registration stopped by an internal error.");
                return;
            }
        };
        self.replace_repositories(report.repositories);
        self.refresh_rows();
        let summary = format!(
            "Added {} repositories · {} already registered · {} failed",
            report.added, report.duplicates, report.failed,
        );
        self.set_notice(&summary);
        if report.failed > 0 {
            self.open_detail(&tasks::DetailReport {
                heading: String::from("Repository registration"),
                subheading: summary,
                sections: vec![(
                    "Not added (up to 50 folders)",
                    report
                        .problems
                        .into_iter()
                        .map(|problem| tasks::DetailLine {
                            title: problem.path.display().to_string(),
                            subtitle: problem.message,
                            note: String::new(),
                        })
                        .collect(),
                )],
            });
        }
    }

    /// 저장소 목록 변경을 background에서 저장한다. `from_editor`면 결과를 편집 창에 알린다.
    fn change_repositories(&self, change: RepositoryChange, from_editor: bool) {
        let started = self.start_job(
            "repositories",
            false,
            move |_| tasks::apply_change(change),
            move |app, result| app.finish_change(result, from_editor),
        );
        if !started && from_editor {
            self.set_editor_error("Another job is running. Try again when it finishes.");
        }
    }

    /// 저장소 목록 변경 결과를 반영한다.
    fn finish_change(
        &self,
        result: Option<Result<Vec<RepositoryConfig>, ChangeError>>,
        from_editor: bool,
    ) {
        let message = match result {
            Some(Ok(configs)) => {
                self.replace_repositories(configs);
                self.refresh_rows();
                self.set_notice("");
                if from_editor {
                    hide_and_release(|app| &app.editor);
                }
                return;
            }
            Some(Err(error)) => error.to_string(),
            None => String::from("internal error"),
        };
        log::warn!("repository change failed: {message}");
        if from_editor {
            self.set_editor_error(&format!("Could not save: {message}"));
        } else {
            self.set_notice(&format!("Could not update repositories: {message}"));
        }
    }

    /// 저장소 폴더를 Finder/Explorer(`terminal`이 `false`) 또는 terminal로 연다.
    fn open_repository_path(&self, id: RepositoryId, terminal: bool) {
        let Some(config) = self.find_config(id) else {
            return;
        };
        let opened = if terminal {
            crate::platform::shell_open::open_terminal(&config.path)
        } else {
            crate::platform::shell_open::open_folder(&config.path)
        };
        if let Err(error) = opened {
            self.set_notice(&format!("Could not open {}: {error}", config.name));
        }
    }

    /// 설정 저장을 background에서 시작한다. 성공하면 설정 사본을 바꾸고 설정 창을 닫는다.
    fn save_settings(&self, settings: AppSettings) {
        let apply_autostart = self.settings.borrow().start_at_login != settings.start_at_login;
        let started = self.start_job(
            "settings",
            false,
            move |_| tasks::save_settings(&settings, apply_autostart).map(|()| settings),
            |app, result| match result {
                Some(Ok(settings)) => {
                    let interval_changed = app.settings.borrow().maintenance_interval_minutes
                        != settings.maintenance_interval_minutes;
                    *app.settings.borrow_mut() = settings;
                    if interval_changed {
                        app.configure_maintenance();
                    }
                    hide_and_release(|app| &app.settings_window);
                }
                Some(Err(error)) => app.set_settings_error(&format!("Could not save: {error}")),
                None => app.set_settings_error("Could not save: internal error"),
            },
        );
        if !started {
            self.set_settings_error("Another job is running. Try again when it finishes.");
        }
    }

    /// 앱을 종료한다. 실행 중인 작업에는 취소를 요청하고, 확인 대기 중인 계획은 버린다.
    fn quit(&self) {
        self.maintenance_timer.stop();
        if let Some(running) = self.job.borrow().as_ref() {
            running.job.cancel();
        }
        self.pending.borrow_mut().take();
        if let Err(error) = slint::quit_event_loop() {
            log::error!("failed to quit event loop: {error}");
        }
    }

    /// 사용자가 저장한 간격을 적용한다. 꺼져 있으면 timer를 남기지 않는다.
    fn configure_maintenance(&self) {
        self.maintenance_timer.stop();
        self.schedule.borrow_mut().configure(
            self.settings
                .borrow()
                .maintenance_interval_minutes
                .map(MaintenanceInterval::duration),
        );
        self.resume_maintenance();
    }

    /// 다른 작업을 시작하기 전에 남은 시간을 보관하고 OS timer를 멈춘다.
    fn pause_maintenance(&self) {
        self.maintenance_timer.stop();
        self.schedule.borrow_mut().pause(Instant::now());
    }

    /// 작업과 확인 대기가 모두 끝나면 남은 시간부터 재개한다. polling은 하지 않는다.
    fn resume_maintenance(&self) {
        if self.is_busy() || self.folder_picker_open.get() {
            return;
        }
        let delay = self.schedule.borrow_mut().resume(Instant::now());
        if let Some(delay) = delay {
            self.maintenance_timer
                .start(slint::TimerMode::SingleShot, delay, || {
                    with_app(|app| {
                        app.schedule.borrow_mut().fired();
                        app.start_maintenance(ScanScope::Enabled, true);
                    });
                });
        }
    }

    /// 수동 최신화 또는 미리 동의한 주기 최신화·정리를 하나의 worker에서 실행한다.
    fn start_maintenance(&self, scope: ScanScope, clean: bool) {
        if self.is_busy() {
            return;
        }
        let started = self.start_job(
            "update",
            true,
            move |cancel| {
                maintenance::run(
                    &tasks::data_dir()?,
                    scope,
                    clean,
                    cancel,
                    &mut progress_to_ui,
                )
            },
            move |app, report| app.finish_maintenance(report, !clean),
        );
        if started {
            self.set_notice("");
            self.set_status(TrayStatus::Updating);
        }
    }

    /// 최신화 결과와 주기 정리 결과를 목록·tray에 반영한다. 자동 실행은 창을 만들지 않는다.
    fn finish_maintenance(
        &self,
        report: Option<CleanupResult<maintenance::MaintenanceReport>>,
        show_detail: bool,
    ) {
        let report = match report {
            Some(Ok(report)) => report,
            Some(Err(error)) => {
                self.set_last_result(&format!("Update failed: {error}"));
                self.set_notice(&format!("Update failed: {error}"));
                self.set_status(TrayStatus::Error);
                return;
            }
            None => {
                self.set_last_result("Update stopped by an internal error");
                self.set_status(TrayStatus::Error);
                return;
            }
        };
        self.replace_repositories(report.repositories);
        for (id, outcome) in report.summaries {
            self.update_summary(id, |summary| summary.outcome = outcome);
        }
        let mut errors = report.errors;
        let mut skipped = report.skipped;
        let mut summary = format!(
            "Updated {} branches · {} skipped · {} errors",
            report.updated, skipped, errors
        );
        let mut problems = report.problems;
        if let Some(cleanup) = report.cleanup {
            errors += cleanup.totals.errors;
            skipped += cleanup.totals.skipped;
            summary.push_str(" · ");
            summary.push_str(&text::cleanup_summary(&cleanup.totals, cleanup.cancelled));
            for (id, outcome) in cleanup.summaries {
                self.update_summary(id, |entry| {
                    entry.outcome = outcome;
                    entry.last_cleanup = Some(cleanup.finished_at);
                });
            }
            problems.extend(
                cleanup
                    .problems
                    .into_iter()
                    .take(50_usize.saturating_sub(problems.len())),
            );
        }
        if report.cancelled {
            summary.push_str(" (cancelled)");
        }
        self.set_last_result(&summary);
        self.set_notice(&summary);
        if show_detail && !problems.is_empty() {
            self.open_detail(&tasks::DetailReport {
                heading: String::from("Fetch and update"),
                subheading: summary,
                sections: vec![(
                    "Skipped and errors (up to 50)",
                    problems
                        .into_iter()
                        .map(|message| tasks::DetailLine {
                            title: message,
                            subtitle: String::new(),
                            note: String::new(),
                        })
                        .collect(),
                )],
            });
        }
        self.refresh_rows();
        self.set_status(if errors > 0 {
            TrayStatus::Error
        } else if skipped > 0 {
            TrayStatus::AttentionRequired
        } else {
            TrayStatus::Idle
        });
    }

    /// 메인 창 목록을 현재 저장소 상태로 맞춘다. 창이 닫혀 있으면 아무것도 하지 않는다.
    fn refresh_rows(&self) {
        if let Some(view) = self.main_window.borrow().as_ref() {
            let rows =
                crate::ui::models::repository_rows(&self.repositories.borrow(), tasks::now_secs());
            crate::ui::models::sync_rows(&view.rows, rows);
        }
    }
}

/// `slot`의 창 instance를 다음 event loop 회차에 drop한다.
///
/// close callback 안에서 자기 자신을 drop하지 않도록 미룬다. drop 중 callback이 다시 들어와도
/// `RefCell` borrow와 겹치지 않도록 먼저 꺼낸 뒤 drop한다.
fn release_later<T: 'static>(slot: fn(&App) -> &RefCell<Option<T>>) {
    slint::Timer::single_shot(Duration::ZERO, move || {
        with_app(|app| {
            let window = slot(app).borrow_mut().take();
            if window.is_some() {
                log::debug!("window released");
            }
            drop(window);
        });
    });
}

/// 창을 숨기고 instance를 다음 회차에 drop한다. 창 안의 Close/Cancel 버튼에서 쓴다.
///
/// slot에 창 자체가 아니라 창을 담은 구조체가 들어 있을 수 있어 [`AsWindow`]로 꺼낸다.
fn hide_and_release<T: AsWindow + 'static>(slot: fn(&App) -> &RefCell<Option<T>>) {
    with_app(|app| {
        if let Some(holder) = slot(app).borrow().as_ref()
            && let Err(error) = holder.window().hide()
        {
            log::error!("failed to hide window: {error}");
        }
    });
    release_later(slot);
}

/// slot에 들어 있는 값에서 Slint 창을 꺼낸다.
trait AsWindow {
    /// 창의 [`slint::Window`].
    fn window(&self) -> &slint::Window;
}

/// Slint component는 그 자체가 창이다.
impl<T: ComponentHandle> AsWindow for T {
    /// [`ComponentHandle::window`]와 같다.
    fn window(&self) -> &slint::Window {
        ComponentHandle::window(self)
    }
}

/// 편집 창 slot은 창과 대상 저장소를 함께 담는다.
impl AsWindow for EditorView {
    /// 편집 창의 [`slint::Window`].
    fn window(&self) -> &slint::Window {
        ComponentHandle::window(&self.window)
    }
}
