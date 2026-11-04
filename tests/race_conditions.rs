// SPDX-License-Identifier: GPL-3.0-only

//! Scan 이후 Execute 이전 상태 변경 test (PLAN 9, 26장 Race-condition Tests).
//!
//! executor가 plan을 그대로 믿지 않고, 바뀐 대상을 지우지 않고 `StateChanged`로 넘기는지 확인한다.

mod common;

use common::{Sandbox, Scenario, default_namespaces, path_str, request};
use git_cleanup_tray::cleanup::CleanupPlan;
use git_cleanup_tray::cleanup::candidate::{SkipReason, TargetKind};
use git_cleanup_tray::cleanup::executor::CleanupExecutor;
use git_cleanup_tray::cleanup::planner::CleanupPlanner;
use git_cleanup_tray::cleanup::result::CleanupResult;
use git_cleanup_tray::git::runner::ProcessGitRunner;

/// scenario를 만들고 Scan한 plan을 돌려준다.
fn scanned(sandbox: &Sandbox) -> (Scenario, CleanupPlan) {
    let scenario = sandbox.scenario();
    let runner = ProcessGitRunner::new();
    let namespaces = default_namespaces();
    let plan = CleanupPlanner::new(&runner)
        .scan(&request(&scenario.work, &namespaces, &[]))
        .expect("scan");
    assert_eq!(plan.removable_count(), 4, "precondition: {plan:?}");
    (scenario, plan)
}

/// plan을 실행한다.
fn execute(plan: &CleanupPlan) -> CleanupResult {
    CleanupExecutor::new(&ProcessGitRunner::new()).execute(plan)
}

/// 결과에서 `kind` 항목이 `StateChanged`로 넘어갔으면 `true`.
fn state_changed(result: &CleanupResult, kind: TargetKind) -> bool {
    result
        .skipped
        .iter()
        .any(|item| item.kind == kind && item.reason == SkipReason::StateChanged)
}

/// Scan 뒤 merge된 branch에 새 commit이 생기면 그 branch를 지우지 않는다.
#[test]
fn branch_with_new_commit_is_kept() {
    let sandbox = Sandbox::new();
    let (s, plan) = scanned(&sandbox);
    let side = sandbox.add_worktree(&s.work, "side checkout", "기능/병합됨");
    sandbox.commit(&side, "new work after scan");
    sandbox.git(&s.work, &["worktree", "remove", path_str(&side)]);

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(state_changed(&result, TargetKind::Branch));
    assert!(sandbox.ref_exists(&s.work, "refs/heads/기능/병합됨"));
    // 바뀌지 않은 대상은 그대로 정리된다.
    assert_eq!(result.removed_custom_refs, ["refs/remote/agent/merged"]);
}

/// Scan 뒤 clean worktree에 파일이 바뀌면 worktree와 branch를 모두 남긴다.
#[test]
fn worktree_modified_after_scan_is_kept() {
    let sandbox = Sandbox::new();
    let (s, plan) = scanned(&sandbox);
    std::fs::write(s.wt_clean.join("history.txt"), "edited after scan\n").expect("edit");

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(state_changed(&result, TargetKind::Worktree));
    assert!(
        result.removed_worktrees.is_empty(),
        "{:?}",
        result.removed_worktrees
    );
    assert!(s.wt_clean.join("history.txt").exists());
    assert!(sandbox.ref_exists(&s.work, "refs/heads/wt-clean"));
}

/// Scan 뒤 worktree가 lock되면 제거하지 않는다.
#[test]
fn worktree_locked_after_scan_is_kept() {
    let sandbox = Sandbox::new();
    let (s, plan) = scanned(&sandbox);
    sandbox.git(&s.work, &["worktree", "lock", path_str(&s.wt_clean)]);

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(state_changed(&result, TargetKind::Worktree));
    assert!(s.wt_clean.exists());
    assert!(sandbox.ref_exists(&s.work, "refs/heads/wt-clean"));
}

/// Scan 뒤 base branch가 움직이면 아무것도 지우지 않는다.
#[test]
fn moved_base_branch_cancels_everything() {
    let sandbox = Sandbox::new();
    let (s, plan) = scanned(&sandbox);
    sandbox.commit(&s.work, "base moved after scan");

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        result.removed_worktrees.is_empty(),
        "{:?}",
        result.removed_worktrees
    );
    assert!(
        result.removed_branches.is_empty(),
        "{:?}",
        result.removed_branches
    );
    assert!(
        result.removed_custom_refs.is_empty(),
        "{:?}",
        result.removed_custom_refs
    );
    assert_eq!(result.skipped.len(), plan.removable_count());
    assert!(
        result
            .skipped
            .iter()
            .all(|item| item.reason == SkipReason::StateChanged)
    );
    assert!(s.wt_clean.exists());
}

/// Scan 뒤 `origin/HEAD`가 다른 branch를 가리키게 되면 아무것도 지우지 않는다.
#[test]
fn changed_remote_head_cancels_everything() {
    let sandbox = Sandbox::new();
    let (s, plan) = scanned(&sandbox);
    sandbox.git(
        &s.work,
        &["update-ref", "refs/remotes/origin/develop", "HEAD"],
    );
    sandbox.git(&s.work, &["remote", "set-head", "origin", "develop"]);

    let result = execute(&plan);
    assert!(
        result.removed_branches.is_empty(),
        "{:?}",
        result.removed_branches
    );
    assert_eq!(result.skipped.len(), plan.removable_count());
}

/// Scan 뒤 custom ref가 다른 commit으로 바뀌면, 여전히 merge된 commit이어도 지우지 않는다.
#[test]
fn moved_custom_ref_is_kept() {
    let sandbox = Sandbox::new();
    let (s, plan) = scanned(&sandbox);
    // merged ref는 첫 commit에 있다. base(main)로 옮기면 merge 상태는 같지만 commit이 다르다.
    sandbox.git(&s.work, &["update-ref", "refs/remote/agent/merged", "main"]);

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(state_changed(&result, TargetKind::CustomRef));
    assert!(sandbox.ref_exists(&s.work, "refs/remote/agent/merged"));
}

/// Scan 뒤 사용자가 branch를 직접 지웠으면 오류 없이 `StateChanged`로 넘긴다.
#[test]
fn branch_deleted_after_scan_is_reported_as_changed() {
    let sandbox = Sandbox::new();
    let (s, plan) = scanned(&sandbox);
    sandbox.git(&s.work, &["branch", "-d", "기능/병합됨"]);

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(state_changed(&result, TargetKind::Branch));
}

/// Scan 뒤 worktree가 이미 제거되었으면 `StateChanged`로 넘기고, 남은 branch도 지우지 않는다.
#[test]
fn worktree_removed_after_scan_is_reported_as_changed() {
    let sandbox = Sandbox::new();
    let (s, plan) = scanned(&sandbox);
    sandbox.git(&s.work, &["worktree", "remove", path_str(&s.wt_clean)]);

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(state_changed(&result, TargetKind::Worktree));
    // worktree 후보의 branch는 worktree 제거에 성공했을 때만 지운다.
    assert!(sandbox.ref_exists(&s.work, "refs/heads/wt-clean"));
}

/// Scan 뒤 worktree가 다른 branch로 바뀌면 (worktree-branch mapping 변경) 제거하지 않는다.
#[test]
fn worktree_switched_to_other_branch_is_kept() {
    let sandbox = Sandbox::new();
    let (s, plan) = scanned(&sandbox);
    sandbox.git(&s.wt_clean, &["switch", "-q", "-c", "새-작업"]);

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(state_changed(&result, TargetKind::Worktree));
    assert!(s.wt_clean.exists());
    assert!(sandbox.ref_exists(&s.work, "refs/heads/새-작업"));
}

/// Scan 뒤 clean worktree에 untracked 파일이나 staged 변경이 생기면 제거하지 않는다.
#[test]
fn worktree_with_new_untracked_or_staged_file_is_kept() {
    for staged in [false, true] {
        let sandbox = Sandbox::new();
        let (s, plan) = scanned(&sandbox);
        std::fs::write(s.wt_clean.join("새 파일.txt"), "after scan").expect("write");
        if staged {
            sandbox.git(&s.wt_clean, &["add", "새 파일.txt"]);
        }

        let result = execute(&plan);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(
            state_changed(&result, TargetKind::Worktree),
            "staged={staged}"
        );
        assert!(s.wt_clean.join("새 파일.txt").exists(), "staged={staged}");
    }
}

/// Scan 때 worktree가 없던 branch가 Execute 전에 새 worktree에 checkout되면 지우지 않는다.
#[test]
fn branch_checked_out_after_scan_is_kept() {
    let sandbox = Sandbox::new();
    let (s, plan) = scanned(&sandbox);
    let new_wt = sandbox.add_worktree(&s.work, "새 worktree", "기능/병합됨");

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(state_changed(&result, TargetKind::Branch));
    assert!(new_wt.exists());
    assert!(sandbox.ref_exists(&s.work, "refs/heads/기능/병합됨"));
}

/// Scan 뒤 custom ref가 이미 지워졌으면 오류 없이 `StateChanged`로 넘긴다.
#[test]
fn custom_ref_deleted_after_scan_is_reported_as_changed() {
    let sandbox = Sandbox::new();
    let (s, plan) = scanned(&sandbox);
    sandbox.git(&s.work, &["update-ref", "-d", "refs/remote/agent/merged"]);

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(state_changed(&result, TargetKind::CustomRef));
}

/// Scan 뒤 custom ref가 같은 commit의 branch를 가리키는 symbolic ref로 바뀌어도 그 branch를 지우지 않는다.
///
/// `wt-dirty`는 Scan 때 custom ref와 같은 commit이라 old value 비교만으로는 걸러지지 않는다.
/// `--no-deref`가 symbolic ref 자신만 지우게 하는지 확인한다.
#[test]
fn custom_ref_replaced_by_symbolic_ref_keeps_target() {
    let sandbox = Sandbox::new();
    let (s, plan) = scanned(&sandbox);
    sandbox.git(&s.work, &["update-ref", "-d", "refs/remote/agent/merged"]);
    sandbox.git(
        &s.work,
        &[
            "symbolic-ref",
            "refs/remote/agent/merged",
            "refs/heads/wt-dirty",
        ],
    );

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(sandbox.ref_exists(&s.work, "refs/heads/wt-dirty"));
}
