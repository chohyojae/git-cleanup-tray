// SPDX-License-Identifier: GPL-3.0-only

//! worker thread에서 실행하는 작업 본체.
//!
//! 이 module의 함수는 모두 UI thread 밖에서 돌고 Slint 타입을 쓰지 않는다. 결과는 plain Rust
//! 값으로 돌려주고, Slint model 변환은 UI thread(`app` module)에서 한다. 설정 파일은 작업마다 새로
//! 읽는다 (AGENTS 4.1).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::app::state::SummaryOutcome;
use crate::app::text;
use crate::cleanup::CleanupPlan;
use crate::cleanup::candidate::{SkipReason, SkippedItem};
use crate::cleanup::planner::ScanRequest;
use crate::error::{CleanupError, Result};
use crate::git::default_branch::BaseBranchQuery;
use crate::git::repository::{self, RepositoryId};
use crate::git::runner::ProcessGitRunner;
use crate::jobs::batch::{self, PlanTotals, ResultTotals};
use crate::jobs::cancellation::CancellationToken;
use crate::platform;
use crate::storage;
use crate::storage::history_store::{self, HistoryEntry};
use crate::storage::repository_store::{self, RepositoryConfig};
use crate::storage::settings_store::{self, AppSettings};

/// 확인 dialog 등에 보여줄 상세 줄 수 상한. 저장소가 많아도 model이 커지지 않게 한다.
const MAX_PROBLEM_LINES: usize = 50;

/// 진행 상황 문구를 받는 callback. worker thread에서 호출된다.
pub type ProgressSink<'a> = &'a mut dyn FnMut(String);

/// 현재 시각 (UNIX epoch 초). 시계가 1970년 이전이면 0.
#[must_use]
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// 앱 data directory. OS가 알려주지 않으면 [`CleanupError::Io`] (`NotFound`).
pub(super) fn data_dir() -> Result<PathBuf> {
    storage::app_data_dir().ok_or_else(|| {
        CleanupError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "app data directory is unknown",
        ))
    })
}

/// 설정에 맞는 git runner를 만든다.
pub(super) fn runner_for(git_executable: Option<&Path>) -> ProcessGitRunner {
    git_executable.map_or_else(ProcessGitRunner::new, ProcessGitRunner::with_executable)
}

/// 앱 시작 시 읽는 상태.
#[derive(Debug)]
pub struct LoadedState {
    /// `config.json`. 깨졌으면 오류.
    pub settings: Result<AppSettings>,
    /// `repositories.json`. 깨졌으면 오류.
    pub repositories: Result<Vec<RepositoryConfig>>,
    /// 저장소별 마지막 cleanup 시각 (`history.json`). 이력이 깨졌으면 비어 있다.
    pub last_cleanup: HashMap<RepositoryId, u64>,
}

/// worker thread: 설정, 저장소 목록, 이력을 읽는다. Git은 실행하지 않는다.
#[must_use]
pub fn load_state() -> LoadedState {
    let Ok(dir) = data_dir() else {
        log::warn!("app data directory is unknown; starting with defaults");
        return LoadedState {
            settings: Ok(AppSettings::default()),
            repositories: Ok(Vec::new()),
            last_cleanup: HashMap::new(),
        };
    };
    let mut last_cleanup = HashMap::new();
    match history_store::load(&dir) {
        Ok(history) => {
            for entry in history {
                let at = last_cleanup.entry(entry.repository_id).or_insert(0);
                *at = (*at).max(entry.finished_at);
            }
        }
        Err(error) => log::warn!("ignoring unreadable history: {error}"),
    }
    LoadedState {
        settings: settings_store::load(&dir),
        repositories: repository_store::load(&dir),
        last_cleanup,
    }
}

/// Scan 대상 범위.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanScope {
    /// enabled 저장소 전체 (Scan All / Clean All).
    Enabled,
    /// 저장소 하나. 사용자가 직접 고른 것이므로 disabled여도 검사한다.
    Only(RepositoryId),
}

/// 확인 dialog가 떠 있는 동안 들고 있는 정리 계획.
///
/// 확인 뒤 다시 Scan하지 않고 이 plan을 executor에 넘긴다. executor가 대상마다 삭제 직전에
/// 상태를 재검증하므로, 확인을 기다리는 사이에 바뀐 대상은 `StateChanged`로 넘어간다.
#[derive(Debug)]
pub struct PendingCleanup {
    /// 삭제할 항목이 있는 저장소의 plan.
    pub plans: Vec<CleanupPlan>,
    /// 결과 문구와 이력에 쓸 저장소 이름.
    pub names: Vec<(RepositoryId, String)>,
    /// Scan 때 쓴 git 실행 파일 설정. 실행도 같은 git으로 한다.
    pub git_executable: Option<PathBuf>,
}

/// 상세 화면 항목 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailLine {
    /// 주 이름.
    pub title: String,
    /// 보조 정보. 없으면 빈 문자열.
    pub subtitle: String,
    /// 상태 설명.
    pub note: String,
}

/// 상세 화면 내용 (PLAN 14장).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailReport {
    /// 저장소 이름.
    pub heading: String,
    /// base branch와 경로.
    pub subheading: String,
    /// (종류 이름, 항목) 목록. 비어 있는 종류는 넣지 않는다.
    pub sections: Vec<(&'static str, Vec<DetailLine>)>,
}

/// 저장소 하나의 Scan 요약.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanSummary {
    /// 대상 저장소.
    pub repository_id: RepositoryId,
    /// 판별한 base branch. 검사에 실패했으면 `None`.
    pub base: Option<String>,
    /// 결과.
    pub outcome: SummaryOutcome,
}

/// Scan worker 결과.
#[derive(Debug)]
pub struct ScanReport {
    /// Scan 시작 시 읽은 저장소 목록. 화면 목록을 이것으로 맞춘다.
    pub repositories: Vec<RepositoryConfig>,
    /// 실제로 Scan한 저장소 수.
    pub scanned: usize,
    /// 확인 dialog용 합계.
    pub totals: PlanTotals,
    /// 오류와 건너뛴 항목 상세 ("저장소: 내용").
    pub problems: Vec<String>,
    /// 저장소별 요약.
    pub summaries: Vec<ScanSummary>,
    /// 상세 화면 내용. 요청했고 Scan에 성공했을 때만 있다.
    pub detail: Option<DetailReport>,
    /// 확인 이후 실행할 계획.
    pub pending: PendingCleanup,
    /// Clean All 전에 확인 dialog를 띄울지 (`config.json`).
    pub confirm_clean_all: bool,
    /// Scan이 끝난 시각.
    pub finished_at: u64,
}

/// worker thread: 설정과 저장소 목록을 읽고 `scope`의 저장소들을 제한된 수만큼 동시에 Scan한다.
///
/// # Errors
///
/// 설정 파일을 읽거나 해석하지 못하면 해당 오류. 이때는 어떤 저장소도 검사하지 않는다. 손상된
/// 설정을 기본값으로 대신하면 보호 branch pattern이 빠진 채 정리될 수 있기 때문이다.
pub fn scan(
    scope: ScanScope,
    want_detail: bool,
    cancel: &CancellationToken,
    progress: ProgressSink<'_>,
) -> Result<ScanReport> {
    let dir = data_dir()?;
    let settings = settings_store::load(&dir)?;
    let repositories = repository_store::load(&dir)?;
    let targets: Vec<&RepositoryConfig> = repositories
        .iter()
        .filter(|repo| match scope {
            ScanScope::Enabled => repo.enabled,
            ScanScope::Only(id) => repo.id == id,
        })
        .collect();
    let runner = runner_for(settings.git_executable.as_deref());
    let requests: Vec<ScanRequest<'_>> = targets
        .iter()
        .map(|repo| scan_request(repo, &settings))
        .collect();
    let scans = batch::scan_all(&runner, &requests, cancel, &mut |p| {
        let name = targets.get(p.index).map_or("", |repo| repo.name.as_str());
        progress(format!("Scanning {} of {}: {name}", p.index + 1, p.total));
    });
    let totals = PlanTotals::from_scans(&scans);

    let mut problems = Vec::new();
    let mut summaries = Vec::with_capacity(scans.len());
    let mut plans = Vec::new();
    let mut detail = None;
    for (repo, scan) in targets.iter().zip(scans) {
        match scan.outcome {
            Ok(plan) => {
                for error in &plan.errors {
                    push_problem(&mut problems, &repo.name, error);
                }
                push_skipped(&mut problems, &repo.name, &plan.skipped);
                summaries.push(plan_summary(&plan));
                if want_detail {
                    detail = Some(detail_report(&repo.name, &plan));
                }
                if plan.removable_count() > 0 {
                    plans.push(plan);
                }
            }
            Err(error) => {
                push_problem(&mut problems, &repo.name, &error);
                summaries.push(ScanSummary {
                    repository_id: repo.id,
                    base: None,
                    outcome: SummaryOutcome::Failed(error.to_string()),
                });
            }
        }
    }
    let names = targets
        .iter()
        .map(|repo| (repo.id, repo.name.clone()))
        .collect();
    let scanned = summaries.len();
    Ok(ScanReport {
        repositories,
        scanned,
        totals,
        problems,
        summaries,
        detail,
        pending: PendingCleanup {
            plans,
            names,
            git_executable: settings.git_executable,
        },
        confirm_clean_all: settings.confirm_clean_all,
        finished_at: now_secs(),
    })
}

/// 저장소 설정과 앱 설정으로 Scan 요청을 만든다.
pub(super) fn scan_request<'a>(
    repo: &'a RepositoryConfig,
    settings: &'a AppSettings,
) -> ScanRequest<'a> {
    ScanRequest {
        repository_id: repo.id,
        path: &repo.path,
        base: BaseBranchQuery {
            remote: &repo.remote_name,
            override_branch: repo.base_branch_override.as_deref(),
            allow_remote_query: settings.allow_remote_query,
        },
        custom_ref_namespaces: &repo.custom_ref_namespaces,
        protected_branch_patterns: &settings.protected_branch_patterns,
    }
}

/// plan 하나를 저장소 줄 요약으로 바꾼다. 개수 기준은 [`PlanTotals`]와 같다.
fn plan_summary(plan: &CleanupPlan) -> ScanSummary {
    let totals = PlanTotals::from_plan(plan);
    ScanSummary {
        repository_id: plan.repository_id,
        base: Some(plan.base.branch.clone()),
        outcome: SummaryOutcome::Scanned {
            removable: totals.removable(),
            warnings: totals.skipped + totals.errors,
        },
    }
}

/// plan을 상세 화면 내용으로 바꾼다 (PLAN 14장 문구).
fn detail_report(name: &str, plan: &CleanupPlan) -> DetailReport {
    let base = &plan.base.branch;
    let line = |title: String, subtitle: String, note: String| DetailLine {
        title,
        subtitle,
        note,
    };
    let mut sections = Vec::new();
    let mut push = |title, lines: Vec<DetailLine>| {
        if !lines.is_empty() {
            sections.push((title, lines));
        }
    };
    push(
        "Worktrees",
        plan.worktrees
            .iter()
            .map(|wt| {
                line(
                    wt.branch.clone(),
                    wt.path.display().to_string(),
                    String::from("Clean. The worktree and its branch will be removed."),
                )
            })
            .collect(),
    );
    push(
        "Local Branches",
        plan.branches
            .iter()
            .map(|branch| {
                line(
                    branch.name.clone(),
                    String::new(),
                    format!("Already merged into {base}"),
                )
            })
            .collect(),
    );
    push(
        "Custom Refs",
        plan.custom_refs
            .iter()
            .map(|r| {
                line(
                    r.full_ref.clone(),
                    String::new(),
                    format!("Already contained in {base}"),
                )
            })
            .collect(),
    );
    push(
        "Skipped",
        plan.skipped
            .iter()
            .filter(|item| item.reason != SkipReason::NotMerged)
            .map(|item| {
                line(
                    item.target.clone(),
                    String::new(),
                    text::skip_reason(item.reason).to_owned(),
                )
            })
            .collect(),
    );
    push(
        "Errors",
        plan.errors
            .iter()
            .map(|error| line(error.to_string(), String::new(), String::new()))
            .collect(),
    );
    DetailReport {
        heading: name.to_owned(),
        subheading: format!(
            "Base: {} ({:?}) · {}",
            plan.base.base_ref.full_ref(),
            plan.base.source,
            plan.repository_path.display()
        ),
        sections,
    }
}

/// Clean worker 결과.
#[derive(Debug)]
pub struct CleanReport {
    /// 실행 결과 합계.
    pub totals: ResultTotals,
    /// 오류와 건너뛴 항목 상세.
    pub problems: Vec<String>,
    /// 저장소별 결과 요약.
    pub summaries: Vec<(RepositoryId, SummaryOutcome)>,
    /// 사용자가 취소해 일부 저장소를 실행하지 않았으면 `true`.
    pub cancelled: bool,
    /// 실행이 끝난 시각.
    pub finished_at: u64,
}

/// worker thread: 확인된 계획을 저장소 순서대로 실행하고, 결과를 요약해 이력에 남긴다.
///
/// 이력 저장 실패는 정리 결과에 영향을 주지 않으므로 로그만 남긴다.
#[must_use]
pub fn clean(
    pending: &PendingCleanup,
    cancel: &CancellationToken,
    progress: ProgressSink<'_>,
) -> CleanReport {
    clean_in(data_dir().as_deref().ok(), pending, cancel, progress)
}

/// 지정한 저장 폴더에 실행 결과를 남긴다. test는 실제 앱 설정을 건드리지 않는다.
pub(super) fn clean_in(
    dir: Option<&Path>,
    pending: &PendingCleanup,
    cancel: &CancellationToken,
    progress: ProgressSink<'_>,
) -> CleanReport {
    let name_of = |id: Option<RepositoryId>| {
        pending
            .names
            .iter()
            .find(|(candidate, _)| Some(*candidate) == id)
            .map_or("?", |(_, name)| name.as_str())
    };
    let runner = runner_for(pending.git_executable.as_deref());
    let results = batch::clean_all(&runner, &pending.plans, cancel, &mut |p| {
        let name = name_of(Some(p.repository_id));
        progress(format!("Cleaning {} of {}: {name}", p.index + 1, p.total));
    });
    let finished_at = now_secs();

    let mut problems = Vec::new();
    let mut summaries = Vec::with_capacity(results.len());
    let mut history = Vec::with_capacity(results.len());
    for result in &results {
        let name = name_of(result.repository_id);
        for error in &result.errors {
            push_problem(&mut problems, name, error);
        }
        push_skipped(&mut problems, name, &result.skipped);
        let Some(id) = result.repository_id else {
            continue;
        };
        let totals = ResultTotals::from_results(std::slice::from_ref(result));
        summaries.push((
            id,
            SummaryOutcome::Cleaned {
                removed: totals.removed(),
                skipped: totals.skipped,
                errors: totals.errors,
            },
        ));
        history.push(HistoryEntry {
            finished_at,
            repository_id: id,
            repository_name: name.to_owned(),
            removed_worktrees: totals.worktrees,
            removed_branches: totals.branches,
            removed_custom_refs: totals.custom_refs,
            skipped: totals.skipped,
            errors: totals.errors,
        });
    }
    if let Some(dir) = dir
        && let Err(error) = history_store::append(dir, &history)
    {
        log::error!("failed to save cleanup history: {error}");
    }
    let planned = pending
        .plans
        .iter()
        .filter(|plan| plan.removable_count() > 0)
        .count();
    CleanReport {
        totals: ResultTotals::from_results(&results),
        problems,
        summaries,
        cancelled: results.len() < planned,
        finished_at,
    }
}

/// 등록 저장소 목록 변경 요청.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepositoryChange {
    /// 폴더를 저장소로 등록한다.
    Add(PathBuf),
    /// 같은 id의 설정을 바꾼다 (경로와 common dir은 유지).
    Update(RepositoryConfig),
    /// 등록을 지운다. 저장소 자체는 건드리지 않는다.
    Remove(RepositoryId),
    /// Scan All / Clean All 대상 여부를 바꾼다.
    SetEnabled(RepositoryId, bool),
}

/// 저장소 목록 변경 실패 이유.
#[derive(Debug, thiserror::Error)]
pub enum ChangeError {
    /// 같은 저장소(common Git directory 기준)가 이미 등록되어 있다.
    #[error("this repository is already registered as \"{0}\"")]
    AlreadyRegistered(String),
    /// 대상 저장소가 목록에 없다 (다른 곳에서 파일을 고친 경우).
    #[error("the repository is no longer registered")]
    NotFound,
    /// 저장소 확인이나 파일 저장 실패.
    #[error(transparent)]
    Failed(#[from] CleanupError),
}

/// worker thread: `repositories.json`을 읽어 `change`를 적용하고 저장한 뒤 새 목록을 돌려준다.
///
/// 저장소를 추가할 때는 `git rev-parse`로 Git 저장소인지 확인하고, 다른 worktree 경로로 같은
/// 저장소를 두 번 등록하지 않도록 common Git directory로 비교한다 (PLAN 6장).
///
/// # Errors
///
/// [`ChangeError`] 참고.
pub fn apply_change(change: RepositoryChange) -> Result<Vec<RepositoryConfig>, ChangeError> {
    let dir = data_dir()?;
    let mut repositories = repository_store::load(&dir)?;
    match change {
        RepositoryChange::Add(path) => {
            let settings = settings_store::load(&dir)?;
            let config = new_repository(&repositories, &settings, &path)?;
            log::info!("registering repository {}", config.id.0);
            repositories.push(config);
        }
        RepositoryChange::Update(config) => {
            let existing = repositories
                .iter_mut()
                .find(|repo| repo.id == config.id)
                .ok_or(ChangeError::NotFound)?;
            *existing = RepositoryConfig {
                path: existing.path.clone(),
                common_dir: existing.common_dir.clone(),
                ..config
            };
        }
        RepositoryChange::Remove(id) => {
            let before = repositories.len();
            repositories.retain(|repo| repo.id != id);
            if repositories.len() == before {
                return Err(ChangeError::NotFound);
            }
        }
        RepositoryChange::SetEnabled(id, enabled) => {
            repositories
                .iter_mut()
                .find(|repo| repo.id == id)
                .ok_or(ChangeError::NotFound)?
                .enabled = enabled;
        }
    }
    repository_store::save(&dir, &repositories)?;
    Ok(repositories)
}

/// `path`를 확인해 새 저장소 설정을 만든다. 경로는 작업 tree 최상위로 맞춘다.
///
/// # Errors
///
/// Git 저장소가 아니면 [`ChangeError::Failed`], 이미 등록된 저장소면
/// [`ChangeError::AlreadyRegistered`].
pub fn new_repository(
    existing: &[RepositoryConfig],
    settings: &AppSettings,
    path: &Path,
) -> Result<RepositoryConfig, ChangeError> {
    let runner = runner_for(settings.git_executable.as_deref());
    let identity = repository::identify(&runner, path)?;
    let common_dir = std::fs::canonicalize(&identity.common_dir).unwrap_or(identity.common_dir);
    if let Some(duplicate) = existing.iter().find(|repo| {
        repo.common_dir == common_dir
            || std::fs::canonicalize(&repo.common_dir).is_ok_and(|dir| dir == common_dir)
    }) {
        return Err(ChangeError::AlreadyRegistered(duplicate.name.clone()));
    }
    let name = identity.toplevel.file_name().map_or_else(
        || identity.toplevel.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    Ok(RepositoryConfig {
        id: repository_store::next_id(existing),
        name,
        path: identity.toplevel,
        common_dir,
        enabled: true,
        remote_name: settings.default_remote.clone(),
        base_branch_override: None,
        custom_ref_namespaces: vec![String::from(repository_store::DEFAULT_CUSTOM_REF_NAMESPACE)],
    })
}

/// worker thread: 설정을 저장하고, login 자동 시작 값이 바뀌었으면 OS에 반영한다.
///
/// 자동 시작을 먼저 바꾸고 성공했을 때만 설정 파일을 저장한다. 순서가 반대면 OS 등록에 실패했는데
/// 설정에는 켜져 있는 것으로 남는다.
///
/// # Errors
///
/// 자동 시작 등록이나 파일 저장에 실패하면 해당 오류.
pub fn save_settings(settings: &AppSettings, apply_autostart: bool) -> Result<()> {
    if apply_autostart {
        platform::autostart::set_enabled(settings.start_at_login)?;
        log::info!("start at login set to {}", settings.start_at_login);
    }
    settings_store::save(&data_dir()?, settings)
}

/// 상세 목록에 "저장소: 내용" 한 줄을 넣는다. [`MAX_PROBLEM_LINES`]를 넘으면 버린다.
fn push_problem(problems: &mut Vec<String>, repository: &str, detail: impl std::fmt::Display) {
    if problems.len() < MAX_PROBLEM_LINES {
        problems.push(format!("{repository}: {detail}"));
    }
}

/// 건너뛴 항목을 상세 목록에 넣는다. merge되지 않은 branch는 정리 대상이 아니므로 넣지 않는다.
fn push_skipped(problems: &mut Vec<String>, repository: &str, skipped: &[SkippedItem]) {
    for item in skipped
        .iter()
        .filter(|item| item.reason != SkipReason::NotMerged)
    {
        push_problem(
            problems,
            repository,
            format_args!(
                "{} skipped ({})",
                item.target,
                text::skip_reason(item.reason)
            ),
        );
    }
}
