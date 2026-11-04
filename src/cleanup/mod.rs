// SPDX-License-Identifier: GPL-3.0-only

//! 검사(Scan)와 실행(Execute) 분리 (PLAN 9장).
//!
//! ```text
//! RepositoryInspector → CleanupPlanner → CleanupPlan → CleanupExecutor
//! ```
//!
//! planner는 mutation 없이 [`CleanupPlan`]만 만든다. executor는 plan을 그대로 믿지 않고
//! 대상마다 삭제 직전에 상태를 다시 확인하며, 조건이 바뀌면 삭제하지 않고 `Skipped`로 기록한다.

pub mod candidate;
pub mod executor;
pub mod planner;
pub mod result;

use std::path::PathBuf;

use crate::error::CleanupError;
use crate::git::default_branch::ResolvedBase;
use crate::git::repository::RepositoryId;

use candidate::{BranchCandidate, CustomRefCandidate, SkippedItem, WorktreeCandidate};

/// 저장소 하나에 대한 정리 계획. Scan 결과이며 아무것도 변경하지 않은 상태다.
///
/// `CleanupError`가 `Clone`이 아니어서 plan도 `Clone`을 구현하지 않는다. plan은 Scan 한 번에
/// 하나 만들어 executor에 빌려주는 일회성 값이므로 복사할 일이 없다.
#[derive(Debug)]
pub struct CleanupPlan {
    /// 대상 저장소.
    pub repository_id: RepositoryId,
    /// Git 명령의 `-C`에 넘기는 등록 경로.
    pub repository_path: PathBuf,
    /// 등록 경로의 작업 tree 최상위 경로. 이 worktree는 제거 대상에서 뺀다.
    pub toplevel: PathBuf,
    /// Scan 시점의 base branch와 base commit. Execute 때 base가 움직였는지 비교한다.
    pub base: ResolvedBase,
    /// 제거할 linked worktree. branch보다 먼저 처리한다.
    pub worktrees: Vec<WorktreeCandidate>,
    /// 삭제할 local branch.
    pub branches: Vec<BranchCandidate>,
    /// 삭제할 custom ref.
    pub custom_refs: Vec<CustomRefCandidate>,
    /// 안전 조건을 만족하지 못해 제외한 항목과 사유.
    pub skipped: Vec<SkippedItem>,
    /// 항목 하나를 검사하다 난 오류. 해당 항목은 후보에서 빠지고 나머지 검사는 계속한다.
    pub errors: Vec<CleanupError>,
}

impl CleanupPlan {
    /// 실제로 삭제할 항목 수 (worktree + branch + custom ref). 확인 dialog의 "Clean N items"에 쓴다.
    #[must_use]
    pub fn removable_count(&self) -> usize {
        self.worktrees.len() + self.branches.len() + self.custom_refs.len()
    }
}

/// 두 경로가 같은 directory를 가리키면 `true`.
///
/// `git worktree list`는 저장된 경로를, `rev-parse --show-toplevel`은 symlink를 푼 경로를
/// 돌려줄 수 있어 (macOS의 `/var` → `/private/var`) 문자열 비교만으로는 부족하다. 문자열이 다를 때만
/// 파일 시스템을 조회한다. 어느 한쪽이라도 풀 수 없으면 다른 경로로 본다.
fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}
