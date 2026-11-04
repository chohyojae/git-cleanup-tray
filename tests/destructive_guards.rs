// SPDX-License-Identifier: GPL-3.0-only

//! 파괴적 명령 차단 regression test (PLAN 24, 26장).
//!
//! guard 단위 test와 별도로, 실제 [`ProcessGitRunner`]가 금지 명령을 process로 내보내기 전에
//! 거부하는지와 공백·한글 경로에서 정상 동작하는지 확인한다. 또 recording runner로 실제 Scan/Execute가
//! 내보내는 명령 전체를 기록해 금지 명령이 한 번도 나가지 않는지 검사한다.

mod common;

use std::ffi::OsStr;
use std::path::Path;
use std::sync::Mutex;

use common::{Sandbox, default_namespaces, request};
use git_cleanup_tray::cleanup::executor::CleanupExecutor;
use git_cleanup_tray::cleanup::planner::CleanupPlanner;
use git_cleanup_tray::error::{CleanupError, Result};
use git_cleanup_tray::git::runner::{GitOutput, GitRunner, ProcessGitRunner};

/// 문자열 slice를 runner 인자 형식으로 바꾼다.
fn os(args: &[&'static str]) -> Vec<&'static OsStr> {
    args.iter().map(|arg| OsStr::new(*arg)).collect()
}

/// 금지 명령은 git을 실행하기 전에 `ForbiddenCommand`로 거부되어야 한다.
///
/// 존재하지 않는 git 경로를 쓰므로, 만약 guard를 통과해 spawn까지 갔다면 `GitNotFound`가 나와 test가 실패한다.
#[test]
fn runner_blocks_forbidden_commands_before_spawn() {
    let runner = ProcessGitRunner::with_executable("/nonexistent/git");
    let dir = tempfile::tempdir().expect("create temp dir");

    for args in [
        &["branch", "-D", "--", "feature"][..],
        &["worktree", "remove", "--force", "--", "/tmp/wt"],
        &["push", "--delete", "origin", "feature"],
        &["reset", "--hard"],
        &["clean", "-fd"],
        &["update-ref", "-d", "refs/remotes/origin/main"],
        // long option 앞부분, 원격 추적 branch 삭제, symbolic ref 추적 삭제
        &["branch", "-d", "--forc", "--", "feature"],
        &["worktree", "remove", "--fo", "--", "/tmp/wt"],
        &["branch", "-d", "-r", "--", "origin/feature"],
        &["update-ref", "-d", "refs/remote/agent/x"],
        &[
            "-c",
            "core.quotePath=false",
            "branch",
            "-D",
            "--",
            "feature",
        ],
    ] {
        let result = runner.run(dir.path(), &os(args));
        assert!(
            matches!(result, Err(CleanupError::ForbiddenCommand(_))),
            "{args:?} must be blocked, got {result:?}"
        );
    }
}

/// 공백과 한글이 포함된 경로에서도 `git init`과 `rev-parse`가 동작해야 한다.
#[test]
fn runner_handles_unicode_and_space_paths() {
    let root = tempfile::tempdir().expect("create temp dir");
    let repo = root.path().join("한글 저장소 path");
    std::fs::create_dir(&repo).expect("create repo dir");

    let runner = ProcessGitRunner::new();
    let init = runner
        .run(&repo, &os(&["init", "--quiet"]))
        .expect("run git init");
    assert!(init.success(), "git init failed: {}", init.stderr_lossy());

    let toplevel = runner
        .run(&repo, &os(&["rev-parse", "--show-toplevel"]))
        .expect("run rev-parse");
    assert!(
        toplevel.success(),
        "rev-parse failed: {}",
        toplevel.stderr_lossy()
    );
    assert!(
        toplevel
            .stdout_lossy()
            .trim_end()
            .ends_with("한글 저장소 path")
    );
}

/// 실제 runner에 위임하면서 실행한 인자 목록을 모두 기록하는 test runner.
struct RecordingRunner {
    /// 실제 실행을 맡는 runner.
    inner: ProcessGitRunner,
    /// 실행 순서대로 기록한 인자 목록.
    calls: Mutex<Vec<Vec<String>>>,
}

impl GitRunner for RecordingRunner {
    /// 인자를 기록한 뒤 실제 runner로 실행한다. guard에 막힌 명령도 기록에 남는다.
    fn run(&self, repository: &Path, args: &[&OsStr]) -> Result<GitOutput> {
        self.calls.lock().expect("lock call log").push(
            args.iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
        );
        self.inner.run(repository, args)
    }
}

/// 인자 목록이 PLAN 24장의 금지 명령에 해당하면 `true`. guard와 별개로 문자열 수준에서 판정한다.
fn is_forbidden(args: &[String]) -> bool {
    let has = |flag: &str| args.iter().any(|a| a == flag);
    match args.first().map(String::as_str) {
        Some("push" | "reset" | "clean" | "stash") => true,
        Some("branch") => has("-D") || has("-f") || has("--force") || has("-r"),
        Some("worktree") => has("--force") || has("-f"),
        Some("update-ref") => {
            has("-d") && (!has("--no-deref") || !args.iter().any(|a| a.starts_with("refs/remote/")))
        }
        _ => false,
    }
}

/// 전체 scenario의 Scan과 Execute가 금지 명령을 한 번도 실행하지 않고,
/// 실제 삭제 명령(`worktree remove`, `branch -d`, `update-ref -d`)은 실행했는지 확인한다.
#[test]
fn full_cleanup_never_issues_forbidden_commands() {
    let sandbox = Sandbox::new();
    let s = sandbox.scenario();
    let runner = RecordingRunner {
        inner: ProcessGitRunner::new(),
        calls: Mutex::new(Vec::new()),
    };
    let namespaces = default_namespaces();

    let plan = CleanupPlanner::new(&runner)
        .scan(&request(&s.work, &namespaces, &[]))
        .expect("scan");
    let result = CleanupExecutor::new(&runner).execute(&plan);
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    let calls = runner.calls.into_inner().expect("call log");
    let forbidden: Vec<&Vec<String>> = calls.iter().filter(|args| is_forbidden(args)).collect();
    assert!(
        forbidden.is_empty(),
        "forbidden commands issued: {forbidden:?}"
    );

    let issued = |sub: &str, flag: &str| {
        calls
            .iter()
            .any(|args| args.first().is_some_and(|a| a == sub) && args.iter().any(|a| a == flag))
    };
    assert!(issued("worktree", "remove"));
    assert!(issued("branch", "-d"));
    assert!(issued("update-ref", "-d"));
    // Scan 단계에서는 어떤 mutation 명령도 나가지 않아야 한다: 삭제 명령은 모두 base 재검증 뒤에 있다.
    let first_mutation = calls
        .iter()
        .position(|args| {
            matches!(
                args.first().map(String::as_str),
                Some("branch" | "update-ref")
            ) || (args.first().is_some_and(|a| a == "worktree")
                && args.get(1).is_some_and(|a| a == "remove"))
        })
        .expect("mutation issued");
    let first_revalidation = calls
        .iter()
        .position(|args| args.first().is_some_and(|a| a == "symbolic-ref"))
        .and_then(|scan_symref| {
            calls
                .iter()
                .skip(scan_symref + 1)
                .position(|args| args.first().is_some_and(|a| a == "symbolic-ref"))
                .map(|offset| scan_symref + 1 + offset)
        })
        .expect("execute revalidates base");
    assert!(first_revalidation < first_mutation);
}

/// Scan은 읽기 전용 명령만 실행해야 한다 (PLAN 9장 "Scan: mutation 없이 CleanupPlan만 만든다").
///
/// 허용 목록 밖의 명령이 하나라도 나가면 실패한다. remote 질의 경로(`remote get-url`, `ls-remote`)도
/// 타도록 `origin/HEAD`를 지운 상태에서 Scan한다.
#[test]
fn scan_issues_only_read_only_commands() {
    let sandbox = Sandbox::new();
    let s = sandbox.scenario();
    sandbox.git(&s.work, &["remote", "set-head", "origin", "--delete"]);
    let runner = RecordingRunner {
        inner: ProcessGitRunner::new(),
        calls: Mutex::new(Vec::new()),
    };
    let namespaces = default_namespaces();
    let plan = CleanupPlanner::new(&runner)
        .scan(&request(&s.work, &namespaces, &[]))
        .expect("scan");
    assert!(plan.removable_count() > 0);

    let calls = runner.calls.into_inner().expect("call log");
    let read_only = |args: &[String]| {
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        matches!(
            words.as_slice(),
            ["rev-parse" | "ls-remote" | "for-each-ref" | "status", ..]
                | ["symbolic-ref", "--quiet", _]
                | ["remote", "get-url", _]
                | ["worktree", "list", ..]
                | ["merge-base", "--is-ancestor", ..]
        )
    };
    let mutating: Vec<&Vec<String>> = calls.iter().filter(|args| !read_only(args)).collect();
    assert!(
        mutating.is_empty(),
        "scan issued non read-only commands: {mutating:?}"
    );
    assert!(
        calls
            .iter()
            .any(|args| args.first().is_some_and(|a| a == "ls-remote"))
    );
}
