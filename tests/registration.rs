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

/// 여러 입력에 중복 worktree와 잘못된 폴더가 섞여도 정상 저장소는 모두 저장한다.
#[test]
fn batch_registration_keeps_valid_folders_and_reports_rejections() {
    use git_cleanup_tray::app::registration::add_repositories;
    use git_cleanup_tray::storage::repository_store;

    let sandbox = Sandbox::new();
    let first = sandbox.clone_with_default("main", "기존 저장소");
    let second = sandbox.clone_with_default("trunk", "새 저장소");
    sandbox.git(&first, &["branch", "작업"]);
    let linked = sandbox.add_worktree(&first, "중복 worktree", "작업");
    let plain = sandbox.root.join("Git 아닌 폴더");
    std::fs::create_dir(&plain).expect("create plain directory");
    let missing = sandbox.root.join("없는 폴더");
    let data = sandbox.root.join("앱 설정");
    let mut existing = new_repository(&[], &AppSettings::default(), &first).expect("existing");
    existing.name = String::from("사용자가 바꾼 이름");
    existing.enabled = false;
    repository_store::save(&data, std::slice::from_ref(&existing)).expect("save existing");
    let mut progress = Vec::new();

    let report = add_repositories(
        &data,
        &[linked, plain.clone(), second, first, missing.clone()],
        &mut |message| progress.push(message),
    )
    .expect("register batch");

    assert_eq!((report.added, report.duplicates, report.failed), (1, 2, 2));
    assert_eq!(report.repositories.len(), 2);
    assert_eq!(report.repositories[0], existing);
    assert_eq!(report.repositories[1].id, RepositoryId(2));
    assert_eq!(report.problems[0].path, plain);
    assert_eq!(report.problems[1].path, missing);
    assert_eq!(progress.len(), 5);
    assert_eq!(
        repository_store::load(&data).expect("saved list"),
        report.repositories
    );
}

/// 추가할 항목이 없으면 앱 설정 폴더나 빈 저장소 목록 파일을 만들지 않는다.
#[test]
fn empty_or_rejected_batch_does_not_create_storage() {
    use git_cleanup_tray::app::registration::add_repositories;

    let sandbox = Sandbox::new();
    let data = sandbox.root.join("만들지 않을 설정 폴더");
    let empty = add_repositories(&data, &[], &mut |_| {}).expect("empty batch");
    assert_eq!((empty.added, empty.duplicates, empty.failed), (0, 0, 0));
    assert!(!data.exists());

    let report = add_repositories(&data, &[sandbox.root.join("없는 저장소")], &mut |_| {})
        .expect("rejected batch");
    assert_eq!((report.added, report.duplicates, report.failed), (0, 0, 1));
    assert!(!data.exists());
}

/// 손상된 설정을 기본값으로 대신하지 않고 기존 등록 목록을 그대로 유지한다.
#[test]
fn batch_registration_preserves_storage_when_settings_are_invalid() {
    use git_cleanup_tray::app::registration::add_repositories;
    use git_cleanup_tray::storage::{repository_store, settings_store};

    let sandbox = Sandbox::new();
    let data = sandbox.root.join("손상된 앱 설정");
    repository_store::save(&data, &[]).expect("save empty list");
    let list = data.join(repository_store::FILE_NAME);
    let before = std::fs::read(&list).expect("read original list");
    std::fs::write(data.join(settings_store::FILE_NAME), "{").expect("write broken settings");

    let result = add_repositories(&data, std::slice::from_ref(&sandbox.root), &mut |_| {});
    assert!(matches!(
        result,
        Err(CleanupError::InvalidConfigFile { .. })
    ));
    assert_eq!(std::fs::read(list).expect("read retained list"), before);
}
