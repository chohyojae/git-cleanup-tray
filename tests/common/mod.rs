// SPDX-License-Identifier: GPL-3.0-only

//! integration test 공용 fixture.
//!
//! 실제 임시 Git 저장소를 만든다. fixture 구성용 Git 명령은 사용자의 전역/시스템 설정을 읽지 않도록
//! `GIT_CONFIG_GLOBAL`(빈 파일)과 `GIT_CONFIG_NOSYSTEM`으로 격리한다. fixture 구성은 검증 대상이
//! 아니므로 `GitRunner`를 거치지 않고 `Command`로 직접 실행한다 (guard가 막는 `branch -D` 등도
//! fixture에서는 필요하다).

// test 파일마다 쓰는 helper가 달라, 각 test crate에서 안 쓰는 함수가 dead code 경고를 낸다.
#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::Stdio;

use git_cleanup_tray::cleanup::planner::ScanRequest;
use git_cleanup_tray::git::default_branch::BaseBranchQuery;
use git_cleanup_tray::git::repository::RepositoryId;

/// 같은 안전 테스트를 Git 명령과 라이브러리로 각각 실행한다. 기본은 기존 Git 명령이다.
#[must_use]
pub fn test_runner() -> git_cleanup_tray::git::in_process::InProcessGitRunner {
    use git_cleanup_tray::git::in_process::InProcessGitRunner;
    use git_cleanup_tray::git::runner::ProcessGitRunner;
    if std::env::var_os("GIT_CLEANUP_TEST_READS").is_some_and(|value| value == "gix") {
        InProcessGitRunner::default()
    } else {
        InProcessGitRunner::commands_only(ProcessGitRunner::new())
    }
}

/// test 하나가 쓰는 임시 directory. drop될 때 통째로 지워진다.
pub struct Sandbox {
    /// 임시 directory handle. drop 시 삭제되도록 들고만 있는다.
    _dir: tempfile::TempDir,
    /// fixture를 만드는 기준 경로. 공백·한글을 일부러 넣는다 (AGENTS 7장).
    pub root: PathBuf,
    /// fixture Git 명령에 쓰는 빈 전역 설정 파일.
    config: PathBuf,
}

impl Sandbox {
    /// 대량 fixture 입력을 표준 입력으로 전달하고 사용자 Git 설정과 격리한다.
    ///
    /// # Panics
    /// Git 실행·입력 쓰기·종료에 실패하면 fixture 구성 실패로 중단한다.
    pub fn git_input(&self, dir: &Path, args: &[&str], input: &[u8]) -> String {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", &self.config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
            .env("LC_ALL", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start fixture git");
        child
            .stdin
            .take()
            .expect("fixture stdin")
            .write_all(input)
            .expect("write fixture input");
        let output = child.wait_with_output().expect("wait fixture git");
        assert!(
            output.status.success(),
            "fixture git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("UTF-8 fixture output")
    }

    /// 공백과 한글이 들어간 기준 경로를 가진 sandbox를 만든다.
    ///
    /// # Panics
    ///
    /// 임시 directory를 만들지 못하면 panic한다.
    #[must_use]
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("create temp dir");
        // macOS의 `/var` 별칭은 해소하되 Windows에서 `canonicalize()`가 붙이는
        // `\\?\` 접두사는 피한다. Git for Windows의 `clone --bare`가 이 경로에서 실패한다.
        let root = gix::path::realpath(dir.path())
            .expect("physical temporary path")
            .join("한글 sandbox 경로");
        std::fs::create_dir(&root).expect("create sandbox root");
        let config = dir.path().join("empty-gitconfig");
        std::fs::write(&config, "").expect("create empty gitconfig");
        Self {
            _dir: dir,
            root,
            config,
        }
    }

    /// 격리된 설정으로 `git -C <dir> <args>`를 실행하고 stdout을 돌려준다.
    ///
    /// # Panics
    ///
    /// git이 실패하면 stderr와 함께 panic한다. fixture 구성 실패는 test 전제가 깨진 것이기 때문이다.
    pub fn git(&self, dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", &self.config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
            .env("LC_ALL", "C")
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("utf-8 git output")
    }

    /// `history.txt`에 한 줄을 더해 commit한다. 매번 다른 commit이 생긴다.
    ///
    /// # Panics
    ///
    /// 파일 쓰기나 git이 실패하면 panic한다.
    pub fn commit(&self, dir: &Path, message: &str) {
        let file = dir.join("history.txt");
        let mut text = std::fs::read_to_string(&file).unwrap_or_default();
        text.push_str(message);
        text.push('\n');
        std::fs::write(&file, text).expect("write history.txt");
        self.git(dir, &["add", "history.txt"]);
        self.git(dir, &["commit", "-q", "-m", message]);
    }

    /// default branch가 `default_branch`인 remote를 만들고 그것을 clone한 작업 저장소를 돌려준다.
    ///
    /// clone이 `refs/remotes/origin/HEAD`를 만들어 주므로 network 없이 default branch를 판별할 수 있다.
    ///
    /// # Panics
    ///
    /// git이 실패하면 panic한다.
    pub fn clone_with_default(&self, default_branch: &str, name: &str) -> PathBuf {
        let seed = self.root.join(format!("seed {name}"));
        let origin = self.root.join(format!("origin {name}.git"));
        let work = self.root.join(name);
        std::fs::create_dir(&seed).expect("create seed dir");

        self.git(&seed, &["init", "-q", "-b", default_branch]);
        self.commit(&seed, "initial");
        self.git(
            &self.root,
            &["clone", "-q", "--bare", path_str(&seed), path_str(&origin)],
        );
        self.git(
            &self.root,
            &["clone", "-q", path_str(&origin), path_str(&work)],
        );
        work
    }

    /// `branch`를 checkout한 linked worktree를 sandbox 아래 `name`에 만든다.
    ///
    /// # Panics
    ///
    /// git이 실패하면 panic한다.
    pub fn add_worktree(&self, repo: &Path, name: &str, branch: &str) -> PathBuf {
        let path = self.root.join(name);
        self.git(repo, &["worktree", "add", "-q", path_str(&path), branch]);
        path
    }

    /// full ref가 존재하면 `true`.
    ///
    /// # Panics
    ///
    /// git 실행 자체가 실패하면 panic한다.
    #[must_use]
    pub fn ref_exists(&self, repo: &Path, full_ref: &str) -> bool {
        let listed = self.git(repo, &["for-each-ref", "--format=%(refname)", full_ref]);
        listed.lines().any(|line| line == full_ref)
    }
}

/// test 경로를 `&str`로 바꾼다. fixture 경로는 모두 UTF-8이다.
///
/// # Panics
///
/// 경로가 UTF-8이 아니면 panic한다.
#[must_use]
pub fn path_str(path: &Path) -> &str {
    path.to_str().expect("utf-8 fixture path")
}

/// 기본값(remote `origin`, 자동 판별, remote 질의 허용)으로 Scan 요청을 만든다.
#[must_use]
pub fn request<'a>(
    path: &'a Path,
    custom_ref_namespaces: &'a [String],
    protected_branch_patterns: &'a [String],
) -> ScanRequest<'a> {
    ScanRequest {
        repository_id: RepositoryId(1),
        path,
        base: BaseBranchQuery {
            remote: "origin",
            override_branch: None,
            allow_remote_query: true,
        },
        custom_ref_namespaces,
        protected_branch_patterns,
    }
}

/// 기본 custom ref namespace 목록 (`refs/remote/`).
#[must_use]
pub fn default_namespaces() -> Vec<String> {
    vec![String::from("refs/remote/")]
}

/// PLAN 26장 필수 케이스를 한 저장소에 모은 fixture.
pub struct Scenario {
    /// main worktree (default branch `main`, local branch 있음).
    pub work: PathBuf,
    /// merge된 branch의 clean linked worktree. 제거되어야 한다.
    pub wt_clean: PathBuf,
    /// tracked 파일을 수정한 worktree. 남아야 한다.
    pub wt_dirty: PathBuf,
    /// staged 변경이 있는 worktree. 남아야 한다.
    pub wt_staged: PathBuf,
    /// untracked 파일이 있는 worktree. 남아야 한다.
    pub wt_untracked: PathBuf,
    /// lock된 worktree. 남아야 한다.
    pub wt_locked: PathBuf,
}

impl Sandbox {
    /// [`Scenario`] 저장소를 만든다.
    ///
    /// - merge됨: `기능/병합됨`, `release/1.0`(보호 pattern 대상), `wt-*` branch, `refs/remote/agent/merged`
    /// - merge 안 됨: `unmerged`, `refs/remote/agent/unmerged`
    /// - 절대 건드리면 안 됨: `refs/remotes/origin/*`, `refs/tags/v1`, `main`
    ///
    /// merge된 ref들은 첫 commit에 두고 `main`을 한 commit 앞으로 보내, "같은 commit"이 아니라
    /// "ancestor"인 경우를 검증한다.
    ///
    /// # Panics
    ///
    /// git이나 파일 쓰기가 실패하면 panic한다.
    #[must_use]
    pub fn scenario(&self) -> Scenario {
        let work = self.clone_with_default("main", "작업 저장소");
        for branch in [
            "기능/병합됨",
            "release/1.0",
            "wt-clean",
            "wt-dirty",
            "wt-staged",
            "wt-untracked",
            "wt-locked",
        ] {
            self.git(&work, &["branch", branch]);
        }
        self.git(&work, &["update-ref", "refs/remote/agent/merged", "HEAD"]);
        self.git(
            &work,
            &["update-ref", "refs/remotes/origin/old-feature", "HEAD"],
        );
        self.git(&work, &["tag", "v1"]);

        self.git(&work, &["checkout", "-q", "-b", "unmerged"]);
        self.commit(&work, "unmerged work");
        self.git(&work, &["checkout", "-q", "main"]);
        self.git(
            &work,
            &["update-ref", "refs/remote/agent/unmerged", "unmerged"],
        );
        self.commit(&work, "main moves on");

        let wt_clean = self.add_worktree(&work, "wt clean 깨끗함", "wt-clean");
        let wt_dirty = self.add_worktree(&work, "wt dirty", "wt-dirty");
        let wt_staged = self.add_worktree(&work, "wt staged", "wt-staged");
        let wt_untracked = self.add_worktree(&work, "wt untracked", "wt-untracked");
        let wt_locked = self.add_worktree(&work, "wt locked", "wt-locked");

        std::fs::write(wt_dirty.join("history.txt"), "changed\n").expect("modify tracked file");
        std::fs::write(wt_staged.join("staged.txt"), "staged\n").expect("write staged file");
        self.git(&wt_staged, &["add", "staged.txt"]);
        std::fs::write(wt_untracked.join("untracked.txt"), "new\n").expect("write untracked");
        self.git(
            &work,
            &[
                "worktree",
                "lock",
                "--reason",
                "agent 작업 중",
                path_str(&wt_locked),
            ],
        );

        Scenario {
            work,
            wt_clean,
            wt_dirty,
            wt_staged,
            wt_untracked,
            wt_locked,
        }
    }
}

impl Sandbox {
    /// `rev`가 가리키는 commit id.
    ///
    /// # Panics
    ///
    /// git이 실패하거나 출력이 object id가 아니면 panic한다.
    #[must_use]
    pub fn oid(&self, repo: &Path, rev: &str) -> git_cleanup_tray::git::object::ObjectId {
        let text = self.git(repo, &["rev-parse", "--verify", rev]);
        git_cleanup_tray::git::object::ObjectId::parse(text.trim_end()).expect("object id")
    }
}
