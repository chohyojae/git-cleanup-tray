// SPDX-License-Identifier: GPL-3.0-only

//! 안전 조건 integration test (PLAN 30장 Phase 2, 24장 안전 불변조건).
//!
//! dirty/locked worktree, 보호 ref, symbolic ref, 특수문자 이름처럼 "지우면 안 되는 것"과
//! "지울 때 다른 것을 건드리면 안 되는 것"을 실제 저장소에서 확인한다.

mod common;

use std::path::Path;

use common::{Sandbox, default_namespaces, path_str, request};
use git_cleanup_tray::cleanup::CleanupPlan;
use git_cleanup_tray::cleanup::candidate::{
    BranchCandidate, CustomRefCandidate, SkipReason, TargetKind, WorktreeCandidate,
};
use git_cleanup_tray::cleanup::executor::CleanupExecutor;
use git_cleanup_tray::cleanup::planner::CleanupPlanner;
use git_cleanup_tray::git::runner::ProcessGitRunner;

/// 기본 namespace, 보호 pattern으로 Scan한다.
fn scan(repo: &Path, protected: &[String]) -> CleanupPlan {
    let namespaces = default_namespaces();
    CleanupPlanner::new(&ProcessGitRunner::new())
        .scan(&request(repo, &namespaces, protected))
        .expect("scan")
}

/// plan에서 `kind`/`reason`에 해당하는 제외 항목 수.
fn count(plan: &CleanupPlan, kind: TargetKind, reason: SkipReason) -> usize {
    plan.skipped
        .iter()
        .filter(|item| item.kind == kind && item.reason == reason)
        .count()
}

/// tracked 파일 삭제, untracked directory, staged rename도 dirty로 보고 건드리지 않는다.
#[test]
fn dirty_variants_are_never_removed() {
    let sandbox = Sandbox::new();
    let work = sandbox.clone_with_default("main", "repo");
    for branch in ["deleted", "untracked-dir", "renamed"] {
        sandbox.git(&work, &["branch", branch]);
    }
    sandbox.commit(&work, "advance main");
    let deleted = sandbox.add_worktree(&work, "wt deleted", "deleted");
    let untracked = sandbox.add_worktree(&work, "wt untracked dir", "untracked-dir");
    let renamed = sandbox.add_worktree(&work, "wt renamed", "renamed");

    std::fs::remove_file(deleted.join("history.txt")).expect("delete tracked file");
    std::fs::create_dir_all(untracked.join("새 폴더/깊은 곳")).expect("create dirs");
    std::fs::write(untracked.join("새 폴더/깊은 곳/memo.txt"), "memo").expect("write");
    sandbox.git(&renamed, &["mv", "history.txt", "renamed.txt"]);

    let plan = scan(&work, &[]);
    assert!(plan.worktrees.is_empty(), "{:?}", plan.worktrees);
    assert_eq!(
        count(&plan, TargetKind::Worktree, SkipReason::DirtyWorktree),
        3
    );

    let result = CleanupExecutor::new(&ProcessGitRunner::new()).execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(untracked.join("새 폴더/깊은 곳/memo.txt").exists());
    assert!(renamed.join("renamed.txt").exists());
    assert!(deleted.exists());
}

/// 사유 없이 lock된 worktree도 lock으로 본다.
#[test]
fn worktree_locked_without_reason_is_kept() {
    let sandbox = Sandbox::new();
    let work = sandbox.clone_with_default("main", "repo");
    sandbox.git(&work, &["branch", "locked"]);
    sandbox.commit(&work, "advance main");
    let wt = sandbox.add_worktree(&work, "wt locked", "locked");
    sandbox.git(&work, &["worktree", "lock", path_str(&wt)]);

    let plan = scan(&work, &[]);
    assert_eq!(
        count(&plan, TargetKind::Worktree, SkipReason::LockedWorktree),
        1
    );
    let result = CleanupExecutor::new(&ProcessGitRunner::new()).execute(&plan);
    assert!(
        result.removed_worktrees.is_empty(),
        "{:?}",
        result.removed_worktrees
    );
    assert!(wt.exists());
    assert!(sandbox.ref_exists(&work, "refs/heads/locked"));
}

/// 경로가 사라진 worktree는 이번에는 건너뛰고, 마지막 `worktree prune` 뒤 다음 Scan에서
/// 일반 branch로 정리된다. 폴더를 직접 지우는 일은 앱이 하지 않는다.
#[test]
fn missing_worktree_path_is_pruned_then_cleaned_next_time() {
    let sandbox = Sandbox::new();
    let work = sandbox.clone_with_default("main", "repo");
    sandbox.git(&work, &["branch", "gone"]);
    sandbox.commit(&work, "advance main");
    let wt = sandbox.add_worktree(&work, "wt gone", "gone");
    std::fs::remove_dir_all(&wt).expect("user deletes worktree folder");

    let runner = ProcessGitRunner::new();
    let plan = scan(&work, &[]);
    assert_eq!(
        count(&plan, TargetKind::Worktree, SkipReason::MissingWorktreePath),
        1
    );
    let first = CleanupExecutor::new(&runner).execute(&plan);
    assert!(first.errors.is_empty(), "{:?}", first.errors);
    assert!(sandbox.ref_exists(&work, "refs/heads/gone"));

    let plan = scan(&work, &[]);
    let names: Vec<&str> = plan.branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["gone"]);
    let second = CleanupExecutor::new(&runner).execute(&plan);
    assert!(second.errors.is_empty(), "{:?}", second.errors);
    assert!(!sandbox.ref_exists(&work, "refs/heads/gone"));
}

/// detached HEAD인 linked worktree는 branch가 없으므로 정리 대상이 아니다.
#[test]
fn detached_worktree_is_untouched() {
    let sandbox = Sandbox::new();
    let work = sandbox.clone_with_default("main", "repo");
    let wt = sandbox.root.join("wt detached");
    sandbox.git(
        &work,
        &["worktree", "add", "-q", "--detach", path_str(&wt), "HEAD"],
    );
    sandbox.commit(&work, "advance main");

    let plan = scan(&work, &[]);
    assert!(plan.worktrees.is_empty(), "{:?}", plan.worktrees);
    let result = CleanupExecutor::new(&ProcessGitRunner::new()).execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(wt.exists());
}

/// 보호 pattern은 worktree에 연결된 branch에도 적용된다. clean worktree여도 제거하지 않는다.
#[test]
fn protected_branch_keeps_its_worktree() {
    let sandbox = Sandbox::new();
    let work = sandbox.clone_with_default("main", "repo");
    sandbox.git(&work, &["branch", "keep/한글"]);
    sandbox.git(&work, &["branch", "develop"]);
    sandbox.commit(&work, "advance main");
    let wt = sandbox.add_worktree(&work, "wt keep", "keep/한글");

    let protected = vec![String::from("keep/*"), String::from("develop")];
    let plan = scan(&work, &protected);
    assert_eq!(plan.removable_count(), 0, "{plan:?}");
    assert_eq!(count(&plan, TargetKind::Branch, SkipReason::Protected), 2);

    let result = CleanupExecutor::new(&ProcessGitRunner::new()).execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(wt.exists());
    assert!(sandbox.ref_exists(&work, "refs/heads/keep/한글"));
    assert!(sandbox.ref_exists(&work, "refs/heads/develop"));
}

/// tag, notes, stash, remote-tracking ref는 namespace 설정을 어떻게 넣어도 지우지 않는다.
#[test]
fn reserved_namespaces_survive_any_namespace_setting() {
    let sandbox = Sandbox::new();
    let work = sandbox.clone_with_default("main", "repo");
    sandbox.git(&work, &["tag", "v1"]);
    sandbox.git(&work, &["notes", "add", "-m", "note", "HEAD"]);
    sandbox.git(
        &work,
        &["update-ref", "refs/remotes/origin/merged-feature", "HEAD"],
    );
    std::fs::write(work.join("history.txt"), "stashed change\n").expect("modify");
    sandbox.git(&work, &["stash", "push", "-q"]);
    sandbox.commit(&work, "advance main");

    let namespaces: Vec<String> = [
        "refs/",
        "refs/tags/",
        "refs/notes/",
        "refs/remotes/",
        "refs/stash",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let runner = ProcessGitRunner::new();
    let plan = CleanupPlanner::new(&runner)
        .scan(&request(&work, &namespaces, &[]))
        .expect("scan");
    assert!(plan.custom_refs.is_empty(), "{:?}", plan.custom_refs);
    let result = CleanupExecutor::new(&runner).execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    for kept in [
        "refs/tags/v1",
        "refs/notes/commits",
        "refs/stash",
        "refs/remotes/origin/merged-feature",
        "refs/remotes/origin/main",
        "refs/heads/main",
    ] {
        assert!(sandbox.ref_exists(&work, kept), "{kept} must be kept");
    }
}

/// `refs/remote/*` 아래 symbolic ref는 후보로 올리지 않고, 그 대상 branch도 그대로 둔다.
///
/// `update-ref -d`는 `--no-deref` 없이 symbolic ref를 지우면 대상 ref를 지운다. base branch를
/// 가리키는 symbolic ref라면 `main`이 사라진다.
#[test]
fn symbolic_refs_are_skipped_and_targets_kept() {
    let sandbox = Sandbox::new();
    let work = sandbox.clone_with_default("main", "repo");
    sandbox.git(&work, &["branch", "merged"]);
    sandbox.commit(&work, "advance main");
    sandbox.git(
        &work,
        &[
            "symbolic-ref",
            "refs/remote/agent/to-main",
            "refs/heads/main",
        ],
    );
    sandbox.git(
        &work,
        &[
            "symbolic-ref",
            "refs/remote/agent/to-merged",
            "refs/heads/merged",
        ],
    );
    sandbox.git(
        &work,
        &["symbolic-ref", "refs/heads/alias", "refs/heads/merged"],
    );

    let plan = scan(&work, &[]);
    assert!(plan.custom_refs.is_empty(), "{:?}", plan.custom_refs);
    assert_eq!(
        count(&plan, TargetKind::CustomRef, SkipReason::SymbolicRef),
        2
    );
    assert_eq!(count(&plan, TargetKind::Branch, SkipReason::SymbolicRef), 1);

    let result = CleanupExecutor::new(&ProcessGitRunner::new()).execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(sandbox.ref_exists(&work, "refs/heads/main"));
    // `merged` 자체는 일반 branch로 정리되지만, symbolic ref 때문에 다른 ref가 지워지지는 않는다.
    assert_eq!(result.removed_branches, ["merged"]);
}

/// planner를 거치지 않고 symbolic ref가 후보로 들어와도 executor는 그 ref 자신만 지운다.
#[test]
fn executor_never_follows_symbolic_custom_ref() {
    let sandbox = Sandbox::new();
    let work = sandbox.clone_with_default("main", "repo");
    sandbox.git(
        &work,
        &[
            "symbolic-ref",
            "refs/remote/agent/to-main",
            "refs/heads/main",
        ],
    );

    let mut plan = scan(&work, &[]);
    plan.custom_refs.push(CustomRefCandidate {
        full_ref: String::from("refs/remote/agent/to-main"),
        commit: sandbox.oid(&work, "main"),
    });
    let result = CleanupExecutor::new(&ProcessGitRunner::new()).execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        sandbox.ref_exists(&work, "refs/heads/main"),
        "base must survive"
    );
    assert_eq!(sandbox.oid(&work, "main"), plan.base.commit);
}

/// 조작된 plan(base branch, `refs/remotes/*`, tag, main worktree)이 들어와도 executor가 지우지 않는다.
#[test]
fn executor_rejects_forged_candidates() {
    let sandbox = Sandbox::new();
    let work = sandbox.clone_with_default("main", "repo");
    sandbox.git(&work, &["tag", "v1"]);
    // main worktree가 `main`을 checkout하고 있으면 Git이 대신 막아 주므로, 일부러 detach한다.
    sandbox.git(&work, &["checkout", "-q", "--detach"]);

    let mut plan = scan(&work, &[]);
    let main_commit = sandbox.oid(&work, "main");
    plan.branches.push(BranchCandidate {
        name: String::from("main"),
        commit: main_commit.clone(),
    });
    plan.worktrees.push(WorktreeCandidate {
        path: work.clone(),
        branch: String::from("main"),
        commit: main_commit.clone(),
    });
    for full_ref in [
        "refs/remotes/origin/main",
        "refs/tags/v1",
        "refs/heads/main",
    ] {
        plan.custom_refs.push(CustomRefCandidate {
            full_ref: String::from(full_ref),
            commit: main_commit.clone(),
        });
    }

    let result = CleanupExecutor::new(&ProcessGitRunner::new()).execute(&plan);
    assert!(
        result.removed_branches.is_empty(),
        "{:?}",
        result.removed_branches
    );
    assert!(
        result.removed_worktrees.is_empty(),
        "{:?}",
        result.removed_worktrees
    );
    assert!(
        result.removed_custom_refs.is_empty(),
        "{:?}",
        result.removed_custom_refs
    );
    for kept in [
        "refs/heads/main",
        "refs/remotes/origin/main",
        "refs/tags/v1",
    ] {
        assert!(sandbox.ref_exists(&work, kept), "{kept} must be kept");
    }
    assert!(work.join("history.txt").exists());
}

/// shell metacharacter, option처럼 보이는 branch 이름과 path도 그대로 Git 인자로 넘어가야 한다.
///
/// `--` 덕분에 `-D`, `--force`라는 이름의 branch가 option으로 해석되지 않고, shell을 거치지 않으므로
/// `$(...)`, `;`가 실행되지 않는다.
#[test]
fn special_character_names_are_not_interpreted() {
    let sandbox = Sandbox::new();
    let work = sandbox.clone_with_default("main", "repo");
    // ref 이름에는 공백을 쓸 수 없어, shell이 해석하면 파일을 만드는 redirection 형태를 쓴다.
    let names = [
        "x$(>pwned)",
        "a;>pwned2",
        "q'uote\"d`>pwned4`",
        "-D",
        "--force",
    ];
    for name in names {
        // `git branch`는 `-`로 시작하는 이름을 거부하므로 ref를 직접 만든다.
        sandbox.git(
            &work,
            &["update-ref", &format!("refs/heads/{name}"), "HEAD"],
        );
    }
    sandbox.git(&work, &["branch", "wt-branch"]);
    sandbox.commit(&work, "advance main");
    let wt = sandbox.add_worktree(&work, "wt $(touch pwned3); 'q'", "wt-branch");

    let plan = scan(&work, &[]);
    assert_eq!(plan.branches.len(), names.len(), "{:?}", plan.branches);
    assert_eq!(plan.worktrees.len(), 1);
    let result = CleanupExecutor::new(&ProcessGitRunner::new()).execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.removed_branches.len(), names.len() + 1);
    assert!(!wt.exists());

    for name in names {
        assert!(
            !sandbox.ref_exists(&work, &format!("refs/heads/{name}")),
            "{name}"
        );
    }
    for marker in ["pwned", "pwned2", "pwned3", "pwned4"] {
        assert!(!work.join(marker).exists() && !sandbox.root.join(marker).exists());
    }
    assert!(sandbox.ref_exists(&work, "refs/heads/main"));
}
