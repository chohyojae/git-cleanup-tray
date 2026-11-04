// SPDX-License-Identifier: GPL-3.0-only

//! 수동 최신화와 주기 최신화 → Scan → Clean을 같은 worker 안에서 순차 처리한다.

use std::path::Path;

use crate::app::state::SummaryOutcome;
use crate::app::tasks::{self, CleanReport, PendingCleanup, ProgressSink, ScanScope};
use crate::cleanup::planner::CleanupPlanner;
use crate::error::Result;
use crate::git::repository::RepositoryId;
use crate::git::sync;
use crate::jobs::cancellation::CancellationToken;
use crate::storage::repository_store::RepositoryConfig;
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
    let planner = CleanupPlanner::new(&runner);
    let mut pending = PendingCleanup {
        plans: Vec::new(),
        names: Vec::new(),
        git_executable: settings.git_executable.clone(),
    };
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
    for repo in repositories.iter().filter(|repo| match scope {
        ScanScope::Enabled => repo.enabled,
        ScanScope::Only(id) => repo.id == id,
    }) {
        if cancel.is_cancelled() {
            break;
        }
        progress(format!("Fetching and updating: {}", repo.name));
        match sync::synchronize(&runner, &repo.path, &repo.remote_name, &|| {
            cancel.is_cancelled()
        }) {
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
                    problem(&mut report, &repo.name, message);
                }
                for error in &result.errors {
                    problem(&mut report, &repo.name, error);
                }
                if !clean || !result.errors.is_empty() || cancel.is_cancelled() {
                    continue;
                }
                progress(format!("Scanning after update: {}", repo.name));
                match planner.scan(&tasks::scan_request(repo, &settings)) {
                    Ok(plan) => {
                        report.errors += plan.errors.len();
                        for error in &plan.errors {
                            problem(&mut report, &repo.name, error);
                        }
                        // 일부 항목을 검사하지 못한 저장소도 자동 삭제에서 제외한다.
                        if plan.errors.is_empty() {
                            pending.names.push((repo.id, repo.name.clone()));
                            pending.plans.push(plan);
                        }
                    }
                    Err(error) => {
                        report.errors += 1;
                        problem(&mut report, &repo.name, error);
                    }
                }
            }
            Err(error) => {
                report.errors += 1;
                problem(&mut report, &repo.name, &error);
                report
                    .summaries
                    .push((repo.id, SummaryOutcome::Failed(error.to_string())));
            }
        }
    }
    if clean && !cancel.is_cancelled() {
        report.cleanup = Some(tasks::clean_in(Some(dir), &pending, cancel, progress));
    }
    report.cancelled = cancel.is_cancelled();
    report.repositories = repositories;
    Ok(report)
}

/// 많은 저장소에서도 상세 UI가 무한히 커지지 않도록 오류 문구 수를 제한한다.
fn problem(report: &mut MaintenanceReport, repository: &str, detail: impl std::fmt::Display) {
    if report.problems.len() < 50 {
        report.problems.push(format!("{repository}: {detail}"));
    }
}
