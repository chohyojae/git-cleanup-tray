// SPDX-License-Identifier: GPL-3.0-only

//! 실제 Git 저장소에서 인프로세스 조회의 판정·읽기 전용 동작·재검사를 대조한다.

mod common;

use std::ffi::OsStr;
use std::path::Path;
use std::sync::{Arc, Mutex};

use common::{Sandbox, default_namespaces, path_str, request};
use git_cleanup_tray::cleanup::CleanupPlan;
use git_cleanup_tray::cleanup::candidate::{SkipReason, TargetKind};
use git_cleanup_tray::cleanup::executor::CleanupExecutor;
use git_cleanup_tray::cleanup::planner::CleanupPlanner;
use git_cleanup_tray::error::{CleanupError, Result};
use git_cleanup_tray::git::default_branch::{self, BaseBranchQuery, BaseBranchSource};
use git_cleanup_tray::git::in_process::InProcessGitRunner;
use git_cleanup_tray::git::runner::{GitOutput, GitRunner, ProcessGitRunner};
use git_cleanup_tray::git::{refs, repository, worktree};

/// Git 명령이 한 번이라도 필요하면 실패해 로컬 Scan의 process 제거를 확인한다.
struct NoCommands;

impl GitRunner for NoCommands {
    /// 변경 명령의 guard도 유지하며 명령 실행을 거부한다.
    fn run(&self, _: &Path, args: &[&OsStr]) -> Result<GitOutput> {
        git_cleanup_tray::git::guard::check_forbidden(args)?;
        Err(CleanupError::ForbiddenCommand(String::from(
            "unexpected git process in local read",
        )))
    }
}

/// 명령 종류만 기록해 원격 질의와 fallback을 확인한다. 전체 인자나 URL은 기록하지 않는다.
struct RecordingCommands(Arc<Mutex<Vec<String>>>);

impl GitRunner for RecordingCommands {
    /// 실제 guarded runner를 실행하고 호출 수를 기록한다.
    fn run(&self, path: &Path, args: &[&OsStr]) -> Result<GitOutput> {
        self.0
            .lock()
            .expect("command log")
            .push(args[0].to_string_lossy().into_owned());
        ProcessGitRunner::new().run(path, args)
    }
}

/// 순서 차이로 판정 차이가 가려지지 않도록 모든 후보·제외 사유와 기준 커밋을 비교한다.
fn assert_same_plan(left: &CleanupPlan, right: &CleanupPlan) {
    assert_eq!(left.base, right.base);
    assert_eq!(left.worktrees, right.worktrees);
    assert_eq!(left.branches, right.branches);
    assert_eq!(left.custom_refs, right.custom_refs);
    assert_eq!(left.skipped, right.skipped);
    assert!(left.errors.is_empty(), "{:?}", left.errors);
    assert!(right.errors.is_empty(), "{:?}", right.errors);
}

/// 공백·한글 경로와 보호·병합·symbolic·tag·dirty·locked 판정을 process 없이 대조한다.
#[test]
fn local_scan_matches_git_and_does_not_write_index() {
    let sandbox = Sandbox::new();
    let scenario = sandbox.scenario();
    sandbox.git(
        &scenario.work,
        &["tag", "-a", "-m", "tag object", "annotated", "HEAD"],
    );
    sandbox.git(
        &scenario.work,
        &["update-ref", "refs/remote/tag-object", "annotated"],
    );
    sandbox.git(
        &scenario.work,
        &["symbolic-ref", "refs/remote/alias", "refs/heads/main"],
    );
    sandbox.git(
        &scenario.work,
        &["config", "status.showUntrackedFiles", "no"],
    );
    let namespaces = default_namespaces();
    let protected = vec![String::from("release/*")];
    let input = request(&scenario.work, &namespaces, &protected);
    let command = ProcessGitRunner::new();
    let native = InProcessGitRunner::new(NoCommands);
    let identity = repository::identify(&command, &scenario.work).expect("identity");
    let index = identity.git_dir.join("index");
    let contents = std::fs::read(&index).expect("index");
    let modified = std::fs::metadata(&index)
        .expect("index metadata")
        .modified()
        .expect("mtime");
    let expected = CleanupPlanner::new(&command)
        .scan(&input)
        .expect("Git scan");
    let actual = CleanupPlanner::new(&native)
        .scan(&input)
        .expect("in-process scan");
    assert_same_plan(&expected, &actual);
    assert_eq!(std::fs::read(&index).expect("index after scan"), contents);
    assert_eq!(
        std::fs::metadata(&index)
            .expect("index metadata")
            .modified()
            .expect("mtime"),
        modified
    );
    assert!(!worktree::is_clean(&native, &scenario.wt_untracked).expect("untracked status"));
    assert!(!worktree::is_clean(&native, &scenario.wt_staged).expect("staged status"));
    assert!(!worktree::is_clean(&native, &scenario.wt_dirty).expect("modified status"));
    let base = &actual.base.commit;
    assert!(refs::is_ancestor(&native, &scenario.work, base, base).expect("equal commits"));
}

/// 저장된 원격 HEAD가 없을 때만 기존 Git 명령의 인증 경로로 질의한다.
#[test]
fn remote_head_query_keeps_existing_process_path() {
    let sandbox = Sandbox::new();
    let path = sandbox.clone_with_default("trunk", "원격 판별 저장소");
    sandbox.git(&path, &["remote", "set-head", "origin", "--delete"]);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let native = InProcessGitRunner::new(RecordingCommands(Arc::clone(&calls)));
    let mut query = BaseBranchQuery {
        remote: "origin",
        override_branch: None,
        allow_remote_query: false,
    };
    assert!(matches!(
        default_branch::resolve(&native, &path, query),
        Err(CleanupError::DefaultBranchUnknown(_))
    ));
    assert!(calls.lock().expect("calls").is_empty());
    query.allow_remote_query = true;
    let base = default_branch::resolve(&native, &path, query).expect("remote query");
    assert_eq!(base.branch, "trunk");
    assert_eq!(base.source, BaseBranchSource::RemoteQuery);
    assert_eq!(*calls.lock().expect("calls"), ["remote", "ls-remote"]);
}

/// Scan 이후 변경된 ref·base·worktree 상태는 새 라이브러리 조회로 확인하고 남긴다.
#[test]
fn execution_reopens_repository_and_preserves_changed_targets() {
    for change in ["base", "branch", "dirty", "locked", "symbolic"] {
        let sandbox = Sandbox::new();
        let scenario = sandbox.scenario();
        let namespaces = default_namespaces();
        let native = InProcessGitRunner::default();
        let plan = CleanupPlanner::new(&native)
            .scan(&request(&scenario.work, &namespaces, &[]))
            .expect("scan");
        let (kind, target) = match change {
            "base" => {
                sandbox.commit(&scenario.work, "base moves");
                (TargetKind::Branch, "기능/병합됨")
            }
            "branch" => {
                sandbox.git(
                    &scenario.work,
                    &["update-ref", "refs/heads/기능/병합됨", "unmerged"],
                );
                (TargetKind::Branch, "기능/병합됨")
            }
            "dirty" => {
                std::fs::write(scenario.wt_clean.join("새 파일"), "keep").expect("user writes");
                (TargetKind::Worktree, "")
            }
            "locked" => {
                sandbox.git(
                    &scenario.work,
                    &["worktree", "lock", path_str(&scenario.wt_clean)],
                );
                (TargetKind::Worktree, "")
            }
            _ => {
                sandbox.git(
                    &scenario.work,
                    &["update-ref", "-d", "refs/remote/agent/merged"],
                );
                sandbox.git(
                    &scenario.work,
                    &[
                        "symbolic-ref",
                        "refs/remote/agent/merged",
                        "refs/heads/main",
                    ],
                );
                (TargetKind::CustomRef, "refs/remote/agent/merged")
            }
        };
        let result = CleanupExecutor::new(&native).execute(&plan);
        assert!(result.errors.is_empty(), "{change}: {:?}", result.errors);
        assert!(
            result.skipped.iter().any(|item| item.kind == kind
                && item.reason == SkipReason::StateChanged
                && (target.is_empty() || item.target == target)),
            "{change}: {result:?}"
        );
        assert!(sandbox.ref_exists(&scenario.work, "refs/heads/main"));
        if kind == TargetKind::Worktree {
            assert!(scenario.wt_clean.exists());
        }
    }
}

/// 외부 필터가 설정된 저장소는 보수적으로 Git 명령으로 조회하며 판정을 유지한다.
#[test]
fn configured_filters_use_command_reads() {
    let sandbox = Sandbox::new();
    let path = sandbox.clone_with_default("trunk", "필터 미적용 파일 저장소");
    sandbox.git(&path, &["config", "filter.custom.clean", "cat"]);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let native = InProcessGitRunner::new(RecordingCommands(Arc::clone(&calls)));
    let namespaces = default_namespaces();
    let plan = CleanupPlanner::new(&native)
        .scan(&request(&path, &namespaces, &[]))
        .expect("fallback scan");
    assert_eq!(plan.base.branch, "trunk");
    assert!(
        calls
            .lock()
            .expect("calls")
            .iter()
            .any(|command| command == "for-each-ref")
    );
}

/// SHA-256과 packed ref에서도 같은 계획을 만들고 tag object를 커밋 ref로 오인하지 않는다.
#[test]
fn sha256_and_packed_refs_match_command_reads() {
    let sandbox = Sandbox::new();
    let path = sandbox.root.join("SHA256 저장소");
    std::fs::create_dir(&path).expect("repository directory");
    sandbox.git(
        &path,
        &["init", "-q", "--object-format=sha256", "-b", "기준"],
    );
    sandbox.commit(&path, "initial");
    sandbox.git(&path, &["branch", "완료"]);
    sandbox.git(&path, &["update-ref", "refs/remote/완료", "HEAD"]);
    sandbox.commit(&path, "base advances");
    sandbox.git(&path, &["gc", "--quiet"]);
    let namespaces = default_namespaces();
    let mut input = request(&path, &namespaces, &[]);
    input.base.override_branch = Some("기준");
    let expected = CleanupPlanner::new(&ProcessGitRunner::new())
        .scan(&input)
        .expect("command scan");
    let actual = CleanupPlanner::new(&InProcessGitRunner::new(NoCommands))
        .scan(&input)
        .expect("SHA256 gix scan");
    assert_same_plan(&expected, &actual);
    assert_eq!(actual.base.commit.as_str().len(), 64);
}

/// 내용은 그대로인 파일의 갱신과 하위 경로 조회에서도 저장소 전체를 정확히 검사한다.
#[test]
fn unchanged_content_and_subdirectory_status_match_git() {
    let sandbox = Sandbox::new();
    let path = sandbox.clone_with_default("trunk", "파일 시각 저장소");
    let directory = path.join("하위 폴더");
    std::fs::create_dir(&directory).expect("subdirectory");
    std::fs::write(path.join(".gitignore"), "하위 폴더/\n").expect("ignore directory");
    sandbox.git(&path, &["add", ".gitignore"]);
    sandbox.commit(&path, "ignore directory");
    let file = path.join("history.txt");
    let original = std::fs::read(&file).expect("original contents");
    std::fs::write(&file, &original).expect("rewrite same content");
    let native = InProcessGitRunner::new(NoCommands);
    assert!(worktree::is_clean(&ProcessGitRunner::new(), &path).expect("command clean"));
    assert!(worktree::is_clean(&native, &path).expect("unchanged gix content"));
    std::fs::write(&file, "changed outside subdirectory\n").expect("user change");
    assert!(!worktree::is_clean(&native, &directory).expect("full repository status"));
}

/// 서브모듈 안의 추적·미추적 변경과 사용자 ignore 설정도 기존 Git 결과와 일치해야 한다.
#[test]
fn submodule_status_matches_git_without_processes() {
    let sandbox = Sandbox::new();
    let module = sandbox.clone_with_default("trunk", "서브모듈 원본");
    let parent = sandbox.clone_with_default("trunk", "서브모듈 부모");
    sandbox.git(
        &parent,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            path_str(&module),
            "module",
        ],
    );
    sandbox.commit(&parent, "add submodule");
    let native = InProcessGitRunner::new(NoCommands);
    assert!(worktree::is_clean(&native, &parent).expect("clean submodule"));
    std::fs::write(parent.join("module/history.txt"), "modified\n").expect("edit module");
    assert!(!worktree::is_clean(&native, &parent).expect("dirty submodule"));
    sandbox.git(&parent, &["config", "submodule.module.ignore", "all"]);
    assert_eq!(
        worktree::is_clean(&native, &parent).expect("gix ignore"),
        worktree::is_clean(&ProcessGitRunner::new(), &parent).expect("git ignore")
    );
}

/// reftable은 라이브러리의 files ref 조회를 쓰지 않고 Git 명령으로 처리한다.
#[test]
fn reftable_uses_command_reads() {
    let sandbox = Sandbox::new();
    let path = sandbox.root.join("reftable 저장소");
    std::fs::create_dir(&path).expect("repository directory");
    sandbox.git(
        &path,
        &["init", "-q", "--ref-format=reftable", "-b", "기준"],
    );
    sandbox.commit(&path, "initial");
    sandbox.git(&path, &["branch", "완료"]);
    let namespaces = default_namespaces();
    let mut input = request(&path, &namespaces, &[]);
    input.base.override_branch = Some("기준");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let native = InProcessGitRunner::new(RecordingCommands(Arc::clone(&calls)));
    let expected = CleanupPlanner::new(&ProcessGitRunner::new())
        .scan(&input)
        .expect("command scan");
    let actual = CleanupPlanner::new(&native)
        .scan(&input)
        .expect("reftable fallback");
    assert_same_plan(&expected, &actual);
    assert!(!calls.lock().expect("calls").is_empty());
}

/// sparse checkout은 지원 여부가 불확실한 상태 판정을 섞지 않고 Git 명령을 사용한다.
#[test]
fn sparse_checkout_uses_command_reads() {
    let sandbox = Sandbox::new();
    let path = sandbox.clone_with_default("trunk", "sparse 저장소");
    sandbox.git(
        &path,
        &["sparse-checkout", "init", "--cone", "--sparse-index"],
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let native = InProcessGitRunner::new(RecordingCommands(Arc::clone(&calls)));
    assert!(worktree::is_clean(&native, &path).expect("sparse status"));
    assert!(!calls.lock().expect("calls").is_empty());
}

/// upstream의 축약·복수 merge 설정과 fetch mapping 누락도 Git 결과와 같아야 한다.
#[test]
fn upstream_configuration_matches_git() {
    for change in ["normal", "short", "multiple", "no-fetch"] {
        let sandbox = Sandbox::new();
        let path = sandbox.clone_with_default("trunk", "upstream 설정 저장소");
        match change {
            "short" => {
                sandbox.git(&path, &["config", "branch.trunk.merge", "trunk"]);
            }
            "multiple" => {
                sandbox.git(
                    &path,
                    &["config", "--add", "branch.trunk.merge", "refs/heads/other"],
                );
            }
            "no-fetch" => {
                sandbox.git(&path, &["config", "--unset-all", "remote.origin.fetch"]);
            }
            _ => {}
        }
        let command = ProcessGitRunner::new();
        let native = InProcessGitRunner::default();
        let expected = command
            .open_read(&path)
            .expect("command reader")
            .tracked_branches()
            .expect("upstream");
        let actual = native
            .open_read(&path)
            .expect("native reader")
            .tracked_branches()
            .expect("upstream");
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(&expected) {
            assert_eq!(
                (
                    &actual.name,
                    &actual.commit,
                    &actual.remote,
                    &actual.upstream
                ),
                (
                    &expected.name,
                    &expected.commit,
                    &expected.remote,
                    &expected.upstream
                ),
                "{change}"
            );
        }
    }
}
