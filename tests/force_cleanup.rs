// SPDX-License-Identifier: GPL-3.0-only

//! 사용자가 확인 창에서 직접 고른 강제 삭제 test (PLAN 8.5, 24장).
//!
//! dirty worktree와 merge되지 않은 branch는 고르지 않으면 지금까지처럼 남고, 고른 것만 지워지는지
//! 확인한다. Scan 뒤 상태가 바뀐 대상과 조작된 승인 항목은 지우지 않아야 한다.

mod common;

use std::path::Path;

use common::{Sandbox, Scenario, default_namespaces, path_str, request};
use git_cleanup_tray::app::selection;
use git_cleanup_tray::app::tasks::PendingCleanup;
use git_cleanup_tray::cleanup::CleanupPlan;
use git_cleanup_tray::cleanup::candidate::{ForceCandidate, ForceRisk, SkipReason, TargetKind};
use git_cleanup_tray::cleanup::executor::CleanupExecutor;
use git_cleanup_tray::cleanup::planner::CleanupPlanner;
use git_cleanup_tray::cleanup::result::CleanupResult;
use git_cleanup_tray::git::repository::RepositoryId;

/// `repo`를 Scan한다.
fn scan(repo: &Path, protected: &[String]) -> CleanupPlan {
    let namespaces = default_namespaces();
    CleanupPlanner::new(&common::test_runner())
        .scan(&request(repo, &namespaces, protected))
        .expect("scan")
}

/// plan을 실행한다.
fn execute(plan: &CleanupPlan) -> CleanupResult {
    CleanupExecutor::new(&common::test_runner()).execute(plan)
}

/// 강제 삭제 후보의 branch 이름 목록.
fn offered(plan: &CleanupPlan) -> Vec<&str> {
    plan.force_candidates
        .iter()
        .map(|candidate| candidate.branch.as_str())
        .collect()
}

/// 사용자가 확인 창에서 `branch` 줄만 추가로 체크한 것처럼 그 후보를 승인 목록으로 옮긴다.
fn approve(plan: &mut CleanupPlan, branch: &str) {
    let index = plan
        .force_candidates
        .iter()
        .position(|candidate| candidate.branch == branch)
        .unwrap_or_else(|| panic!("{branch} must be offered: {:?}", plan.force_candidates));
    let candidate = plan.force_candidates.remove(index);
    plan.approved_force.push(candidate);
}

/// 결과에서 `kind` 항목이 `StateChanged`로 넘어갔으면 `true`.
fn state_changed(result: &CleanupResult, kind: TargetKind) -> bool {
    result
        .skipped
        .iter()
        .any(|item| item.kind == kind && item.reason == SkipReason::StateChanged)
}

/// dirty worktree와 merge되지 않은 branch는 후보로만 올라가고, 고르지 않으면 그대로 남는다.
#[test]
fn force_candidates_are_offered_but_kept_unless_approved() {
    let sandbox = Sandbox::new();
    let s = sandbox.scenario();
    let plan = scan(&s.work, &[]);

    let mut names = offered(&plan);
    names.sort_unstable();
    assert_eq!(names, ["unmerged", "wt-dirty", "wt-staged", "wt-untracked"]);
    assert!(plan.approved_force.is_empty(), "{:?}", plan.approved_force);
    // 고르지 않은 후보는 삭제 수에 들어가지 않는다.
    assert_eq!(plan.removable_count(), 4);
    for candidate in &plan.force_candidates {
        let dirty = candidate.branch.starts_with("wt-");
        assert_eq!(candidate.worktree.is_some(), dirty, "{candidate:?}");
        let risk = if dirty {
            ForceRisk::DirtyWorktree
        } else {
            ForceRisk::NotMerged
        };
        assert_eq!(candidate.risk, risk, "{candidate:?}");
    }

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    for kept in [&s.wt_dirty, &s.wt_staged, &s.wt_untracked, &s.wt_locked] {
        assert!(kept.is_dir(), "{} must be kept", kept.display());
    }
    for kept in ["unmerged", "wt-dirty", "wt-staged", "wt-untracked"] {
        assert!(sandbox.ref_exists(&s.work, &format!("refs/heads/{kept}")));
    }
}

/// 보호 branch, lock된 worktree, main worktree에 checkout된 branch는 후보로도 올리지 않는다.
#[test]
fn protected_locked_and_main_checkouts_are_never_offered() {
    let sandbox = Sandbox::new();
    let s = sandbox.scenario();
    // merge되지 않은 branch를 lock된 worktree와 main worktree에 각각 둔다.
    sandbox.git(&s.work, &["branch", "잠긴-작업", "unmerged"]);
    let locked = sandbox.add_worktree(&s.work, "wt 잠긴 작업", "잠긴-작업");
    sandbox.git(&s.work, &["worktree", "lock", path_str(&locked)]);
    sandbox.git(&s.work, &["checkout", "-q", "-b", "current", "unmerged"]);

    let protected = [String::from("unmerged")];
    let plan = scan(&s.work, &protected);
    let mut names = offered(&plan);
    names.sort_unstable();
    assert_eq!(names, ["wt-dirty", "wt-staged", "wt-untracked"]);
}

/// 고른 dirty worktree는 변경과 함께 제거되고, 고르지 않은 dirty worktree는 남는다.
#[test]
fn approved_dirty_worktree_is_removed_with_its_branch() {
    let sandbox = Sandbox::new();
    let s = sandbox.scenario();
    let mut plan = scan(&s.work, &[]);
    approve(&mut plan, "wt-dirty");
    approve(&mut plan, "wt-untracked");

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(!s.wt_dirty.exists());
    assert!(!s.wt_untracked.exists());
    assert!(!sandbox.ref_exists(&s.work, "refs/heads/wt-dirty"));
    assert!(!sandbox.ref_exists(&s.work, "refs/heads/wt-untracked"));
    // 고르지 않은 것과 lock된 것은 그대로다.
    assert!(s.wt_staged.is_dir());
    assert!(sandbox.ref_exists(&s.work, "refs/heads/wt-staged"));
    assert!(s.wt_locked.is_dir());
    assert!(sandbox.ref_exists(&s.work, "refs/heads/unmerged"));
}

/// 고른 merge되지 않은 branch는 지워지고, `refs/remotes/*`와 다른 branch는 그대로다.
#[test]
fn approved_unmerged_branch_is_deleted() {
    let sandbox = Sandbox::new();
    let s = sandbox.scenario();
    let mut plan = scan(&s.work, &[]);
    approve(&mut plan, "unmerged");

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        result
            .removed_branches
            .iter()
            .any(|name| name == "unmerged")
    );
    assert!(!sandbox.ref_exists(&s.work, "refs/heads/unmerged"));
    assert!(sandbox.ref_exists(&s.work, "refs/heads/main"));
    assert!(sandbox.ref_exists(&s.work, "refs/remotes/origin/main"));
    // merge되지 않은 custom ref는 후보가 아니므로 남는다.
    assert!(sandbox.ref_exists(&s.work, "refs/remote/agent/unmerged"));
}

/// merge되지 않은 branch가 linked worktree에 있으면 worktree(변경 포함)와 branch를 함께 지운다.
#[test]
fn approved_unmerged_branch_with_worktree_is_removed() {
    let sandbox = Sandbox::new();
    let work = sandbox.clone_with_default("main", "저장소");
    sandbox.git(&work, &["branch", "agent/진행-중"]);
    let wt = sandbox.add_worktree(&work, "wt 진행 중", "agent/진행-중");
    sandbox.commit(&wt, "unmerged agent work");
    std::fs::write(wt.join("메모.txt"), "draft\n").expect("write untracked");

    let mut plan = scan(&work, &[]);
    assert_eq!(offered(&plan), ["agent/진행-중"]);
    assert_eq!(plan.force_candidates[0].worktree.as_deref(), Some(&*wt));
    assert_eq!(plan.force_candidates[0].risk, ForceRisk::NotMerged);
    approve(&mut plan, "agent/진행-중");

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.removed_worktrees, std::slice::from_ref(&wt));
    assert_eq!(result.removed_branches, ["agent/진행-중"]);
    assert!(!wt.exists());
    assert!(!sandbox.ref_exists(&work, "refs/heads/agent/진행-중"));
    assert!(work.join("history.txt").exists());
}

/// 승인 뒤 branch에 commit이 더 쌓이면 사용자가 확인한 것보다 많이 잃으므로 지우지 않는다.
#[test]
fn approved_branch_with_new_commit_is_kept() {
    let sandbox = Sandbox::new();
    let s = sandbox.scenario();
    let mut plan = scan(&s.work, &[]);
    approve(&mut plan, "unmerged");
    let side = sandbox.add_worktree(&s.work, "side checkout", "unmerged");
    sandbox.commit(&side, "more work after scan");
    sandbox.git(&s.work, &["worktree", "remove", path_str(&side)]);

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(state_changed(&result, TargetKind::Branch));
    assert!(sandbox.ref_exists(&s.work, "refs/heads/unmerged"));
}

/// 승인 뒤 dirty worktree에서 commit하면 branch가 움직였으므로 worktree와 branch를 모두 남긴다.
#[test]
fn approved_dirty_worktree_with_new_commit_is_kept() {
    let sandbox = Sandbox::new();
    let s = sandbox.scenario();
    let mut plan = scan(&s.work, &[]);
    approve(&mut plan, "wt-dirty");
    sandbox.commit(&s.wt_dirty, "saved after scan");

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(state_changed(&result, TargetKind::Worktree));
    assert!(s.wt_dirty.is_dir());
    assert!(sandbox.ref_exists(&s.work, "refs/heads/wt-dirty"));
}

/// 승인 뒤 worktree에 lock이 걸리면 강제로도 지우지 않는다.
#[test]
fn approved_worktree_locked_after_scan_is_kept() {
    let sandbox = Sandbox::new();
    let s = sandbox.scenario();
    let mut plan = scan(&s.work, &[]);
    approve(&mut plan, "wt-dirty");
    sandbox.git(&s.work, &["worktree", "lock", path_str(&s.wt_dirty)]);

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(state_changed(&result, TargetKind::Worktree));
    assert!(s.wt_dirty.is_dir());
    assert!(sandbox.ref_exists(&s.work, "refs/heads/wt-dirty"));
}

/// 승인 뒤 branch가 다른 worktree에 checkout되면 지우지 않는다.
#[test]
fn approved_branch_checked_out_after_scan_is_kept() {
    let sandbox = Sandbox::new();
    let s = sandbox.scenario();
    let mut plan = scan(&s.work, &[]);
    approve(&mut plan, "unmerged");
    let side = sandbox.add_worktree(&s.work, "side checkout", "unmerged");

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(state_changed(&result, TargetKind::Branch));
    assert!(side.is_dir());
    assert!(sandbox.ref_exists(&s.work, "refs/heads/unmerged"));
}

/// 조작된 승인 항목(base branch, main worktree, lock된 worktree, 맞지 않는 조합)은 지우지 않는다.
#[test]
fn forged_force_approvals_are_rejected() {
    let sandbox = Sandbox::new();
    let s = sandbox.scenario();
    let mut plan = scan(&s.work, &[]);
    let main = sandbox.oid(&s.work, "main");
    let merged = sandbox.oid(&s.work, "wt-locked");
    let forged = [
        // base branch 자체.
        (
            String::from("main"),
            main.clone(),
            None,
            ForceRisk::NotMerged,
        ),
        // main worktree.
        (
            String::from("main"),
            main,
            Some(s.work.clone()),
            ForceRisk::NotMerged,
        ),
        // lock된 worktree.
        (
            String::from("wt-locked"),
            merged.clone(),
            Some(s.wt_locked.clone()),
            ForceRisk::DirtyWorktree,
        ),
        // 다른 branch의 worktree를 가리키는 항목.
        (
            String::from("wt-staged"),
            merged.clone(),
            Some(s.wt_dirty.clone()),
            ForceRisk::DirtyWorktree,
        ),
        // worktree에 checkout된 branch를 worktree 없이 지우려는 항목.
        (
            String::from("wt-untracked"),
            merged,
            None,
            ForceRisk::DirtyWorktree,
        ),
    ];
    plan.approved_force
        .extend(
            forged
                .into_iter()
                .map(|(branch, commit, worktree, risk)| ForceCandidate {
                    branch,
                    commit,
                    worktree,
                    risk,
                }),
        );

    let result = execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    for kept in [
        &s.work,
        &s.wt_locked,
        &s.wt_dirty,
        &s.wt_staged,
        &s.wt_untracked,
    ] {
        assert!(kept.is_dir(), "{} must be kept", kept.display());
    }
    for kept in ["main", "wt-locked", "wt-dirty", "wt-staged", "wt-untracked"] {
        assert!(sandbox.ref_exists(&s.work, &format!("refs/heads/{kept}")));
    }
}

/// scenario를 Scan해 확인을 기다리는 계획으로 만든다.
fn pending(scenario: &Scenario) -> PendingCleanup {
    PendingCleanup {
        plans: vec![scan(&scenario.work, &[])],
        names: vec![(RepositoryId(1), String::from("작업 저장소"))],
        git_executable: None,
    }
}

/// 확인 창 목록은 안전한 항목만 체크해 두고, 숫자는 체크 상태를 따른다.
#[test]
fn selection_checks_only_safe_items_by_default() {
    let sandbox = Sandbox::new();
    let s = sandbox.scenario();
    let pending = pending(&s);

    let items = selection::items(&pending);
    assert_eq!(items.len(), selection::selectable_count(&pending));
    assert_eq!(items.len(), 8);
    assert!(items.iter().all(|item| item.checked != item.risky));
    assert_eq!(items.iter().filter(|item| item.risky).count(), 4);
    assert_eq!(selection::selectable_dirty_count(&pending), 3);
    assert!(
        items
            .iter()
            .all(|item| item.note.starts_with("작업 저장소"))
    );

    let checked: Vec<bool> = items.iter().map(|item| item.checked).collect();
    let counts = selection::counts(&pending, &checked);
    assert_eq!(counts.repositories, 1);
    assert_eq!(counts.worktrees, 1);
    // worktree와 함께 지우는 branch 1개 + worktree 없는 branch 2개.
    assert_eq!(counts.branches, 3);
    assert_eq!(counts.custom_refs, 1);
    assert_eq!(counts.forced, 0);

    // 줄 수가 맞지 않으면 아무것도 고르지 않은 것으로 본다.
    assert_eq!(
        selection::counts(&pending, &checked[1..]),
        selection::SelectionCounts::default()
    );
}

/// 체크를 바꾼 그대로 실행된다: 체크를 푼 안전한 항목은 남고, 체크한 dirty worktree는 지워진다.
#[test]
fn applied_selection_decides_what_is_cleaned() {
    let sandbox = Sandbox::new();
    let s = sandbox.scenario();
    let mut pending = pending(&s);
    let items = selection::items(&pending);
    let checked: Vec<bool> = items
        .iter()
        .map(|item| {
            if item.title == "Branch: release/1.0" {
                false
            } else if item.title == "Worktree + branch: wt-dirty" {
                true
            } else {
                item.checked
            }
        })
        .collect();
    let counts = selection::counts(&pending, &checked);
    assert_eq!(counts.forced, 1);
    assert_eq!(counts.worktrees, 2);
    assert_eq!(counts.branches, 3);

    // 줄 수가 다르면 계획을 바꾸지 않는다.
    assert!(!selection::apply(&mut pending, &checked[1..]));
    assert!(
        pending.plans[0].approved_force.is_empty(),
        "{:?}",
        pending.plans[0].approved_force
    );

    assert!(selection::apply(&mut pending, &checked));
    let plan = &pending.plans[0];
    assert!(
        plan.force_candidates.is_empty(),
        "{:?}",
        plan.force_candidates
    );
    assert_eq!(plan.approved_force.len(), 1);
    assert_eq!(plan.approved_force[0].branch, "wt-dirty");
    assert_eq!(plan.branches.len(), 1);

    let result = execute(plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(sandbox.ref_exists(&s.work, "refs/heads/release/1.0"));
    assert!(!s.wt_dirty.exists());
    assert!(!s.wt_clean.exists());
    assert!(!sandbox.ref_exists(&s.work, "refs/heads/기능/병합됨"));
    assert!(s.wt_staged.is_dir());
    assert!(s.wt_untracked.is_dir());
    assert!(sandbox.ref_exists(&s.work, "refs/heads/unmerged"));
}

/// 아무것도 체크하지 않으면 실행할 plan이 남지 않는다.
#[test]
fn empty_selection_leaves_nothing_to_run() {
    let sandbox = Sandbox::new();
    let s = sandbox.scenario();
    let mut pending = pending(&s);
    let none = vec![false; selection::selectable_count(&pending)];

    assert!(selection::apply(&mut pending, &none));
    assert!(pending.plans.is_empty());
    assert!(s.wt_clean.is_dir());
}
