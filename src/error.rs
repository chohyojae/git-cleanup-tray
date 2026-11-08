// SPDX-License-Identifier: GPL-3.0-only

//! 애플리케이션 전체에서 쓰는 오류 category (PLAN 25장).
//!
//! 오류를 문자열 하나로 뭉뚱그리지 않고 종류별 variant로 나눠, UI에는 읽기 쉬운 메시지를
//! 보여주고 상세 화면에서는 Git stderr와 exit code를 확인할 수 있게 한다.

use std::path::PathBuf;

/// Git 실행·저장소 검사·cleanup 과정에서 발생하는 오류.
#[derive(Debug, thiserror::Error)]
pub enum CleanupError {
    /// `git` 실행 파일을 찾지 못했다.
    #[error("git executable was not found")]
    GitNotFound,

    /// 라이브러리로 저장소를 읽지 못했다. 종료 코드나 Git stderr로 위장하지 않는다.
    #[error("repository read failed during {operation}")]
    RepositoryReadFailed {
        /// 실패한 조회 작업. 로그에 설정 값이나 원격 URL을 포함하지 않는다.
        operation: &'static str,
        /// 상세 원인. 일반 로그에는 출력하지 않고 오류 연결 관계를 유지한다.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// 경로가 Git 저장소가 아니다.
    #[error("not a git repository: {}", .0.display())]
    NotARepository(PathBuf),

    /// 저장소 경로에 접근할 수 없다 (삭제, 권한, 네트워크 drive 해제 등).
    #[error("repository is not accessible: {}", .0.display())]
    RepositoryNotAccessible(PathBuf),

    /// 설정된 remote가 저장소에 없다.
    #[error("remote not found: {0}")]
    RemoteNotFound(String),

    /// remote default branch를 판별하지 못했다. `git fetch`가 필요할 수 있다.
    #[error("default branch is unknown for remote {0}")]
    DefaultBranchUnknown(String),

    /// remote 질의 중 인증에 실패했다.
    #[error("authentication failed")]
    AuthenticationFailed,

    /// remote 질의 중 network에 연결할 수 없다.
    #[error("network is unavailable")]
    NetworkUnavailable,

    /// worktree에 staged/modified/untracked 변경이 있다.
    #[error("worktree has uncommitted or untracked changes: {}", .0.display())]
    DirtyWorktree(PathBuf),

    /// worktree가 lock 상태다.
    #[error("worktree is locked: {}", .0.display())]
    LockedWorktree(PathBuf),

    /// Git 명령이 0이 아닌 exit code로 끝났다.
    #[error("git {command} failed with exit code {exit_code:?}: {stderr}")]
    GitCommandFailed {
        /// 실행한 git subcommand 이름 (예: `branch`). 전체 인자는 로그에만 남긴다.
        command: String,
        /// process exit code. signal로 종료되면 `None`.
        exit_code: Option<i32>,
        /// Git이 출력한 stderr. 상세 화면에 그대로 보여준다.
        stderr: String,
    },

    /// 파일 또는 process 실행 권한이 없다.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// Scan 이후 Execute 직전 재검증에서 상태가 바뀐 것을 발견했다.
    #[error("state changed since scan: {0}")]
    StateChanged(String),

    /// 앱 설정 파일(`config.json`, `repositories.json`)을 해석하지 못했다.
    ///
    /// 손상된 설정을 기본값으로 대체하면 보호 branch pattern 같은 사용자 설정이 사라진 채 정리가
    /// 실행될 수 있으므로, 이 오류가 나면 작업을 시작하지 않는다.
    #[error("invalid settings file {}: {source}", .path.display())]
    InvalidConfigFile {
        /// 문제가 된 파일 경로.
        path: PathBuf,
        /// JSON 해석 오류. 줄·열 위치를 담고 있다.
        #[source]
        source: serde_json::Error,
    },

    /// 안전 불변조건(PLAN 24장)에 어긋나는 Git 명령 실행을 차단했다.
    #[error("forbidden git command was blocked: {0}")]
    ForbiddenCommand(String),

    /// 위 category에 속하지 않는 I/O 오류.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// crate 전체에서 쓰는 `Result` alias.
pub type Result<T, E = CleanupError> = std::result::Result<T, E>;
