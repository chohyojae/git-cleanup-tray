// SPDX-License-Identifier: GPL-3.0-only

//! 수동 최신화와 주기 최신화 → Scan → Clean을 같은 worker 안에서 단계별로 처리한다.
//!
//! 단계는 순서대로 진행하고, 각 단계 안에서는 서로 다른 저장소를 제한된 수만큼 동시에 처리한다.

use std::path::Path;

use crate::app::state::SummaryOutcome;
use crate::app::tasks::{self, CleanReport, PendingCleanup, ProgressSink, ScanScope};
use crate::error::{CleanupError, Result};
use crate::git::in_process::InProcessGitRunner;
use crate::git::repository::RepositoryId;
use crate::git::sync;
use crate::jobs::cancellation::CancellationToken;
use crate::jobs::{batch, parallel, queue};
use crate::storage::repository_store::RepositoryConfig;
use crate::storage::settings_store::AppSettings;
use crate::storage::{repository_store, settings_store};

/// 최신화와 선택적 정리의 요약. 상세 plan은 worker 종료 시 버린다.
#[derive(Debug)]
pub struct MaintenanceReport {
    /// worker에서 읽은 등록 목록.
    pub repositories: Vec<RepositoryConfig>,
    /// 저장소별 최신화 결과.
    pub summaries: Vec<(RepositoryId, SummaryOutcome)>,
    /// fast-forward된 브랜치 수.
    pub updated: usize,
    /// 건너뛴 브랜치 수.
    pub skipped: usize,
    /// fetch/갱신/검사 실패 수.
    pub errors: usize,
    /// 표시할 상세 오류와 건너뛴 이유. 최대 50줄.
    pub problems: Vec<String>,
    /// 자동 정리 실행 결과. 수동 최신화에는 없다.
    pub cleanup: Option<CleanReport>,
    /// 취소되어 남은 작업을 중단했는지.
    pub cancelled: bool,
}

/// 등록된 remote를 최신화하고, 요청받으면 성공한 저장소만 새로 Scan해 정리한다.
///
/// 주기 실행은 사용자가 설정 화면에서 미리 동의했으므로 확인 dialog 없이 기존 executor를 쓴다.
/// fetch에 실패한 저장소는 오래된 ref로 정리하지 않는다. 설정이 손상되면 모든 작업을 중단한다.
/// 취소되면 새 저장소를 시작하지 않고 실행 중인 저장소가 끝나기를 기다린 뒤 다음 단계로 가지 않는다.
/// `progress`는 호출한 thread에서만 부른다.
///
/// # Errors
///
/// 설정이나 등록 목록을 읽을 수 없으면 해당 오류. 저장소별 오류는 결과에 담고 다음 것을 처리한다.
pub fn run(
    dir: &Path,
    scope: ScanScope,
    clean: bool,
    cancel: &CancellationToken,
    progress: ProgressSink<'_>,
) -> Result<MaintenanceReport> {
    let settings = settings_store::load(dir)?;
    let repositories = repository_store::load(dir)?;
    let runner = tasks::runner_for(settings.git_executable.as_deref());
    let mut report = MaintenanceReport {
        repositories: Vec::new(),
        summaries: Vec::new(),
        updated: 0,
        skipped: 0,
        errors: 0,
        problems: Vec::new(),
        cleanup: None,
        cancelled: false,
    };
    let targets: Vec<&RepositoryConfig> = repositories
        .iter()
        .filter(|repo| match scope {
            ScanScope::Enabled => repo.enabled,
            ScanScope::Only(id) => repo.id == id,
        })
        .collect();

    let updated = update_repositories(&runner, &targets, cancel, progress, &mut report);
    if clean && !cancel.is_cancelled() {
        let pending = scan_updated(&runner, &settings, &updated, cancel, progress, &mut report);
        if !cancel.is_cancelled() {
            // 저장소별 정리도 동시에 실행한다 ([`batch::clean_all`]).
            report.cleanup = Some(tasks::clean_in(Some(dir), &pending, cancel, progress));
        }
    }
    report.cancelled = cancel.is_cancelled();
    report.repositories = repositories;
    Ok(report)
}

/// 1단계: 서로 다른 저장소의 fetch와 fast-forward를 동시에 실행하고 결과를 `report`에 등록 순서로 담는다.
///
/// 시간 대부분이 remote 응답 대기라 겹칠수록 전체 시간이 준다. 한 저장소 안의 branch 갱신은
/// [`sync::synchronize`]가 순서대로 한다. 오류 없이 최신화한 저장소만 돌려주며, 이 저장소들만
/// 정리 전 Scan 대상이 된다.
fn update_repositories<'a>(
    runner: &InProcessGitRunner,
    targets: &[&'a RepositoryConfig],
    cancel: &CancellationToken,
    progress: ProgressSink<'_>,
    report: &mut MaintenanceReport,
) -> Vec<&'a RepositoryConfig> {
    let synced = parallel::map_bounded(
        targets,
        queue::repository_parallelism(),
        cancel,
        "update-repository",
        &mut |index| {
            progress(format!(
                "Fetching and updating {} of {}: {}",
                index + 1,
                targets.len(),
                targets[index].name
            ));
        },
        &|repo| {
            sync::synchronize(runner, &repo.path, &repo.remote_name, &|| {
                cancel.is_cancelled()
            })
        },
        &|_, error| Err(CleanupError::Io(error)),
    );
    let mut updated = Vec::with_capacity(synced.len());
    for (repo, outcome) in targets.iter().copied().zip(synced) {
        match outcome {
            Ok(result) => {
                report.updated += result.updated;
                report.skipped += result.skipped.len();
                report.errors += result.errors.len();
                report.summaries.push((
                    repo.id,
                    SummaryOutcome::Updated {
                        updated: result.updated,
                        skipped: result.skipped.len(),
                        errors: result.errors.len(),
                    },
                ));
                for message in &result.skipped {
                    problem(report, &repo.name, message);
                }
                for error in &result.errors {
                    problem(report, &repo.name, error);
                }
                if result.errors.is_empty() {
                    updated.push(repo);
                }
            }
            Err(error) => {
                report.errors += 1;
                problem(report, &repo.name, &error);
                report
                    .summaries
                    .push((repo.id, SummaryOutcome::Failed(error.to_string())));
            }
        }
    }
    updated
}

/// 2단계: 최신화한 ref로 `updated` 저장소를 새로 Scan해 정리 계획을 만든다.
///
/// Scan All과 같은 동시 검사 경로를 쓴다. 일부 항목이라도 검사하지 못한 저장소는 자동 삭제에서 뺀다.
fn scan_updated(
    runner: &InProcessGitRunner,
    settings: &AppSettings,
    updated: &[&RepositoryConfig],
    cancel: &CancellationToken,
    progress: ProgressSink<'_>,
    report: &mut MaintenanceReport,
) -> PendingCleanup {
    let mut pending = PendingCleanup {
        plans: Vec::new(),
        names: Vec::new(),
        git_executable: settings.git_executable.clone(),
    };
    let requests: Vec<_> = updated
        .iter()
        .map(|repo| tasks::scan_request(repo, settings))
        .collect();
    let scans = batch::scan_all(runner, &requests, cancel, &mut |p| {
        let name = updated.get(p.index).map_or("", |repo| repo.name.as_str());
        progress(format!(
            "Scanning after update {} of {}: {name}",
            p.index + 1,
            p.total
        ));
    });
    for (repo, scan) in updated.iter().zip(scans) {
        match scan.outcome {
            Ok(plan) => {
                report.errors += plan.errors.len();
                for error in &plan.errors {
                    problem(report, &repo.name, error);
                }
                if plan.errors.is_empty() {
                    pending.names.push((repo.id, repo.name.clone()));
                    pending.plans.push(plan);
                }
            }
            Err(error) => {
                report.errors += 1;
                problem(report, &repo.name, error);
            }
        }
    }
    pending
}

/// 많은 저장소에서도 상세 UI가 무한히 커지지 않도록 오류 문구 수를 제한한다.
fn problem(report: &mut MaintenanceReport, repository: &str, detail: impl std::fmt::Display) {
    if report.problems.len() < 50 {
        report.problems.push(format!("{repository}: {detail}"));
    }
}
