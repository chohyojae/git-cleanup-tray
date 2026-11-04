// SPDX-License-Identifier: GPL-3.0-only

//! 여러 저장소를 순차 처리하는 Scan All / Clean All 본체 (PLAN 12장).
//!
//! GUI 없이 test할 수 있도록 thread나 UI와 무관한 동기 함수로 둔다. worker thread에서의 실행은
//! [`super::spawn`]이 맡는다. 한 저장소가 실패해도 다음 저장소를 계속 처리하고, 취소 요청은
//! 저장소 사이에서만 확인한다 (MVP에서는 병렬 처리하지 않는다).

use crate::cleanup::CleanupPlan;
use crate::cleanup::candidate::SkipReason;
use crate::cleanup::executor::CleanupExecutor;
use crate::cleanup::planner::{CleanupPlanner, ScanRequest};
use crate::cleanup::result::CleanupResult;
use crate::error::Result;
use crate::git::repository::RepositoryId;
use crate::git::runner::GitRunner;
use crate::jobs::cancellation::CancellationToken;

/// 저장소 하나의 Scan 결과.
#[derive(Debug)]
pub struct RepositoryScan {
    /// 대상 저장소.
    pub repository_id: RepositoryId,
    /// 정리 계획, 또는 저장소 전체를 검사하지 못한 이유.
    pub outcome: Result<CleanupPlan>,
}

/// 순차 처리 진행 상황. 저장소 하나를 시작할 때마다 progress callback에 넘긴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// 지금 시작하는 저장소의 순번 (0부터).
    pub index: usize,
    /// 이번 작업에서 처리할 저장소 수.
    pub total: usize,
    /// 지금 시작하는 저장소.
    pub repository_id: RepositoryId,
}

/// `requests`를 순서대로 Scan한다. 어떤 Git 상태도 바꾸지 않는다.
///
/// 결과는 입력 순서를 따른다. 저장소를 시작할 때마다 `on_progress`를 호출하고, 취소되면 그
/// 시점까지의 결과만 돌려준다.
pub fn scan_all<R: GitRunner + ?Sized>(
    runner: &R,
    requests: &[ScanRequest<'_>],
    cancel: &CancellationToken,
    on_progress: &mut dyn FnMut(Progress),
) -> Vec<RepositoryScan> {
    let planner = CleanupPlanner::new(runner);
    let mut scans = Vec::with_capacity(requests.len());
    for (index, request) in requests.iter().enumerate() {
        if cancel.is_cancelled() {
            log::info!("scan cancelled");
            break;
        }
        on_progress(Progress {
            index,
            total: requests.len(),
            repository_id: request.repository_id,
        });
        let outcome = planner.scan(request);
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
        scans.push(RepositoryScan {
            repository_id: request.repository_id,
            outcome,
        });
    }
    scans
}

/// `plans`를 순서대로 실행한다. 삭제할 항목이 없는 plan은 건너뛴다.
///
/// 빈 plan을 실행하지 않는 이유: executor는 마지막에 `git worktree prune`을 실행하는데, 정리할
/// 것이 없는 저장소에는 어떤 mutation도 하지 않는 편이 사용자가 예상하는 동작이다.
/// 저장소를 시작할 때마다 `on_progress`를 호출한다. 취소되면 진행 중인 저장소까지만 끝내고 그
/// 시점까지의 결과를 돌려준다.
pub fn clean_all<R: GitRunner + ?Sized>(
    runner: &R,
    plans: &[CleanupPlan],
    cancel: &CancellationToken,
    on_progress: &mut dyn FnMut(Progress),
) -> Vec<CleanupResult> {
    let executor = CleanupExecutor::new(runner);
    let total = plans
        .iter()
        .filter(|plan| plan.removable_count() > 0)
        .count();
    let mut results = Vec::with_capacity(total);
    for (index, plan) in plans
        .iter()
        .filter(|plan| plan.removable_count() > 0)
        .enumerate()
    {
        if cancel.is_cancelled() {
            log::info!("cleanup cancelled");
            break;
        }
        on_progress(Progress {
            index,
            total,
            repository_id: plan.repository_id,
        });
        log::info!("cleanup start: repository {}", plan.repository_id.0);
        let result = executor.execute(plan);
        log::info!(
            "cleanup end: repository {}: removed {} worktrees, {} branches, {} refs; {} skipped, {} errors",
            plan.repository_id.0,
            result.removed_worktrees.len(),
            result.removed_branches.len(),
            result.removed_custom_refs.len(),
            result.skipped.len(),
            result.errors.len(),
        );
        results.push(result);
    }
    results
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
