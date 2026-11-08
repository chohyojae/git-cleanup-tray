// SPDX-License-Identifier: GPL-3.0-only

//! `git worktree list --porcelain -z` 출력 parsing과 worktree 상태 확인 (PLAN 8.2장).
//!
//! 공백·한글·줄바꿈 path를 안전하게 다루기 위해 `-z`(NUL 구분, Git 2.36+) 출력을 parsing한다.
//! dirty 판정은 `git -C <worktree> status --porcelain --untracked-files=normal` 출력이
//! 하나라도 있으면 dirty로 본다.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::git::object::ObjectId;
use crate::git::runner::GitRunner;
use crate::platform;

/// porcelain 출력의 worktree 한 항목.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    /// worktree 경로.
    pub path: PathBuf,
    /// checkout된 commit (`HEAD <oid>`). bare 항목이나 아직 commit이 없는 저장소면 `None`.
    pub head: Option<ObjectId>,
    /// 연결된 branch의 full ref (`branch refs/heads/...`). detached면 `None`.
    pub branch: Option<String>,
    /// main worktree(첫 항목)이면 `true`. 절대 삭제하지 않는다.
    pub is_main: bool,
    /// bare 저장소 항목이면 `true`.
    pub is_bare: bool,
    /// lock 사유. lock되어 있지 않으면 `None`, 사유 없이 lock되면 빈 문자열.
    pub locked: Option<String>,
    /// prune 가능 상태(경로가 사라짐)면 그 사유.
    pub prunable: Option<String>,
}

/// 저장소의 worktree 목록을 읽는다. 첫 항목은 항상 main worktree다.
///
/// # Errors
///
/// Git 실행이 실패하면 [`crate::error::CleanupError::GitCommandFailed`].
pub(crate) fn list_process<R: GitRunner + ?Sized>(
    runner: &R,
    repository: &Path,
) -> Result<Vec<WorktreeEntry>> {
    let output = runner.run(
        repository,
        &[
            "worktree".as_ref(),
            "list".as_ref(),
            "--porcelain".as_ref(),
            "-z".as_ref(),
        ],
    )?;
    if !output.success() {
        return Err(output.failure("worktree"));
    }
    Ok(parse_porcelain(&output.stdout))
}

/// `-z` porcelain 출력을 parsing한다.
///
/// 각 attribute는 NUL로 끝나고, 항목 사이에는 빈 attribute(연속 NUL)가 들어간다.
/// 알 수 없는 attribute는 무시한다 (이후 Git 버전에서 추가될 수 있다).
fn parse_porcelain(stdout: &[u8]) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut current: Option<WorktreeEntry> = None;

    for field in stdout.split(|&b| b == 0) {
        if field.is_empty() {
            // 항목 구분자. 마지막 NUL 뒤의 빈 조각도 여기로 온다.
            entries.extend(current.take());
            continue;
        }
        // attribute 이름과 값은 첫 공백으로 나뉜다. 값이 없는 attribute(`bare`, `detached`)도 있다.
        let (key, value) = match field.iter().position(|&b| b == b' ') {
            Some(i) => (&field[..i], Some(&field[i + 1..])),
            None => (field, None),
        };

        if key == b"worktree" {
            entries.extend(current.take());
            current = value.map(|path| WorktreeEntry {
                path: platform::path_from_git_bytes(path),
                head: None,
                branch: None,
                is_main: entries.is_empty(),
                is_bare: false,
                locked: None,
                prunable: None,
            });
            continue;
        }
        let Some(entry) = current.as_mut() else {
            // `worktree` 줄 없이 attribute가 오면 형식이 깨진 것이다. 해당 attribute는 버린다.
            continue;
        };
        let text = || value.map_or_else(String::new, |v| String::from_utf8_lossy(v).into_owned());
        match key {
            b"HEAD" => {
                entry.head = value.and_then(|v| ObjectId::parse(&String::from_utf8_lossy(v)));
            }
            b"branch" => entry.branch = Some(text()),
            b"bare" => entry.is_bare = true,
            b"locked" => entry.locked = Some(text()),
            b"prunable" => entry.prunable = Some(text()),
            _ => {}
        }
    }
    entries.extend(current);
    entries
}

/// worktree에 staged/modified/untracked 변경이 하나도 없으면 `true`.
///
/// `--untracked-files=normal`은 untracked directory를 하나의 항목으로만 보여줘 큰 build 폴더가
/// 있어도 빠르다. ignore된 파일은 사용자 작업물이 아니므로 dirty로 보지 않는다 (Git 기본 동작).
///
/// # Errors
///
/// 경로가 없거나 Git 실행이 실패하면 [`crate::error::CleanupError::GitCommandFailed`].
/// 상태를 알 수 없으면 clean으로 취급하지 않도록 호출자는 오류를 "삭제하지 않음"으로 처리해야 한다.
pub(crate) fn is_clean_process<R: GitRunner + ?Sized>(runner: &R, worktree: &Path) -> Result<bool> {
    let output = runner.run(
        worktree,
        &[
            "status".as_ref(),
            "--porcelain".as_ref(),
            "--untracked-files=normal".as_ref(),
        ],
    )?;
    if !output.success() {
        return Err(output.failure("status"));
    }
    Ok(output.stdout.is_empty())
}

/// 메인·연결 워크트리와 잠금·경로 누락 상태를 읽는다.
///
/// # Errors
/// 저장소 접근과 해당 조회 작업의 오류를 반환한다.
pub fn list<R: GitRunner + ?Sized>(runner: &R, repository: &Path) -> Result<Vec<WorktreeEntry>> {
    runner.open_read(repository)?.worktrees()
}

/// 스테이징·작업 파일·추적하지 않는 파일 변경이 없는지 저장소 전체를 검사한다.
///
/// # Errors
/// 저장소 접근과 해당 조회 작업의 오류를 반환한다.
pub fn is_clean<R: GitRunner + ?Sized>(runner: &R, repository: &Path) -> Result<bool> {
    runner.open_read(repository)?.is_clean()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 실제 Git 2.54 출력 형태로 main, locked, detached, prunable 항목을 읽는다.
    #[test]
    fn parses_porcelain_z_output() {
        let oid = "e6ec21a92ade4f3a94039927941b1c3c3929b930";
        let stdout = format!(
            "worktree /repo\0HEAD {oid}\0branch refs/heads/main\0\0\
             worktree /w t/한글\0HEAD {oid}\0branch refs/heads/기능\0locked x y\0\0\
             worktree /detached\0HEAD {oid}\0detached\0locked\0\0\
             worktree /gone\0HEAD {oid}\0branch refs/heads/gone\0prunable gitdir file points to non-existent location\0\0"
        );
        let entries = parse_porcelain(stdout.as_bytes());
        assert_eq!(entries.len(), 4);

        assert!(entries[0].is_main);
        assert_eq!(entries[0].path, PathBuf::from("/repo"));
        assert_eq!(entries[0].branch.as_deref(), Some("refs/heads/main"));
        assert_eq!(entries[0].head.as_ref().map(ObjectId::as_str), Some(oid));

        assert!(!entries[1].is_main);
        assert_eq!(entries[1].path, PathBuf::from("/w t/한글"));
        assert_eq!(entries[1].locked.as_deref(), Some("x y"));

        assert_eq!(entries[2].branch, None);
        assert_eq!(entries[2].locked.as_deref(), Some(""));

        assert!(entries[3].prunable.is_some());
        assert_eq!(entries[3].locked, None);
    }

    /// bare 저장소 항목과 끝 NUL이 빠진 출력도 처리한다.
    #[test]
    fn parses_bare_and_unterminated_output() {
        let entries = parse_porcelain(b"worktree /bare.git\0bare\0\0worktree /wt\0detached");
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_bare && entries[0].is_main);
        assert!(!entries[1].is_main);
        assert_eq!(parse_porcelain(b""), Vec::new());
    }
}
