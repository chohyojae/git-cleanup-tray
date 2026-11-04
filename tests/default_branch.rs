// SPDX-License-Identifier: GPL-3.0-only

//! default branch 판별 integration test (PLAN 7장, 26장).
//!
//! branch 이름을 추측하지 않고 remote 설정에서 읽는지, 단계별 fallback이 맞는지 확인한다.

mod common;

use common::Sandbox;
use git_cleanup_tray::error::CleanupError;
use git_cleanup_tray::git::default_branch::{self, BaseBranchQuery, BaseBranchSource, BaseRef};
use git_cleanup_tray::git::runner::ProcessGitRunner;

/// 기본 질의 (remote `origin`, override 없음, remote 질의 허용).
const DEFAULT_QUERY: BaseBranchQuery<'static> = BaseBranchQuery {
    remote: "origin",
    override_branch: None,
    allow_remote_query: true,
};

/// main, master, trunk, 임의 이름 default branch를 모두 local symbolic ref에서 판별한다.
#[test]
fn detects_any_default_branch_name() {
    let sandbox = Sandbox::new();
    let runner = ProcessGitRunner::new();
    for (index, branch) in ["main", "master", "trunk", "개발-라인"]
        .into_iter()
        .enumerate()
    {
        let work = sandbox.clone_with_default(branch, &format!("repo {index}"));
        let base = default_branch::resolve(&runner, &work, DEFAULT_QUERY).expect("resolve");
        assert_eq!(base.branch, branch);
        assert_eq!(base.source, BaseBranchSource::LocalSymbolicRef);
        assert_eq!(
            base.base_ref,
            BaseRef::Local(format!("refs/heads/{branch}"))
        );
    }
}

/// `origin/HEAD`가 없으면 `ls-remote --symref`로 질의하고, 질의가 꺼져 있으면 판별 실패를 알린다.
#[test]
fn falls_back_to_remote_query_only_when_allowed() {
    let sandbox = Sandbox::new();
    let runner = ProcessGitRunner::new();
    let work = sandbox.clone_with_default("trunk", "repo");
    sandbox.git(&work, &["remote", "set-head", "origin", "--delete"]);

    let base = default_branch::resolve(&runner, &work, DEFAULT_QUERY).expect("resolve");
    assert_eq!(base.branch, "trunk");
    assert_eq!(base.source, BaseBranchSource::RemoteQuery);

    let offline = BaseBranchQuery {
        allow_remote_query: false,
        ..DEFAULT_QUERY
    };
    assert!(matches!(
        default_branch::resolve(&runner, &work, offline),
        Err(CleanupError::DefaultBranchUnknown(_))
    ));
}

/// local default branch가 없으면 remote-tracking ref를 base로 쓴다.
#[test]
fn uses_remote_tracking_ref_without_local_branch() {
    let sandbox = Sandbox::new();
    let runner = ProcessGitRunner::new();
    let work = sandbox.clone_with_default("main", "repo");
    sandbox.git(&work, &["checkout", "-q", "--detach"]);
    sandbox.git(&work, &["branch", "-D", "main"]);

    let base = default_branch::resolve(&runner, &work, DEFAULT_QUERY).expect("resolve");
    assert_eq!(
        base.base_ref,
        BaseRef::RemoteTracking(String::from("refs/remotes/origin/main"))
    );
}

/// override가 있으면 remote 설정보다 우선한다. 없는 branch를 지정하면 판별 실패다.
#[test]
fn override_takes_precedence() {
    let sandbox = Sandbox::new();
    let runner = ProcessGitRunner::new();
    let work = sandbox.clone_with_default("main", "repo");
    sandbox.git(&work, &["branch", "develop"]);

    let query = BaseBranchQuery {
        override_branch: Some("develop"),
        ..DEFAULT_QUERY
    };
    let base = default_branch::resolve(&runner, &work, query).expect("resolve");
    assert_eq!(base.branch, "develop");
    assert_eq!(base.source, BaseBranchSource::Override);

    let missing = BaseBranchQuery {
        override_branch: Some("no-such-branch"),
        ..DEFAULT_QUERY
    };
    assert!(matches!(
        default_branch::resolve(&runner, &work, missing),
        Err(CleanupError::DefaultBranchUnknown(_))
    ));
}

/// 설정에 없는 remote를 질의하면 `RemoteNotFound`.
#[test]
fn reports_unknown_remote() {
    let sandbox = Sandbox::new();
    let runner = ProcessGitRunner::new();
    let work = sandbox.clone_with_default("main", "repo");
    let query = BaseBranchQuery {
        remote: "upstream",
        ..DEFAULT_QUERY
    };
    assert!(matches!(
        default_branch::resolve(&runner, &work, query),
        Err(CleanupError::RemoteNotFound(_))
    ));
}
