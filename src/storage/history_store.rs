// SPDX-License-Identifier: GPL-3.0-only

//! 실행 이력 (`history.json`) 저장과 로드.
//!
//! 파일과 메모리가 계속 커지지 않도록 최근 [`HISTORY_LIMIT`]건만 유지한다. 이력은 cleanup이 끝난
//! 뒤 한 번 읽고 쓰며, 앱이 메모리에 들고 있지 않는다.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::git::repository::RepositoryId;

/// 저장 파일 이름.
pub const FILE_NAME: &str = "history.json";

/// 보관할 최대 이력 수 (PLAN 20장).
pub const HISTORY_LIMIT: usize = 100;

/// 저장소 하나에 대한 cleanup 실행 기록.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// 실행이 끝난 시각 (UNIX epoch 초).
    pub finished_at: u64,
    /// 대상 저장소.
    pub repository_id: RepositoryId,
    /// 실행 당시 저장소 표시 이름. 나중에 등록을 지워도 이력을 읽을 수 있게 함께 남긴다.
    pub repository_name: String,
    /// 제거한 worktree 수.
    pub removed_worktrees: usize,
    /// 삭제한 local branch 수.
    pub removed_branches: usize,
    /// 삭제한 custom ref 수.
    pub removed_custom_refs: usize,
    /// 건너뛴 항목 수.
    pub skipped: usize,
    /// 오류 수.
    pub errors: usize,
}

/// `dir`의 `history.json`을 읽는다. 파일이 없으면 빈 목록이다.
///
/// # Errors
///
/// 파일을 읽지 못하거나 JSON이 깨졌으면 [`super::load_json`]의 오류.
pub fn load(dir: &Path) -> Result<Vec<HistoryEntry>> {
    super::load_json(&dir.join(FILE_NAME))
}

/// `entries`를 이력 끝에 붙이고 최근 [`HISTORY_LIMIT`]건만 남겨 저장한다.
///
/// 이력 파일이 깨져 있으면 새로 시작한다. 이력은 정리 동작에 영향을 주지 않는 기록이라, 깨진
/// 파일 때문에 새 기록까지 잃는 것보다 덮어쓰는 편이 낫다.
///
/// # Errors
///
/// 파일 읽기(손상 제외)나 저장에 실패하면 해당 오류.
pub fn append(dir: &Path, entries: &[HistoryEntry]) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut history = match load(dir) {
        Ok(history) => history,
        Err(crate::error::CleanupError::InvalidConfigFile { path, source }) => {
            log::warn!(
                "replacing corrupt history file {}: {source}",
                path.display()
            );
            Vec::new()
        }
        Err(error) => return Err(error),
    };
    history.extend_from_slice(entries);
    let excess = history.len().saturating_sub(HISTORY_LIMIT);
    history.drain(..excess);
    super::save_json(&dir.join(FILE_NAME), &history)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// test용 이력 항목.
    fn entry(finished_at: u64) -> HistoryEntry {
        HistoryEntry {
            finished_at,
            repository_id: RepositoryId(1),
            repository_name: String::from("저장소"),
            removed_worktrees: 0,
            removed_branches: 1,
            removed_custom_refs: 0,
            skipped: 0,
            errors: 0,
        }
    }

    /// 상한을 넘으면 오래된 항목부터 지우고, 깨진 파일은 새로 시작하는지 확인한다.
    #[test]
    fn append_keeps_latest_entries() {
        let dir = tempfile::tempdir().expect("temp dir");
        let first: Vec<_> = (0..90).map(entry).collect();
        append(dir.path(), &first).expect("append");
        let second: Vec<_> = (90..120).map(entry).collect();
        append(dir.path(), &second).expect("append");

        let history = load(dir.path()).expect("load");
        assert_eq!(history.len(), HISTORY_LIMIT);
        assert_eq!(history.first().map(|e| e.finished_at), Some(20));
        assert_eq!(history.last().map(|e| e.finished_at), Some(119));

        std::fs::write(dir.path().join(FILE_NAME), "{").expect("corrupt");
        append(dir.path(), &[entry(500)]).expect("append over corrupt file");
        assert_eq!(load(dir.path()).expect("load"), [entry(500)]);
    }
}
