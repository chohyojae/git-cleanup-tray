// SPDX-License-Identifier: GPL-3.0-only

//! Scan → Execute 전체 흐름 integration test (PLAN 26장, Phase 1 완료 조건).
//!
//! GUI 없이 실제 임시 저장소에서 cleanup이 PLAN 8장 규칙대로 동작하는지 확인한다.

mod common;

use std::path::{Path, PathBuf};

use common::{Sandbox, default_namespaces, request};
use git_cleanup_tray::cleanup::CleanupPlan;
use git_cleanup_tray::cleanup::candidate::{SkipReason, TargetKind};
use git_cleanup_tray::cleanup::executor::CleanupExecutor;
use git_cleanup_tray::cleanup::planner::CleanupPlanner;
use git_cleanup_tray::error::CleanupError;
use git_cleanup_tray::git::default_branch::{BaseBranchSource, BaseRef};
use git_cleanup_tray::git::runner::ProcessGitRunner;

/// symlink를 푼 경로. macOS 임시 경로(`/var` → `/private/var`) 차이를 없애고 비교한다.
fn canon(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).expect("canonicalize existing path")
}

/// plan에서 `kind`/`reason`에 해당하는 제외 항목 이름을 모은다.
fn skipped(plan: &CleanupPlan, kind: TargetKind, reason: SkipReason) -> Vec<&str> {
    plan.skipped
        .iter()
        .filter(|item| item.kind == kind && item.reason == reason)
        .map(|item| item.target.as_str())
        .collect()
}

/// PLAN 26장의 필수 케이스를 한 번에 Scan하고 Execute한다.
///
/// merge된 branch·clean worktree·`refs/remote/*`만 지워지고, 나머지는 사유와 함께 남아야 한다.
#[test]
fn scan_and_execute_cleans_only_safe_targets() {
    let sandbox = Sandbox::new();
    let s = sandbox.scenario();
    let runner = ProcessGitRunner::new();
    let namespaces = default_namespaces();
    let protected = vec![String::from("release/*")];

    let plan = CleanupPlanner::new(&runner)
        .scan(&request(&s.work, &namespaces, &protected))
        .expect("scan scenario");

    // base branch: clone이 만든 origin/HEAD에서 network 없이 판별하고 local branch를 쓴다.
    assert_eq!(plan.base.branch, "main");
    assert_eq!(plan.base.source, BaseBranchSource::LocalSymbolicRef);
    assert_eq!(
        plan.base.base_ref,
        BaseRef::Local(String::from("refs/heads/main"))
    );
    assert!(plan.errors.is_empty(), "scan errors: {:?}", plan.errors);

    // 후보
    let branches: Vec<&str> = plan.branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(branches, ["기능/병합됨"]);
    assert_eq!(plan.worktrees.len(), 1);
    assert_eq!(canon(&plan.worktrees[0].path), canon(&s.wt_clean));
    assert_eq!(plan.worktrees[0].branch, "wt-clean");
    let custom: Vec<&str> = plan
        .custom_refs
        .iter()
        .map(|r| r.full_ref.as_str())
        .collect();
    assert_eq!(custom, ["refs/remote/agent/merged"]);
    assert_eq!(plan.removable_count(), 3);

    // 제외 항목과 사유
    assert_eq!(
        skipped(&plan, TargetKind::Branch, SkipReason::Protected),
        ["release/1.0"]
    );
    assert_eq!(
        skipped(&plan, TargetKind::Branch, SkipReason::NotMerged),
        ["unmerged"]
    );
    assert_eq!(
        skipped(&plan, TargetKind::CustomRef, SkipReason::NotMerged),
        ["refs/remote/agent/unmerged"]
    );
    assert_eq!(
        skipped(&plan, TargetKind::Worktree, SkipReason::DirtyWorktree).len(),
        3
    );
    assert_eq!(
        skipped(&plan, TargetKind::Worktree, SkipReason::LockedWorktree).len(),
        1
    );
    // base branch와 refs/remotes/*, tag는 후보도 제외 항목도 아니다.
    assert!(plan.skipped.iter().all(|item| item.target != "main"
        && !item.target.starts_with("refs/remotes/")
        && !item.target.starts_with("refs/tags/")));

    let result = CleanupExecutor::new(&runner).execute(&plan);
    assert!(
        result.errors.is_empty(),
        "execute errors: {:?}",
        result.errors
    );
    assert!(
        result.skipped.is_empty(),
        "unexpected skips: {:?}",
        result.skipped
    );
    assert_eq!(result.removed_worktrees.len(), 1);
    let mut removed = result.removed_branches.clone();
    removed.sort();
    assert_eq!(removed, ["wt-clean", "기능/병합됨"]);
    assert_eq!(result.removed_custom_refs, ["refs/remote/agent/merged"]);

    // 실제 저장소 상태 확인
    let w = &s.work;
    assert!(
        !s.wt_clean.exists(),
        "clean worktree directory must be removed"
    );
    assert!(!sandbox.ref_exists(w, "refs/heads/wt-clean"));
    assert!(!sandbox.ref_exists(w, "refs/heads/기능/병합됨"));
    assert!(!sandbox.ref_exists(w, "refs/remote/agent/merged"));
    for kept in [
        "refs/heads/main",
        "refs/heads/unmerged",
        "refs/heads/release/1.0",
        "refs/heads/wt-dirty",
        "refs/heads/wt-staged",
        "refs/heads/wt-untracked",
        "refs/heads/wt-locked",
        "refs/remote/agent/unmerged",
        "refs/remotes/origin/main",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/old-feature",
        "refs/tags/v1",
    ] {
        assert!(sandbox.ref_exists(w, kept), "{kept} must be kept");
    }
    // dirty/staged/untracked worktree의 작업물은 그대로 남아야 한다.
    assert_eq!(
        std::fs::read_to_string(s.wt_dirty.join("history.txt")).expect("read dirty file"),
        "changed\n"
    );
    assert!(s.wt_staged.join("staged.txt").exists());
    assert!(s.wt_untracked.join("untracked.txt").exists());
    assert!(s.wt_locked.exists());
}

/// linked worktree 경로로 등록해도 같은 저장소로 동작하고, 등록 경로 자체는 지우지 않는다.
#[test]
fn registered_linked_worktree_is_never_removed() {
    let sandbox = Sandbox::new();
    let s = sandbox.scenario();
    let runner = ProcessGitRunner::new();
    let namespaces = default_namespaces();

    let plan = CleanupPlanner::new(&runner)
        .scan(&request(&s.wt_clean, &namespaces, &[]))
        .expect("scan from linked worktree");
    assert!(plan.worktrees.is_empty(), "{:?}", plan.worktrees);
    assert_eq!(
        skipped(&plan, TargetKind::Worktree, SkipReason::CurrentWorktree).len(),
        1
    );
}

/// main worktree에 checkout된 merge된 branch는 지우지 않는다.
#[test]
fn branch_checked_out_in_main_worktree_is_skipped() {
    let sandbox = Sandbox::new();
    let work = sandbox.clone_with_default("main", "repo");
    sandbox.git(&work, &["checkout", "-q", "-b", "merged-but-checked-out"]);
    sandbox.git(&work, &["checkout", "-q", "main"]);
    sandbox.commit(&work, "advance main");
    sandbox.git(&work, &["checkout", "-q", "merged-but-checked-out"]);

    let runner = ProcessGitRunner::new();
    let plan = CleanupPlanner::new(&runner)
        .scan(&request(&work, &default_namespaces(), &[]))
        .expect("scan");
    assert!(plan.branches.is_empty(), "{:?}", plan.branches);
    assert_eq!(
        skipped(
            &plan,
            TargetKind::Branch,
            SkipReason::CheckedOutInMainWorktree
        ),
        ["merged-but-checked-out"]
    );
}

/// `refs/remote/` 밖의 custom namespace 설정은 무시한다. guard가 어차피 삭제를 막는 영역이다.
#[test]
fn invalid_custom_namespaces_are_ignored() {
    let sandbox = Sandbox::new();
    let s = sandbox.scenario();
    let runner = ProcessGitRunner::new();
    let namespaces = vec![String::from("refs/remotes/"), String::from("refs/tags/")];

    let plan = CleanupPlanner::new(&runner)
        .scan(&request(&s.work, &namespaces, &[]))
        .expect("scan");
    assert!(plan.custom_refs.is_empty(), "{:?}", plan.custom_refs);
    assert!(
        plan.skipped
            .iter()
            .all(|item| item.kind != TargetKind::CustomRef)
    );
}

/// Git 저장소가 아니거나 경로가 없으면 category가 있는 오류를 돌려준다.
#[test]
fn reports_repository_errors() {
    let sandbox = Sandbox::new();
    let runner = ProcessGitRunner::new();
    let planner = CleanupPlanner::new(&runner);
    let namespaces = default_namespaces();

    let plain = sandbox.root.join("그냥 폴더");
    std::fs::create_dir(&plain).expect("create plain dir");
    assert!(matches!(
        planner.scan(&request(&plain, &namespaces, &[])),
        Err(CleanupError::NotARepository(_))
    ));

    let missing = sandbox.root.join("없는 경로");
    assert!(matches!(
        planner.scan(&request(&missing, &namespaces, &[])),
        Err(CleanupError::RepositoryNotAccessible(_))
    ));
}
