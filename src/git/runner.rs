// SPDX-License-Identifier: GPL-3.0-only

//! Git process 실행 abstraction (PLAN 18장).
//!
//! shell command string을 만들지 않고 executable과 argument를 분리해 넘긴다. 공백·한글 path,
//! shell metacharacter, OS별 quoting 차이를 신경 쓰지 않아도 된다.

use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{CleanupError, Result};
use crate::git::guard;

/// Git 명령 한 번의 실행 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    /// process exit code. signal로 종료되면 `None`.
    pub exit_code: Option<i32>,
    /// stdout 원본 byte. path가 UTF-8이 아닐 수 있어 `String`으로 강제 변환하지 않는다.
    pub stdout: Vec<u8>,
    /// stderr 원본 byte.
    pub stderr: Vec<u8>,
}

impl GitOutput {
    /// exit code가 0이면 `true`.
    #[must_use]
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// stdout을 UTF-8로 해석한다. 잘못된 byte는 U+FFFD로 바꾸며, 유효하면 복사하지 않는다.
    #[must_use]
    pub fn stdout_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.stdout)
    }

    /// stderr를 UTF-8로 해석한다. 잘못된 byte는 U+FFFD로 바꾼다.
    #[must_use]
    pub fn stderr_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.stderr)
    }

    /// 실패한 실행 결과를 [`CleanupError::GitCommandFailed`]로 바꾼다.
    ///
    /// `command`에는 subcommand 이름만 넘긴다. 전체 인자는 remote URL 등이 들어갈 수 있어
    /// 오류 메시지에 넣지 않는다 (PLAN 21장).
    #[must_use]
    pub fn failure(&self, command: &str) -> CleanupError {
        CleanupError::GitCommandFailed {
            command: command.to_owned(),
            exit_code: self.exit_code,
            stderr: self.stderr_lossy().trim_end().to_owned(),
        }
    }
}

/// Git 명령을 실행하는 interface.
///
/// 실제 구현([`ProcessGitRunner`])과 test용 recording/mock 구현을 바꿔 끼울 수 있게 trait로 둔다.
/// 구현체는 실행 전에 반드시 [`guard::check_forbidden`]을 호출해야 한다.
pub trait GitRunner: Send + Sync {
    /// 작업 스레드에서 사용할 읽기 전용 저장소 상태를 연다. 기본 구현은 기존 Git 명령을 사용한다.
    ///
    /// # Errors
    /// 라이브러리 구현에서는 저장소 접근·설정·형식 오류를 반환할 수 있다.
    fn open_read<'a>(
        &'a self,
        repository: &'a Path,
    ) -> Result<Box<dyn super::read::GitReadRepository + 'a>> {
        Ok(Box::new(super::read::CommandRead::new(self, repository)))
    }

    /// `repository` 경로에서 `git <args>`를 실행하고 결과를 그대로 돌려준다.
    ///
    /// exit code가 0이 아니어도 `Ok`를 반환한다. 성공 여부 판단은 호출자가 한다
    /// (`merge-base --is-ancestor`처럼 exit code 1이 정상 결과인 명령이 있기 때문이다).
    ///
    /// # Errors
    ///
    /// guard가 차단한 명령이면 [`CleanupError::ForbiddenCommand`], `git` 실행 파일이 없으면
    /// [`CleanupError::GitNotFound`], 그 밖의 process 생성 실패는 해당 I/O 오류를 반환한다.
    fn run(&self, repository: &Path, args: &[&OsStr]) -> Result<GitOutput>;
}

/// 설치된 `git` 실행 파일을 child process로 실행하는 기본 구현.
#[derive(Debug, Clone)]
pub struct ProcessGitRunner {
    /// 실행할 git 경로. 설정에서 Custom을 고르지 않으면 `git` (PATH 검색).
    executable: PathBuf,
}

impl ProcessGitRunner {
    /// PATH에서 `git`을 찾는 runner를 만든다.
    #[must_use]
    pub fn new() -> Self {
        Self::with_executable("git")
    }

    /// 지정한 git 실행 파일을 쓰는 runner를 만든다 (Settings의 Custom git path).
    #[must_use]
    pub fn with_executable(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }
}

impl Default for ProcessGitRunner {
    /// [`ProcessGitRunner::new`]와 같다.
    fn default() -> Self {
        Self::new()
    }
}

impl GitRunner for ProcessGitRunner {
    /// guard 검사 후 `git -C <repository> <args>`를 실행하고 종료까지 기다린다.
    fn run(&self, repository: &Path, args: &[&OsStr]) -> Result<GitOutput> {
        guard::check_forbidden(args)?;

        let mut command = Command::new(&self.executable);
        // current_dir 대신 `-C`를 쓴다. 저장소 경로가 사라졌을 때 spawn 오류(NotFound)가 나면
        // git 미설치와 구분할 수 없지만, `-C`면 git이 자체 오류 메시지로 알려준다.
        command
            .arg("-C")
            .arg(repository)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // stderr 분류를 위해 메시지 언어를 고정한다.
            .env("LC_ALL", "C")
            // credential prompt로 background 작업이 멈추지 않게 한다.
            .env("GIT_TERMINAL_PROMPT", "0")
            // `git status`가 index.lock을 잡지 않게 해 사용자의 Git 작업과 충돌을 줄인다.
            .env("GIT_OPTIONAL_LOCKS", "0");
        crate::platform::configure_background_command(&mut command);

        log::debug!("git {}", describe_subcommand(args));
        let output = command.output().map_err(|error| match error.kind() {
            ErrorKind::NotFound => CleanupError::GitNotFound,
            ErrorKind::PermissionDenied => {
                CleanupError::PermissionDenied(self.executable.display().to_string())
            }
            _ => CleanupError::Io(error),
        })?;

        Ok(GitOutput {
            exit_code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

/// 로그용으로 subcommand 이름만 뽑는다.
///
/// 전체 인자에는 credential이 포함된 remote URL이 들어갈 수 있어 (PLAN 21장) 로그에 남기지 않는다.
#[must_use]
pub fn describe_subcommand(args: &[&OsStr]) -> String {
    guard::subcommand(args).map_or_else(
        || String::from("<none>"),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// `&[OsString]`을 [`GitRunner::run`]이 받는 `Vec<&OsStr>`로 바꾸는 helper.
#[must_use]
pub fn as_os_args(args: &[OsString]) -> Vec<&OsStr> {
    args.iter().map(OsString::as_os_str).collect()
}
