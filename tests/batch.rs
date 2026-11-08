// SPDX-License-Identifier: GPL-3.0-only

//! Scan All 동시 처리와 Clean All 순차 처리 test (PLAN 12장).
//!
//! 저장소별 검사는 동시에 실행해도 결과 순서를 보존하며, 한 저장소의 실패가 다른 검사를
//! 막지 않고 취소 후에는 새 검사를 시작하지 않는지 확인한다.

mod common;

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use common::{Sandbox, default_namespaces, request};
use git_cleanup_tray::cleanup::CleanupPlan;
use git_cleanup_tray::cleanup::planner::ScanRequest;
use git_cleanup_tray::error::CleanupError;
use git_cleanup_tray::git::repository::RepositoryId;
use git_cleanup_tray::git::runner::{GitOutput, GitRunner, ProcessGitRunner};
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

/// 실행 중 취소가 들어오면 아직 시작하지 않은 저장소는 검사하지 않는다.
#[test]
fn cancellation_during_dispatch_keeps_unstarted_repositories_untouched() {
    let sandbox = Sandbox::new();
    let first = sandbox.clone_with_default("trunk", "취소 전 저장소");
    let second = sandbox.clone_with_default("trunk", "취소 후 저장소");
    let namespaces = default_namespaces();
    let cancel = CancellationToken::new();
    let mut started = Vec::new();
    let scans = batch::scan_all(
        &ProcessGitRunner::new(),
        &requests(&[&first, &second], &namespaces),
        &cancel,
        &mut |progress| {
            started.push(progress.repository_id);
            cancel.cancel();
        },
    );
    assert_eq!(started, [RepositoryId(1)]);
    assert_eq!(scans.len(), 1);
    assert!(scans[0].outcome.is_ok());
}

/// 두 저장소가 첫 Git 호출에 함께 도착해야 진행하도록 해 실제 동시 검사를 확인한다.
struct OverlapRunner {
    /// 첫 명령에 도착한 저장소 경로.
    entered: Mutex<HashSet<PathBuf>>,
    /// 다른 저장소의 첫 명령을 기다리는 조건 변수.
    ready: Condvar,
}

impl GitRunner for OverlapRunner {
    /// 첫 호출만 서로 기다리고 나머지는 실제 Git을 사용한다. 순차 구현은 제한 시간에 실패한다.
    fn run(
        &self,
        repository: &Path,
        args: &[&OsStr],
    ) -> git_cleanup_tray::error::Result<GitOutput> {
        let mut entered = self.entered.lock().expect("lock entered paths");
        if entered.insert(repository.to_path_buf()) {
            self.ready.notify_all();
            let (guard, timeout) = self
                .ready
                .wait_timeout_while(entered, Duration::from_secs(5), |paths| paths.len() < 2)
                .expect("wait for concurrent scan");
            entered = guard;
            if timeout.timed_out() {
                return Err(CleanupError::Io(std::io::Error::other(
                    "scans did not overlap",
                )));
            }
        }
        drop(entered);
        ProcessGitRunner::new().run(repository, args)
    }
}

/// 공백·한글 경로의 실제 저장소들을 동시에 검사하고 입력 순서와 plan을 보존한다.
#[test]
fn scan_all_runs_git_queries_for_distinct_repositories_concurrently() {
    let sandbox = Sandbox::new();
    let first = sandbox.clone_with_default("trunk", "첫 번째 동시 저장소");
    let second = sandbox.clone_with_default("기준", "두 번째 동시 저장소");
    sandbox.git(&first, &["branch", "병합된-작업"]);
    sandbox.git(&second, &["branch", "다른-병합된-작업"]);
    let namespaces = default_namespaces();
    let runner = OverlapRunner {
        entered: Mutex::new(HashSet::new()),
        ready: Condvar::new(),
    };
    let scans = batch::scan_all(
        &runner,
        &requests(&[&first, &second], &namespaces),
        &CancellationToken::new(),
        &mut |_| {},
    );
    assert_eq!(
        scans
            .iter()
            .map(|scan| scan.repository_id)
            .collect::<Vec<_>>(),
        [RepositoryId(1), RepositoryId(2)]
    );
    for scan in &scans {
        let plan = scan.outcome.as_ref().expect("concurrent scan succeeds");
        assert_eq!(plan.branches.len(), 1);
    }
    assert!(sandbox.ref_exists(&first, "refs/heads/병합된-작업"));
    assert!(sandbox.ref_exists(&second, "refs/heads/다른-병합된-작업"));
}
