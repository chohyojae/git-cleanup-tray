// SPDX-License-Identifier: GPL-3.0-only

//! 저장소 식별 (PLAN 6장).
//!
//! 같은 저장소의 서로 다른 worktree 경로를 중복 등록하지 않도록 common Git directory를
//! 식별 기준으로 쓴다. 선택한 조회 방식으로 [`RepositoryIdentity`]를 채운다.

use std::path::{Path, PathBuf};

use crate::error::{CleanupError, Result};
use crate::git::runner::GitRunner;
use crate::platform;

/// 읽기 전용 조회로 확인한 저장소 경로 정보.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryIdentity {
    /// 작업 tree 최상위 경로 (`--show-toplevel`).
    pub toplevel: PathBuf,
    /// 현재 worktree의 Git directory (`--git-dir`).
    pub git_dir: PathBuf,
    /// 모든 worktree가 공유하는 Git directory (`--git-common-dir`). 중복 판별 기준이다.
    pub common_dir: PathBuf,
}

/// 앱이 등록 저장소마다 부여하는 식별자. 저장소 경로가 바뀌어도 유지된다.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct RepositoryId(pub u64);

/// `path`가 Git 저장소의 작업 tree인지 확인하고 경로 정보를 돌려준다.
///
/// 세 값을 process 한 번으로 받기 위해 option을 한 번에 넘긴다. `--path-format=absolute`
/// (Git 2.31+)로 상대 경로가 섞이지 않게 한다. 출력이 줄 단위라 줄바꿈이 들어간 path는
/// 지원하지 않는다 (`rev-parse`에는 `-z`가 없다).
///
/// # Errors
///
/// - 경로가 없거나 directory가 아니면 [`CleanupError::RepositoryNotAccessible`]
/// - Git 저장소가 아니거나 bare 저장소면 [`CleanupError::NotARepository`]
/// - 그 밖의 Git 실패는 [`CleanupError::GitCommandFailed`]
pub(crate) fn identify_process<R: GitRunner + ?Sized>(
    runner: &R,
    path: &Path,
) -> Result<RepositoryIdentity> {
    // `-C`가 실패하면 Git 오류 문구로만 구분해야 하므로, 흔한 경우는 미리 확인한다.
    if !path.is_dir() {
        return Err(CleanupError::RepositoryNotAccessible(path.to_path_buf()));
    }

    let output = runner.run(
        path,
        &[
            "rev-parse".as_ref(),
            "--path-format=absolute".as_ref(),
            "--show-toplevel".as_ref(),
            "--git-dir".as_ref(),
            "--git-common-dir".as_ref(),
        ],
    )?;
    if !output.success() {
        let stderr = output.stderr_lossy();
        // bare 저장소는 `--show-toplevel`에서 "must be run in a work tree"로 실패한다.
        // worktree 정리 앱이므로 작업 tree가 없는 저장소는 지원하지 않는다.
        if stderr.contains("not a git repository") || stderr.contains("must be run in a work tree")
        {
            return Err(CleanupError::NotARepository(path.to_path_buf()));
        }
        return Err(output.failure("rev-parse"));
    }

    let mut lines = output
        .stdout
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
        .map(platform::path_from_git_bytes);
    match (lines.next(), lines.next(), lines.next()) {
        (Some(toplevel), Some(git_dir), Some(common_dir)) => Ok(RepositoryIdentity {
            toplevel,
            git_dir,
            common_dir,
        }),
        // 성공했는데 세 줄이 아니면 Git 동작이 예상과 다른 것이다. stdout을 stderr 자리에 담아
        // 상세 화면에서 원인을 볼 수 있게 한다.
        _ => Err(CleanupError::GitCommandFailed {
            command: String::from("rev-parse"),
            exit_code: output.exit_code,
            stderr: output.stdout_lossy().into_owned(),
        }),
    }
}

/// 작업 트리와 공통 Git 디렉터리를 식별한다. bare 저장소는 거부한다.
///
/// # Errors
/// 저장소 접근과 해당 조회 작업의 오류를 반환한다.
pub fn identify<R: GitRunner + ?Sized>(
    runner: &R,
    repository: &Path,
) -> Result<RepositoryIdentity> {
    runner.open_read(repository)?.identity()
}
