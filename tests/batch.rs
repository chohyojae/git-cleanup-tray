// SPDX-License-Identifier: GPL-3.0-only

//! Scan All / Clean All 순차 처리 test (PLAN 12장).
//!
//! 여러 저장소를 순서대로 처리하고, 한 저장소가 실패해도 다음 저장소로 넘어가며, 취소되면
//! 다음 저장소를 시작하지 않는지 확인한다.

mod common;

use std::path::{Path, PathBuf};

use common::{Sandbox, default_namespaces, request};
use git_cleanup_tray::cleanup::CleanupPlan;
use git_cleanup_tray::cleanup::planner::ScanRequest;
use git_cleanup_tray::error::CleanupError;
use git_cleanup_tray::git::repository::RepositoryId;
use git_cleanup_tray::git::runner::ProcessGitRunner;
use git_cleanup_tray::jobs::batch::{self, PlanTotals, Progress, RepositoryScan, ResultTotals};
use git_cleanup_tray::jobs::cancellation::CancellationToken;

/// test에 쓰는 저장소 세 개와 존재하지 않는 경로 하나.
struct Repositories {
    /// PLAN 26장 scenario 저장소. 삭제 후보 4개(worktree 1, branch 2, custom ref 1).
    scenario: PathBuf,
    /// default branch가 `trunk`이고 merge된 branch `done` 하나만 있는 저장소.
    trunk: PathBuf,
    /// 정리할 것이 없는 저장소.
    empty: PathBuf,
    /// 존재하지 않는 경로.
    missing: PathBuf,
}

/// [`Repositories`]를 만든다.
fn repositories(sandbox: &Sandbox) -> Repositories {
    let scenario = sandbox.scenario().work;
    let trunk = sandbox.clone_with_default("trunk", "두번째 저장소");
    sandbox.git(&trunk, &["branch", "done"]);
    let empty = sandbox.clone_with_default("main", "빈 저장소");
    Repositories {
        scenario,
        trunk,
        empty,
        missing: sandbox.root.join("사라진 저장소"),
    }
}

/// `paths`를 1부터 차례로 id를 붙인 Scan 요청으로 바꾼다.
fn requests<'a>(paths: &[&'a Path], namespaces: &'a [String]) -> Vec<ScanRequest<'a>> {
    (1..)
        .zip(paths)
        .map(|(id, path)| ScanRequest {
            repository_id: RepositoryId(id),
            ..request(path, namespaces, &[])
        })
        .collect()
}

/// 성공한 Scan의 plan만 꺼낸다.
fn plans(scans: Vec<RepositoryScan>) -> Vec<CleanupPlan> {
    scans
        .into_iter()
        .map(|scan| scan.outcome.expect("scan succeeds"))
        .collect()
}

/// 중간 저장소가 실패해도 나머지를 입력 순서대로 Scan하고, 합계에 오류로 센다.
#[test]
fn scan_all_continues_after_failed_repository() {
    let sandbox = Sandbox::new();
    let repos = repositories(&sandbox);
    let namespaces = default_namespaces();
    let runner = ProcessGitRunner::new();

    let mut progress = Vec::new();
    let scans = batch::scan_all(
        &runner,
        &requests(
            &[&repos.scenario, &repos.missing, &repos.trunk],
            &namespaces,
        ),
        &CancellationToken::new(),
        &mut |p| progress.push(p),
    );

    let ids: Vec<u64> = scans.iter().map(|scan| scan.repository_id.0).collect();
    assert_eq!(ids, [1, 2, 3]);
    let expected: Vec<_> = (1..=3_u64)
        .zip(0..)
        .map(|(id, index)| Progress {
            index,
            total: 3,
            repository_id: RepositoryId(id),
        })
        .collect();
    assert_eq!(progress, expected);
    assert!(
        matches!(
            &scans[1].outcome,
            Err(CleanupError::RepositoryNotAccessible(path)) if *path == repos.missing
        ),
        "{:?}",
        scans[1].outcome
    );
    assert_eq!(
        PlanTotals::from_scans(&scans),
        PlanTotals {
            repositories: 2,
            worktrees: 1,
            // scenario: `기능/병합됨`, `release/1.0`, worktree의 `wt-clean` / trunk: `done`
            branches: 4,
            custom_refs: 1,
            // dirty, staged, untracked, locked worktree. merge 안 된 `unmerged`는 세지 않는다.
            skipped: 4,
            errors: 1,
        }
    );
}

/// 각 저장소를 순서대로 실행하고, 지울 것이 없는 저장소는 실행하지 않는다.
#[test]
fn clean_all_runs_each_repository_with_removable_items() {
    let sandbox = Sandbox::new();
    let repos = repositories(&sandbox);
    let namespaces = default_namespaces();
    let runner = ProcessGitRunner::new();
    let scans = batch::scan_all(
        &runner,
        &requests(&[&repos.scenario, &repos.empty, &repos.trunk], &namespaces),
        &CancellationToken::new(),
        &mut |_| {},
    );
    let planned = PlanTotals::from_scans(&scans);
    let plans = plans(scans);

    let mut progress = Vec::new();
    let results = batch::clean_all(&runner, &plans, &CancellationToken::new(), &mut |p| {
        progress.push((p.index, p.total, p.repository_id.0));
    });

    // 빈 저장소는 진행률 total에도 들어가지 않는다.
    assert_eq!(progress, [(0, 2, 1), (1, 2, 3)]);
    let ids: Vec<_> = results
        .iter()
        .map(|result| result.repository_id.map(|id| id.0))
        .collect();
    assert_eq!(ids, [Some(1), Some(3)], "empty repository must not run");
    let totals = ResultTotals::from_results(&results);
    assert_eq!(totals.errors, 0, "{results:?}");
    // 확인 dialog의 "Clean N items"와 결과의 "Removed N items"가 같은 기준이어야 한다.
    assert_eq!(totals.removed(), planned.removable());
    assert!(!sandbox.ref_exists(&repos.scenario, "refs/heads/기능/병합됨"));
    assert!(!sandbox.ref_exists(&repos.scenario, "refs/remote/agent/merged"));
    assert!(!sandbox.ref_exists(&repos.trunk, "refs/heads/done"));
}

/// 취소된 뒤에는 다음 저장소를 시작하지 않는다.
#[test]
fn cancelled_batch_does_not_start_next_repository() {
    let sandbox = Sandbox::new();
    let repos = repositories(&sandbox);
    let namespaces = default_namespaces();
    let runner = ProcessGitRunner::new();
    let plans = plans(batch::scan_all(
        &runner,
        &requests(&[&repos.scenario, &repos.trunk], &namespaces),
        &CancellationToken::new(),
        &mut |_| {},
    ));

    let cancel = CancellationToken::new();
    cancel.cancel();
    let scans = batch::scan_all(
        &runner,
        &requests(&[&repos.trunk], &namespaces),
        &cancel,
        &mut |_| {},
    );
    let results = batch::clean_all(&runner, &plans, &cancel, &mut |_| {});

    assert!(scans.is_empty(), "{scans:?}");
    assert!(results.is_empty(), "{results:?}");
    assert!(sandbox.ref_exists(&repos.scenario, "refs/heads/기능/병합됨"));
    assert!(sandbox.ref_exists(&repos.trunk, "refs/heads/done"));
}
