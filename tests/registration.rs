// SPDX-License-Identifier: GPL-3.0-only

//! 저장소 등록 검증 test (PLAN 6장).
//!
//! Git 저장소인지 확인하고, 같은 저장소를 다른 worktree 경로로 두 번 등록하지 않는지 본다.

mod common;

use common::Sandbox;
use git_cleanup_tray::app::tasks::{ChangeError, new_repository};
use git_cleanup_tray::error::CleanupError;
use git_cleanup_tray::git::repository::RepositoryId;
use git_cleanup_tray::storage::settings_store::AppSettings;

/// 하위 폴더를 골라도 작업 tree 최상위로 등록하고, 기본 remote·namespace를 채운다.
#[test]
fn registers_toplevel_with_defaults() {
    let sandbox = Sandbox::new();
    let work = sandbox.clone_with_default("main", "등록 저장소");
    let subdir = work.join("하위 폴더");
    std::fs::create_dir(&subdir).expect("create subdir");
    let settings = AppSettings {
        default_remote: String::from("upstream"),
        ..AppSettings::default()
    };

    let config = new_repository(&[], &settings, &subdir).expect("register");

    assert_eq!(config.id, RepositoryId(1));
    assert_eq!(config.name, "등록 저장소");
    assert_eq!(
        std::fs::canonicalize(&config.path).expect("canonical path"),
        std::fs::canonicalize(&work).expect("canonical work")
    );
    assert_eq!(config.remote_name, "upstream");
    assert_eq!(config.custom_ref_namespaces, ["refs/remote/"]);
    assert!(config.enabled);
}

/// linked worktree 경로로 같은 저장소를 다시 등록하면 거부한다.
#[test]
fn rejects_same_repository_through_linked_worktree() {
    let sandbox = Sandbox::new();
    let work = sandbox.clone_with_default("main", "원본 저장소");
    sandbox.git(&work, &["branch", "작업"]);
    let linked = sandbox.add_worktree(&work, "연결된 worktree", "작업");
    let settings = AppSettings::default();
    let first = new_repository(&[], &settings, &work).expect("register main worktree");

    let error = new_repository(std::slice::from_ref(&first), &settings, &linked)
        .expect_err("duplicate must be rejected");

    assert!(
        matches!(&error, ChangeError::AlreadyRegistered(name) if name == "원본 저장소"),
        "{error:?}"
    );
}

/// Git 저장소가 아닌 폴더는 등록하지 않는다.
#[test]
fn rejects_plain_folder() {
    let sandbox = Sandbox::new();
    let plain = sandbox.root.join("그냥 폴더");
    std::fs::create_dir(&plain).expect("create folder");

    let error = new_repository(&[], &AppSettings::default(), &plain).expect_err("not a repo");

    assert!(
        matches!(error, ChangeError::Failed(CleanupError::NotARepository(_))),
        "{error:?}"
    );
}
