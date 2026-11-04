// SPDX-License-Identifier: GPL-3.0-only

//! 주기 실행에 쓰는 worker의 최신화 → Scan → Clean과 설정·취소 처리를 검증한다.

mod common;

use common::{Sandbox, path_str};
use git_cleanup_tray::app::maintenance;
use git_cleanup_tray::app::tasks::ScanScope;
use git_cleanup_tray::git::repository::RepositoryId;
use git_cleanup_tray::jobs::cancellation::CancellationToken;
use git_cleanup_tray::storage::repository_store::RepositoryConfig;
use git_cleanup_tray::storage::settings_store::AppSettings;
use git_cleanup_tray::storage::{history_store, repository_store, settings_store};
use std::path::Path;

/// 임시 저장소를 앱 등록 목록 형식으로 만든다.
fn config(path: &Path, id: u64, enabled: bool) -> RepositoryConfig {
    RepositoryConfig {
        id: RepositoryId(id),
        name: format!("저장소 {id}"),
        path: path.to_path_buf(),
        common_dir: path.join(".git"),
        enabled,
        remote_name: String::from("origin"),
        base_branch_override: None,
        custom_ref_namespaces: vec![String::from("refs/remote/")],
    }
}

/// 원격에서 막 병합된 branch는 최신화 후 Scan해야만 정리 대상이 된다.
#[test]
fn refreshes_before_cleaning_and_records_history() {
    let sandbox = Sandbox::new();
    let work = sandbox.clone_with_default("trunk", "주기 작업");
    let seed = sandbox.root.join("seed 주기 작업");
    sandbox.git(
        &seed,
        &[
            "remote",
            "add",
            "origin",
            path_str(&sandbox.root.join("origin 주기 작업.git")),
        ],
    );
    sandbox.git(&seed, &["checkout", "-q", "-b", "feature"]);
    sandbox.commit(&seed, "feature work");
    sandbox.git(&seed, &["push", "-q", "origin", "feature"]);
    sandbox.git(&work, &["fetch", "origin"]);
    sandbox.git(&work, &["branch", "--track", "feature", "origin/feature"]);
    sandbox.git(&seed, &["checkout", "-q", "trunk"]);
    sandbox.git(&seed, &["merge", "--ff-only", "feature"]);
    sandbox.git(&seed, &["push", "-q", "origin", "trunk"]);
    let dir = sandbox.root.join("설정 폴더");
    repository_store::save(&dir, &[config(&work, 1, true)]).expect("save repos");
    let cancel = CancellationToken::new();
    let mut progress = Vec::new();
    let report = maintenance::run(&dir, ScanScope::Enabled, true, &cancel, &mut |text| {
        progress.push(text);
    })
    .expect("run");
    assert_eq!(report.errors, 0, "{:?}", report.problems);
    assert_eq!(report.updated, 1);
    assert_eq!(report.cleanup.expect("cleanup").totals.branches, 1);
    assert!(!sandbox.ref_exists(&work, "refs/heads/feature"));
    assert!(sandbox.ref_exists(&work, "refs/remotes/origin/feature"));
    assert_eq!(sandbox.oid(&work, "HEAD"), sandbox.oid(&seed, "trunk"));
    assert_eq!(history_store::load(&dir).expect("history").len(), 1);
    assert!(progress[0].starts_with("Fetching"));
    assert!(progress[1].starts_with("Scanning"));
    assert!(progress[2].starts_with("Cleaning"));
}

/// fetch 실패 저장소는 정리하지 않고 다음 저장소를 처리한다. disabled 저장소도 자동 실행에서 뺀다.
#[test]
fn isolates_fetch_failures_and_disabled_repositories() {
    let sandbox = Sandbox::new();
    let failed = sandbox.clone_with_default("trunk", "실패");
    let good = sandbox.clone_with_default("trunk", "성공");
    let disabled = sandbox.clone_with_default("trunk", "제외");
    for work in [&failed, &good, &disabled] {
        sandbox.git(work, &["branch", "merged"]);
    }
    sandbox.git(
        &failed,
        &[
            "remote",
            "set-url",
            "origin",
            path_str(&sandbox.root.join("absent")),
        ],
    );
    let dir = sandbox.root.join("설정 폴더");
    repository_store::save(
        &dir,
        &[
            config(&failed, 1, true),
            config(&good, 2, true),
            config(&disabled, 3, false),
        ],
    )
    .expect("save");
    let report = maintenance::run(
        &dir,
        ScanScope::Enabled,
        true,
        &CancellationToken::new(),
        &mut |_| {},
    )
    .expect("run");
    assert_eq!(report.errors, 1, "{:?}", report.problems);
    assert!(sandbox.ref_exists(&failed, "refs/heads/merged"));
    assert!(!sandbox.ref_exists(&good, "refs/heads/merged"));
    assert!(sandbox.ref_exists(&disabled, "refs/heads/merged"));
}

/// 수동 최신화는 자동 clean 설정과 무관하게 삭제하지 않는다. 단일 선택은 disabled여도 처리한다.
#[test]
fn manual_update_does_not_clean_and_honors_scope() {
    let sandbox = Sandbox::new();
    let work = sandbox.clone_with_default("trunk", "수동");
    sandbox.git(&work, &["branch", "merged"]);
    let dir = sandbox.root.join("설정 폴더");
    repository_store::save(&dir, &[config(&work, 1, false)]).expect("save");
    let report = maintenance::run(
        &dir,
        ScanScope::Only(RepositoryId(1)),
        false,
        &CancellationToken::new(),
        &mut |_| {},
    )
    .expect("run");
    assert_eq!(report.summaries.len(), 1);
    assert!(report.cleanup.is_none());
    assert!(sandbox.ref_exists(&work, "refs/heads/merged"));
    assert_eq!(history_store::load(&dir).expect("history"), []);
}

/// 손상된 설정에서는 Git을 실행하지 않고, 갱신 직후 취소되면 clean을 시작하지 않는다.
#[test]
fn invalid_settings_and_cancellation_prevent_cleanup() {
    let sandbox = Sandbox::new();
    let work = sandbox.clone_with_default("trunk", "취소");
    sandbox.git(&work, &["branch", "merged"]);
    let dir = sandbox.root.join("설정 폴더");
    repository_store::save(&dir, &[config(&work, 1, true)]).expect("save");
    std::fs::write(dir.join("config.json"), "{broken").expect("corrupt");
    assert!(
        maintenance::run(
            &dir,
            ScanScope::Enabled,
            true,
            &CancellationToken::new(),
            &mut |_| panic!("no Git work")
        )
        .is_err()
    );
    settings_store::save(&dir, &AppSettings::default()).expect("restore");
    let cancel = CancellationToken::new();
    let report = maintenance::run(&dir, ScanScope::Enabled, true, &cancel, &mut |_| {
        cancel.cancel();
    })
    .expect("run");
    assert!(report.cancelled);
    assert!(report.cleanup.is_none());
    assert!(sandbox.ref_exists(&work, "refs/heads/merged"));
}
