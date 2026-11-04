// SPDX-License-Identifier: GPL-3.0-only

//! 정리 실행 결과 (PLAN 16장).

use std::path::PathBuf;

use crate::cleanup::candidate::SkippedItem;
use crate::error::CleanupError;
use crate::git::repository::RepositoryId;

/// 저장소 하나에 대한 Execute 결과.
#[derive(Debug, Default)]
pub struct CleanupResult {
    /// 대상 저장소. 결과를 만들기 시작할 때 채운다.
    pub repository_id: Option<RepositoryId>,
    /// 제거한 worktree 경로.
    pub removed_worktrees: Vec<PathBuf>,
    /// 삭제한 local branch 이름.
    pub removed_branches: Vec<String>,
    /// 삭제한 custom ref.
    pub removed_custom_refs: Vec<String>,
    /// 재검증 등에서 건너뛴 항목.
    pub skipped: Vec<SkippedItem>,
    /// 항목별로 발생한 오류. 한 항목이 실패해도 나머지는 계속 처리한다.
    pub errors: Vec<CleanupError>,
}
