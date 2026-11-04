// SPDX-License-Identifier: GPL-3.0-only

//! 정리 후보와 제외 항목 타입 (PLAN 8, 14장).
//!
//! 각 후보는 Scan 시점의 commit을 함께 들고 있어 Execute 직전 재검증에서 "그 사이에 바뀌었는지"를
//! 비교할 수 있다.

use std::path::PathBuf;

use crate::git::object::ObjectId;

/// 제거 후보 linked worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeCandidate {
    /// worktree 경로. `git worktree list` 출력 그대로다.
    pub path: PathBuf,
    /// 연결된 branch 이름 (`refs/heads/` 제외). worktree 제거 후 이 branch를 삭제한다.
    pub branch: String,
    /// Scan 시점의 branch commit.
    pub commit: ObjectId,
}

/// 삭제 후보 local branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchCandidate {
    /// branch 이름 (`refs/heads/` 제외).
    pub name: String,
    /// Scan 시점의 branch commit.
    pub commit: ObjectId,
}

/// 삭제 후보 custom ref (`refs/remote/*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomRefCandidate {
    /// full ref 이름.
    pub full_ref: String,
    /// Scan 시점의 ref commit. 삭제 시 `update-ref -d <ref> <commit>`으로 넘겨 원자적으로 비교한다.
    pub commit: ObjectId,
}

/// 제외 항목의 종류. Detail 화면의 Worktrees / Local Branches / Custom Refs 구분에 쓴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    /// linked worktree (`target`은 worktree 경로).
    Worktree,
    /// local branch (`target`은 branch 이름).
    Branch,
    /// custom ref (`target`은 full ref 이름).
    CustomRef,
}

/// 정리 대상에서 제외한 이유. UI에서는 색상만이 아니라 이 사유 문구로 상태를 구분한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// base branch에 아직 merge되지 않았다.
    NotMerged,
    /// 사용자가 보호 branch/ref로 지정했다.
    Protected,
    /// ref가 commit이 아닌 object(tag, tree 등)를 가리킨다.
    NotACommit,
    /// 다른 ref를 가리키는 symbolic ref다. 따라간 대상이 지워질 위험이 있어 자동 정리하지 않는다.
    SymbolicRef,
    /// branch가 main worktree에 checkout되어 있다. main worktree는 건드리지 않으므로 삭제할 수 없다.
    CheckedOutInMainWorktree,
    /// 앱에 등록한 경로 자체가 이 linked worktree다. 실행 중인 위치를 지우지 않는다.
    CurrentWorktree,
    /// worktree에 staged/modified/untracked 변경이 있다.
    DirtyWorktree,
    /// worktree가 lock 상태다.
    LockedWorktree,
    /// worktree 경로가 존재하지 않는다. 마지막 `git worktree prune` 뒤 다음 Scan에서 다시 판단한다.
    MissingWorktreePath,
    /// Scan 이후 상태가 바뀌었다 (Execute 재검증 실패).
    StateChanged,
}

/// 제외된 항목 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedItem {
    /// 항목 종류.
    pub kind: TargetKind,
    /// 사용자에게 보여줄 대상 이름 (branch 이름, ref, worktree 경로 등).
    pub target: String,
    /// 제외 사유.
    pub reason: SkipReason,
}

impl SkippedItem {
    /// 제외 항목을 만든다.
    #[must_use]
    pub fn new(kind: TargetKind, target: impl Into<String>, reason: SkipReason) -> Self {
        Self {
            kind,
            target: target.into(),
            reason,
        }
    }
}
