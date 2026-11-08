// SPDX-License-Identifier: GPL-3.0-only

//! 실제 로컬 remote로 fetch, fast-forward, worktree 보호 및 경합을 검증한다.

mod common;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use common::{Sandbox, path_str};
use git_cleanup_tray::git::runner::{GitOutput, GitRunner, ProcessGitRunner};
use git_cleanup_tray::git::sync::{self, SyncResult};

/// 원격에 새 commit이 있지만 로컬에는 아직 fetch되지 않은 저장소.
struct Fixture {
    /// 격리된 임시 저장소들.
    sandbox: Sandbox,
    /// 갱신 대상 작업 저장소.
    work: PathBuf,
    /// 원격 변경을 만드는 작업 저장소.
    seed: PathBuf,
}

impl Fixture {
    /// 이름을 추측하지 않도록 `trunk`를 base로 사용한다.
    fn new() -> Self {
        let sandbox = Sandbox::new();
        let work = sandbox.clone_with_default("trunk", "최신화 작업");
        let seed = sandbox.root.join("seed 최신화 작업");
        let origin = sandbox.root.join("origin 최신화 작업.git");
        sandbox.git(&seed, &["remote", "add", "origin", path_str(&origin)]);
        Self {
            sandbox,
            work,
            seed,
        }
    }

    /// 원격 trunk를 한 commit 진행시킨다.
    fn advance(&self) {
        self.sandbox.commit(&self.seed, "remote advances");
        self.sandbox
            .git(&self.seed, &["push", "-q", "origin", "trunk"]);
    }

    /// 실제 runner로 최신화를 실행한다.
    fn sync(&self) -> SyncResult {
        sync::synchronize(&common::test_runner(), &self.work, "origin", &|| false)
            .expect("synchronize")
    }
}

/// main worktree와 checkout되지 않은 브랜치는 모두 fast-forward되고 tag/custom ref는 그대로다.
#[test]
fn fetches_and_fast_forwards_without_checkout_or_pruning() {
    let f = Fixture::new();
    f.sandbox
        .git(&f.work, &["branch", "--track", "기능/대기", "origin/trunk"]);
    f.sandbox
        .git(&f.work, &["update-ref", "refs/remotes/origin/keep", "HEAD"]);
    f.sandbox
        .git(&f.work, &["update-ref", "refs/remote/keep", "HEAD"]);
    f.sandbox.git(&f.work, &["tag", "keep"]);
    let before = f.sandbox.oid(&f.work, "HEAD");
    f.advance();
    // 사용자의 fetch/prune 설정이 로컬 branch/tag 삭제로 이어지지 않아야 한다.
    f.sandbox.git(
        &f.work,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/heads/*",
        ],
    );
    f.sandbox.git(
        &f.work,
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    f.sandbox.git(&f.work, &["config", "fetch.prune", "true"]);
    f.sandbox
        .git(&f.work, &["config", "fetch.pruneTags", "true"]);
    let result = f.sync();
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.updated, 2, "{result:?}");
    let target = f.sandbox.oid(&f.seed, "HEAD");
    assert_eq!(f.sandbox.oid(&f.work, "HEAD"), target);
    assert_eq!(f.sandbox.oid(&f.work, "기능/대기"), target);
    assert_eq!(f.sandbox.oid(&f.work, "keep"), before);
    assert_eq!(f.sandbox.oid(&f.work, "refs/remote/keep"), before);
    assert!(f.sandbox.ref_exists(&f.work, "refs/remotes/origin/keep"));
    assert_eq!(
        std::fs::read(f.work.join("history.txt")).expect("read"),
        std::fs::read(f.seed.join("history.txt")).expect("read")
    );
    assert_eq!(f.sync().updated, 0);
}

/// dirty/staged/untracked worktree는 autostash 설정이 있어도 변경하지 않는다.
#[test]
fn keeps_all_dirty_states_without_stashing() {
    for kind in ["modified", "staged", "untracked"] {
        let f = Fixture::new();
        f.advance();
        f.sandbox
            .git(&f.work, &["config", "merge.autostash", "true"]);
        let file = if kind == "modified" {
            "history.txt"
        } else {
            "local.txt"
        };
        std::fs::write(f.work.join(file), kind).expect("dirty file");
        if kind == "staged" {
            f.sandbox.git(&f.work, &["add", file]);
        }
        let before = f.sandbox.oid(&f.work, "HEAD");
        let result = f.sync();
        assert_eq!(result.updated, 0, "{kind}: {result:?}");
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(f.sandbox.oid(&f.work, "HEAD"), before);
        assert_eq!(
            std::fs::read_to_string(f.work.join(file)).expect("read"),
            kind
        );
        assert!(!f.sandbox.ref_exists(&f.work, "refs/stash"));
    }
}

/// linked worktree 중 clean만 최신화하고 locked/dirty worktree는 보존한다.
#[test]
fn updates_clean_linked_worktrees_and_keeps_locked_ones() {
    let f = Fixture::new();
    for branch in ["clean", "locked", "dirty"] {
        f.sandbox
            .git(&f.work, &["branch", "--track", branch, "origin/trunk"]);
    }
    let clean = f.sandbox.add_worktree(&f.work, "깨끗한 worktree", "clean");
    let locked = f.sandbox.add_worktree(&f.work, "잠긴 worktree", "locked");
    let dirty = f.sandbox.add_worktree(&f.work, "수정한 worktree", "dirty");
    f.sandbox
        .git(&f.work, &["worktree", "lock", path_str(&locked)]);
    std::fs::write(dirty.join("new.txt"), "keep").expect("write");
    let before = f.sandbox.oid(&f.work, "HEAD");
    f.advance();
    let result = f.sync();
    assert_eq!(result.updated, 2, "{result:?}");
    assert_eq!(result.skipped.len(), 2);
    assert_eq!(
        f.sandbox.oid(&clean, "HEAD"),
        f.sandbox.oid(&f.seed, "HEAD")
    );
    assert_eq!(f.sandbox.oid(&locked, "HEAD"), before);
    assert_eq!(f.sandbox.oid(&dirty, "HEAD"), before);
}

/// 로컬 전용 commit이 있으면 checked-out 여부와 관계없이 ref를 되돌리거나 merge하지 않는다.
#[test]
fn preserves_ahead_and_diverged_branches_and_missing_upstreams() {
    let f = Fixture::new();
    f.sandbox.git(&f.work, &["branch", "no-upstream"]);
    f.sandbox
        .git(&f.work, &["branch", "--track", "missing", "origin/trunk"]);
    f.sandbox.git(
        &f.work,
        &["config", "branch.missing.merge", "refs/heads/missing"],
    );
    f.sandbox.commit(&f.work, "local only");
    f.sandbox
        .git(&f.work, &["branch", "--track", "ahead", "origin/trunk"]);
    f.sandbox
        .git(&f.work, &["update-ref", "refs/heads/ahead", "HEAD"]);
    let before = f.sandbox.oid(&f.work, "HEAD");
    assert_eq!(f.sync().updated, 0);
    f.advance();
    let result = f.sync();
    assert_eq!(result.updated, 0, "{result:?}");
    assert_eq!(result.skipped.len(), 4);
    assert_eq!(f.sandbox.oid(&f.work, "HEAD"), before);
    assert_eq!(f.sandbox.oid(&f.work, "ahead"), before);
}

/// 새 커밋이 없는 rebase 도중에도 HEAD와 작업 파일은 보존한다.
#[test]
fn does_not_update_during_an_in_progress_operation() {
    let f = Fixture::new();
    f.advance();
    let git_dir = f.sandbox.git(&f.work, &["rev-parse", "--absolute-git-dir"]);
    std::fs::create_dir(Path::new(git_dir.trim()).join("rebase-merge")).expect("rebase marker");
    let before = f.sandbox.oid(&f.work, "HEAD");
    let result = f.sync();
    assert_eq!(result.updated, 0);
    assert_eq!(f.sandbox.oid(&f.work, "HEAD"), before);
}

/// fetch 실패나 사전 취소 시 로컬 branch를 변경하지 않는다.
#[test]
fn failed_fetch_and_cancel_do_not_update_local_branches() {
    let f = Fixture::new();
    f.advance();
    let result =
        sync::synchronize(&ProcessGitRunner::new(), &f.work, "origin", &|| true).expect("cancel");
    assert_eq!(result.updated, 0);
    assert_ne!(
        f.sandbox.oid(&f.work, "origin/trunk"),
        f.sandbox.oid(&f.seed, "HEAD")
    );
    f.sandbox.git(
        &f.work,
        &[
            "remote",
            "set-url",
            "origin",
            path_str(&f.sandbox.root.join("absent")),
        ],
    );
    let before = f.sandbox.oid(&f.work, "HEAD");
    assert!(sync::synchronize(&ProcessGitRunner::new(), &f.work, "origin", &|| false).is_err());
    assert_eq!(f.sandbox.oid(&f.work, "HEAD"), before);
}

/// 최초 ref 목록을 읽은 뒤 branch가 움직이는 경합을 재현하는 runner.
struct MovingRunner<'a> {
    /// 실제 Git fixture.
    fixture: &'a Fixture,
    /// 경합을 한 번만 일으키는 flag.
    moved: AtomicBool,
}

impl GitRunner for MovingRunner<'_> {
    /// upstream 목록을 읽은 뒤 새 commit을 만들어 실행 직전 재검사를 검증한다.
    fn run(&self, path: &Path, args: &[&OsStr]) -> git_cleanup_tray::error::Result<GitOutput> {
        let output = ProcessGitRunner::new().run(path, args)?;
        if args
            .iter()
            .any(|arg| arg.to_string_lossy().contains("%(upstream:remoteref)"))
            && !self.moved.swap(true, Ordering::SeqCst)
        {
            self.fixture
                .sandbox
                .commit(&self.fixture.work, "concurrent local commit");
        }
        Ok(output)
    }
}

/// 목록을 읽은 뒤 branch가 진행되면 새 로컬 commit을 보존한다.
#[test]
fn revalidates_branch_before_mutation() {
    let f = Fixture::new();
    f.advance();
    let runner = MovingRunner {
        fixture: &f,
        moved: AtomicBool::new(false),
    };
    let result = sync::synchronize(&runner, &f.work, "origin", &|| false).expect("sync");
    assert_eq!(result.updated, 0);
    assert!(
        result
            .skipped
            .iter()
            .any(|reason| reason.contains("changed"))
    );
    assert_eq!(
        f.sandbox.git(&f.work, &["log", "-1", "--format=%s"]).trim(),
        "concurrent local commit"
    );
}

/// tracking ref가 로컬 branch를 가리키더라도 fetch가 이를 따라가서 변경하지 않는다.
#[test]
fn refuses_symbolic_fetch_destinations() {
    let f = Fixture::new();
    f.sandbox.git(
        &f.work,
        &[
            "symbolic-ref",
            "refs/remotes/origin/trunk",
            "refs/heads/trunk",
        ],
    );
    let before = f.sandbox.oid(&f.work, "HEAD");
    f.advance();
    assert!(sync::synchronize(&ProcessGitRunner::new(), &f.work, "origin", &|| false).is_err());
    assert_eq!(f.sandbox.oid(&f.work, "HEAD"), before);
}

/// fetch.showForcedUpdates=false 설정이 fast-forward 검사를 생략하지 않도록 명시적으로 켠다.
#[test]
fn enforces_fast_forward_with_user_fetch_configuration() {
    let f = Fixture::new();
    f.sandbox
        .git(&f.work, &["config", "fetch.showForcedUpdates", "false"]);
    f.sandbox
        .git(&f.work, &["branch", "--track", "waiting", "origin/trunk"]);
    f.advance();
    let result = f.sync();
    assert_eq!(result.updated, 2, "{result:?}");
    assert!(result.errors.is_empty());
}
