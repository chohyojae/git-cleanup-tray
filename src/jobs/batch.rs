// SPDX-License-Identifier: GPL-3.0-only

//! 여러 저장소의 동시 Scan과 동시 Clean All 본체 (PLAN 12장).
//!
//! 호출한 worker에서 결과를 기다리되 UI에는 의존하지 않는다. Scan과 Clean 모두 서로 다른 저장소를
//! 제한된 수만큼 동시에 처리하고, 한 저장소 안의 작업은 순서대로 한다. 취소 후에는 새 저장소를
//! 시작하지 않고 이미 시작한 저장소를 회수한다.

use std::collections::HashSet;

use crate::cleanup::CleanupPlan;
use crate::cleanup::candidate::SkipReason;
use crate::cleanup::executor::CleanupExecutor;
use crate::cleanup::planner::{CleanupPlanner, ScanRequest};
use crate::cleanup::result::CleanupResult;
use crate::error::{CleanupError, Result};
use crate::git::repository::RepositoryId;
use crate::git::runner::GitRunner;
use crate::jobs::cancellation::CancellationToken;
use crate::jobs::parallel;

/// 저장소 하나의 Scan 결과.
#[derive(Debug)]
pub struct RepositoryScan {
    /// 대상 저장소.
    pub repository_id: RepositoryId,
    /// 정리 계획, 또는 저장소 전체를 검사하지 못한 이유.
    pub outcome: Result<CleanupPlan>,
}

/// 저장소 하나를 시작할 때마다 progress callback에 넘기는 입력 위치와 대상.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// 지금 시작하는 저장소의 순번 (0부터).
    pub index: usize,
    /// 이번 작업에서 처리할 저장소 수.
    pub total: usize,
    /// 지금 시작하는 저장소.
    pub repository_id: RepositoryId,
}

/// `requests`를 기본 동시 실행 수만큼 나눠 Scan한다. 어떤 Git 상태도 바꾸지 않는다.
///
/// 완료 순서와 관계없이 결과는 입력 순서를 따른다. `on_progress`는 호출한 thread에서만 실행한다.
/// 취소되면 이미 시작한 저장소까지만 회수한다. 저장소 하나는 추가 thread 없이 호출자가 처리한다.
pub fn scan_all<R: GitRunner + ?Sized>(
    runner: &R,
    requests: &[ScanRequest<'_>],
    cancel: &CancellationToken,
    on_progress: &mut dyn FnMut(Progress),
) -> Vec<RepositoryScan> {
    scan_all_with_parallelism(
        runner,
        requests,
        cancel,
        super::queue::repository_parallelism(),
        on_progress,
    )
}

/// 지정한 동시 저장소 수로 같은 검사 경로를 실행해 성능 비교와 재현에 사용한다.
///
/// `parallelism`이 0이면 1로 처리한다. 완료 결과와 취소 처리는 [`scan_all`]과 같다.
pub fn scan_all_with_parallelism<R: GitRunner + ?Sized>(
    runner: &R,
    requests: &[ScanRequest<'_>],
    cancel: &CancellationToken,
    parallelism: usize,
    on_progress: &mut dyn FnMut(Progress),
) -> Vec<RepositoryScan> {
    parallel::map_bounded(
        requests,
        parallelism,
        cancel,
        "scan-repository",
        &mut |index| {
            on_progress(Progress {
                index,
                total: requests.len(),
                repository_id: requests[index].repository_id,
            });
        },
        &|request| scan_one(runner, request),
        &|request, error| RepositoryScan {
            repository_id: request.repository_id,
            outcome: Err(CleanupError::Io(error)),
        },
    )
}

/// 저장소 하나의 읽기 검사를 수행하고 결과를 요약해 기록한다.
fn scan_one<R: GitRunner + ?Sized>(runner: &R, request: &ScanRequest<'_>) -> RepositoryScan {
    let outcome = CleanupPlanner::new(runner).scan(request);
    match &outcome {
        Ok(plan) => log::info!(
            "scanned repository {}: {} removable, {} skipped, {} errors (base {} via {:?})",
            request.repository_id.0,
            plan.removable_count(),
            plan.skipped.len(),
            plan.errors.len(),
            plan.base.branch,
            plan.base.source,
        ),
        Err(error) => log::warn!(
            "scan failed for repository {}: {error}",
            request.repository_id.0
        ),
    }
    RepositoryScan {
        repository_id: request.repository_id,
        outcome,
    }
}

/// `plans`를 기본 동시 실행 수만큼 나눠 실행한다. 삭제할 항목이 없는 plan은 건너뛴다.
///
/// 빈 plan을 실행하지 않는 이유: executor는 마지막에 `git worktree prune`을 실행하는데, 정리할
/// 것이 없는 저장소에는 어떤 mutation도 하지 않는 편이 사용자가 예상하는 동작이다.
/// 서로 다른 저장소만 동시에 실행하고, 한 저장소 안의 삭제는 지금처럼 순서대로 한다. 결과는 완료
/// 순서와 관계없이 입력 순서를 따른다. 저장소를 시작할 때마다 호출한 thread에서 `on_progress`를
/// 부른다. 취소되면 새 저장소를 시작하지 않고, 이미 시작한 저장소가 끝난 뒤 그때까지의 결과를 돌려준다.
pub fn clean_all<R: GitRunner + ?Sized>(
    runner: &R,
    plans: &[CleanupPlan],
    cancel: &CancellationToken,
    on_progress: &mut dyn FnMut(Progress),
) -> Vec<CleanupResult> {
    clean_all_with_parallelism(
        runner,
        plans,
        cancel,
        super::queue::repository_parallelism(),
        on_progress,
    )
}

/// 지정한 동시 저장소 수로 [`clean_all`]과 같은 실행 경로를 탄다. 성능 비교와 재현에 사용한다.
///
/// 같은 저장소를 가리키는 plan이 둘 이상 들어오면 `parallelism`과 관계없이 순서대로 실행한다.
/// 한 저장소에서 삭제 명령 두 묶음이 겹치면 Git lock 충돌이 나고 재검증 결과도 믿을 수 없기 때문이다.
pub fn clean_all_with_parallelism<R: GitRunner + ?Sized>(
    runner: &R,
    plans: &[CleanupPlan],
    cancel: &CancellationToken,
    parallelism: usize,
    on_progress: &mut dyn FnMut(Progress),
) -> Vec<CleanupResult> {
    let runnable: Vec<&CleanupPlan> = plans
        .iter()
        .filter(|plan| plan.removable_count() > 0)
        .collect();
    let limit = if targets_are_distinct(&runnable) {
        parallelism
    } else {
        log::warn!("cleanup plans share a repository; running them one at a time");
        1
    };
    let total = runnable.len();
    let results = parallel::map_bounded(
        &runnable,
        limit,
        cancel,
        "clean-repository",
        &mut |index| {
            on_progress(Progress {
                index,
                total,
                repository_id: runnable[index].repository_id,
            });
        },
        &|plan| clean_one(runner, plan),
        &|plan, error| CleanupResult {
            repository_id: Some(plan.repository_id),
            errors: vec![CleanupError::Io(error)],
            ..CleanupResult::default()
        },
    );
    if results.len() < total {
        log::info!("cleanup cancelled");
    }
    results
}

/// 모든 plan이 서로 다른 저장소를 가리키면 `true`. id와 등록 경로를 모두 본다.
fn targets_are_distinct(plans: &[&CleanupPlan]) -> bool {
    let mut ids = HashSet::with_capacity(plans.len());
    let mut paths = HashSet::with_capacity(plans.len());
    plans
        .iter()
        .all(|plan| ids.insert(plan.repository_id) && paths.insert(plan.repository_path.as_path()))
}

/// 저장소 하나의 plan을 실행하고 결과를 요약해 기록한다.
fn clean_one<R: GitRunner + ?Sized>(runner: &R, plan: &CleanupPlan) -> CleanupResult {
    log::info!("cleanup start: repository {}", plan.repository_id.0);
    let result = CleanupExecutor::new(runner).execute(plan);
    log::info!(
        "cleanup end: repository {}: removed {} worktrees, {} branches, {} refs; {} skipped, {} errors",
        plan.repository_id.0,
        result.removed_worktrees.len(),
        result.removed_branches.len(),
        result.removed_custom_refs.len(),
        result.skipped.len(),
        result.errors.len(),
    );
    result
}

/// Clean All 확인 dialog에 보여줄 합계 (PLAN 13장).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlanTotals {
    /// 삭제할 항목이 하나 이상 있는 저장소 수.
    pub repositories: usize,
    /// 제거할 linked worktree 수.
    pub worktrees: usize,
    /// 삭제할 local branch 수. worktree를 제거한 뒤 함께 지울 branch도 센다. 실행 결과의
    /// `removed_branches`와 같은 기준이어야 확인 dialog와 결과 숫자를 비교할 수 있다.
    pub branches: usize,
    /// 삭제할 custom ref 수.
    pub custom_refs: usize,
    /// 안전 조건 때문에 제외한 항목 수. merge되지 않은 branch는 세지 않는다.
    pub skipped: usize,
    /// 검사하지 못한 저장소 수와 항목별 검사 오류 수의 합.
    pub errors: usize,
}

impl PlanTotals {
    /// Scan 결과를 합산한다.
    ///
    /// merge되지 않은 branch·ref는 "건너뛴 것"이 아니라 원래 정리 대상이 아니므로 `skipped`에서
    /// 뺀다. 넣으면 작업 중인 branch가 많은 저장소에서 숫자가 커져 실제로 확인할 항목이 묻힌다.
    #[must_use]
    pub fn from_scans(scans: &[RepositoryScan]) -> Self {
        let mut totals = Self::default();
        for scan in scans {
            match &scan.outcome {
                Ok(plan) => {
                    let one = Self::from_plan(plan);
                    totals.repositories += one.repositories;
                    totals.worktrees += one.worktrees;
                    totals.branches += one.branches;
                    totals.custom_refs += one.custom_refs;
                    totals.skipped += one.skipped;
                    totals.errors += one.errors;
                }
                Err(_) => totals.errors += 1,
            }
        }
        totals
    }

    /// plan 하나의 합계. 저장소 줄 요약도 이 기준으로 센다.
    #[must_use]
    pub fn from_plan(plan: &CleanupPlan) -> Self {
        Self {
            repositories: usize::from(plan.removable_count() > 0),
            worktrees: plan.worktrees.len(),
            branches: plan.branches.len() + plan.worktrees.len(),
            custom_refs: plan.custom_refs.len(),
            skipped: plan
                .skipped
                .iter()
                .filter(|item| item.reason != SkipReason::NotMerged)
                .count(),
            errors: plan.errors.len(),
        }
    }

    /// 삭제할 항목 총수. 확인 버튼의 "Clean N items"에 쓴다.
    #[must_use]
    pub fn removable(&self) -> usize {
        self.worktrees + self.branches + self.custom_refs
    }
}

/// Clean All 실행 결과 합계.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResultTotals {
    /// 실행한 저장소 수.
    pub repositories: usize,
    /// 제거한 linked worktree 수.
    pub worktrees: usize,
    /// 삭제한 local branch 수 (worktree와 함께 지운 branch 포함).
    pub branches: usize,
    /// 삭제한 custom ref 수.
    pub custom_refs: usize,
    /// 재검증 등으로 건너뛴 항목 수.
    pub skipped: usize,
    /// 실행 중 발생한 오류 수.
    pub errors: usize,
}

impl ResultTotals {
    /// 실행 결과를 합산한다.
    #[must_use]
    pub fn from_results(results: &[CleanupResult]) -> Self {
        let mut totals = Self {
            repositories: results.len(),
            ..Self::default()
        };
        for result in results {
            totals.worktrees += result.removed_worktrees.len();
            totals.branches += result.removed_branches.len();
            totals.custom_refs += result.removed_custom_refs.len();
            totals.skipped += result.skipped.len();
            totals.errors += result.errors.len();
        }
        totals
    }

    /// 실제로 삭제한 항목 총수.
    #[must_use]
    pub fn removed(&self) -> usize {
        self.worktrees + self.branches + self.custom_refs
    }
}
