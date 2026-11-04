// SPDX-License-Identifier: GPL-3.0-only

//! [`CleanupPlan`]을 실행하는 executor (PLAN 9장 Execute).
//!
//! 처리 순서와 규칙:
//! 1. base branch가 Scan 때와 같은지 확인한다. 바뀌었으면 아무것도 지우지 않는다.
//! 2. 각 대상마다 삭제 직전에 최신 상태를 다시 확인한다 (ref 존재, merge 여부,
//!    worktree-branch 매핑, clean 여부, lock 여부).
//! 3. worktree: `git worktree remove -- <path>` 성공 시에만 `git branch -d -- <branch>`.
//! 4. branch: `git branch -d -- <branch>` (`-D` 금지).
//! 5. custom ref: `git update-ref --no-deref -d <full-ref> <scan-commit>`.
//! 6. 마지막에 `git worktree prune`.
//!
//! 조건이 바뀐 대상은 삭제하지 않고 `SkipReason::StateChanged`로 기록한다. 한 대상의 오류는
//! 결과의 `errors`에 담고 다음 대상을 계속 처리한다.

use std::path::Path;

use crate::cleanup::CleanupPlan;
use crate::cleanup::candidate::{
    BranchCandidate, CustomRefCandidate, SkipReason, SkippedItem, TargetKind, WorktreeCandidate,
};
use crate::cleanup::result::CleanupResult;
use crate::error::Result;
use crate::git::default_branch;
use crate::git::object::ObjectId;
use crate::git::refs::{self, LOCAL_BRANCH_PREFIX};
use crate::git::runner::{GitOutput, GitRunner};
use crate::git::worktree::{self, WorktreeEntry};

/// 재검증 결과. 오류는 `Result`의 `Err`로 따로 전달한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    /// Scan 때와 같은 상태라 삭제해도 된다.
    Unchanged,
    /// 상태가 바뀌어 삭제하지 않는다.
    Changed,
}

impl Check {
    /// `true`면 [`Check::Unchanged`].
    fn from_bool(unchanged: bool) -> Self {
        if unchanged {
            Self::Unchanged
        } else {
            Self::Changed
        }
    }
}

/// 정리 계획을 재검증하며 실행한다.
#[derive(Debug)]
pub struct CleanupExecutor<'r, R: GitRunner + ?Sized> {
    /// Git 실행에 쓰는 runner.
    runner: &'r R,
}

impl<'r, R: GitRunner + ?Sized> CleanupExecutor<'r, R> {
    /// `runner`를 쓰는 executor를 만든다.
    #[must_use]
    pub fn new(runner: &'r R) -> Self {
        Self { runner }
    }

    /// executor가 쓰는 runner를 돌려준다.
    #[must_use]
    pub fn runner(&self) -> &'r R {
        self.runner
    }

    /// `plan`을 실행하고 결과를 돌려준다.
    ///
    /// plan을 그대로 믿지 않는다. 대상마다 삭제 직전에 최신 상태를 다시 확인한다.
    /// 저장소 단위 실패도 `Err` 대신 결과의 `errors`에 담아, 호출자가 Clean All에서 다음 저장소로
    /// 넘어갈 수 있게 한다.
    #[must_use]
    pub fn execute(&self, plan: &CleanupPlan) -> CleanupResult {
        let mut result = CleanupResult {
            repository_id: Some(plan.repository_id),
            ..CleanupResult::default()
        };
        let repo = plan.repository_path.as_path();

        match default_branch::revalidate(self.runner, repo, &plan.base) {
            Ok(true) => {}
            Ok(false) => {
                // base가 바뀌면 모든 merge 판정의 전제가 무너진다. 하나도 지우지 않는다.
                skip_all(plan, &mut result);
                return result;
            }
            Err(error) => {
                result.errors.push(error);
                return result;
            }
        }

        // worktree를 먼저 제거해야 그 branch를 `branch -d`로 지울 수 있다.
        for candidate in &plan.worktrees {
            if let Err(error) = self.remove_worktree(plan, candidate, &mut result) {
                result.errors.push(error);
            }
        }
        // 위에서 worktree를 제거했으므로 branch 단계용 worktree 목록은 지금 다시 읽는다.
        match worktree::list(self.runner, repo) {
            Ok(worktrees) => {
                for candidate in &plan.branches {
                    if let Err(error) = self.delete_branch(plan, candidate, &worktrees, &mut result)
                    {
                        result.errors.push(error);
                    }
                }
            }
            Err(error) => result.errors.push(error),
        }
        for candidate in &plan.custom_refs {
            if let Err(error) = self.delete_custom_ref(plan, candidate, &mut result) {
                result.errors.push(error);
            }
        }

        // 경로가 사라진 worktree의 metadata만 정리한다. 파일은 건드리지 않는다 (PLAN 8.2).
        match self.git(repo, &["worktree", "prune"]) {
            Ok(output) if !output.success() => result.errors.push(output.failure("worktree")),
            Ok(_) => {}
            Err(error) => result.errors.push(error),
        }
        result
    }

    /// worktree 하나를 재검증 후 제거하고, 성공하면 연결된 branch도 삭제한다.
    fn remove_worktree(
        &self,
        plan: &CleanupPlan,
        candidate: &WorktreeCandidate,
        result: &mut CleanupResult,
    ) -> Result<()> {
        let repo = plan.repository_path.as_path();
        let skip = |result: &mut CleanupResult| {
            result.skipped.push(SkippedItem::new(
                TargetKind::Worktree,
                candidate.path.display().to_string(),
                SkipReason::StateChanged,
            ));
        };
        if self.check_worktree(plan, candidate)? == Check::Changed {
            skip(result);
            return Ok(());
        }

        let output = self.runner.run(
            repo,
            &[
                "worktree".as_ref(),
                "remove".as_ref(),
                "--".as_ref(),
                candidate.path.as_os_str(),
            ],
        )?;
        if !output.success() {
            // 재검증과 remove 사이에 파일이 생기거나 lock이 걸리면 Git이 거부한다.
            // 이는 오류가 아니라 "상태가 바뀜"이다.
            if is_state_change_refusal(&output) {
                skip(result);
                return Ok(());
            }
            return Err(output.failure("worktree"));
        }
        result.removed_worktrees.push(candidate.path.clone());

        // worktree 제거와 branch 삭제 사이에도 branch가 움직였을 수 있어 한 번 더 확인한다.
        let branch = BranchCandidate {
            name: candidate.branch.clone(),
            commit: candidate.commit.clone(),
        };
        if self.check_branch_commit(plan, &branch)? == Check::Changed {
            result.skipped.push(SkippedItem::new(
                TargetKind::Branch,
                &candidate.branch,
                SkipReason::StateChanged,
            ));
            return Ok(());
        }
        self.run_branch_delete(repo, &branch, result)
    }

    /// worktree 후보가 Scan 때와 같은 상태인지 확인한다.
    ///
    /// 확인 항목: 목록에 그대로 있음, 같은 branch에 연결됨, main/등록 경로가 아님, lock 없음,
    /// 경로 존재, branch commit 동일, 여전히 merge됨, clean. 싼 확인부터 하고 `git status`는 마지막에 한다.
    fn check_worktree(&self, plan: &CleanupPlan, candidate: &WorktreeCandidate) -> Result<Check> {
        let repo = plan.repository_path.as_path();
        let worktrees = worktree::list(self.runner, repo)?;
        let Some(entry) = worktrees.iter().find(|entry| entry.path == candidate.path) else {
            return Ok(Check::Changed);
        };
        let branch_ref = format!("{LOCAL_BRANCH_PREFIX}{}", candidate.branch);
        let mapping_unchanged = entry.branch.as_deref() == Some(branch_ref.as_str())
            && !entry.is_main
            && entry.locked.is_none()
            && entry.prunable.is_none()
            && entry.path.is_dir()
            && !super::same_path(&entry.path, &plan.toplevel);
        if !mapping_unchanged {
            return Ok(Check::Changed);
        }
        let branch = BranchCandidate {
            name: candidate.branch.clone(),
            commit: candidate.commit.clone(),
        };
        if self.check_branch_commit(plan, &branch)? == Check::Changed {
            return Ok(Check::Changed);
        }
        Ok(Check::from_bool(worktree::is_clean(
            self.runner,
            &entry.path,
        )?))
    }

    /// worktree 없는 branch 하나를 재검증 후 삭제한다.
    ///
    /// `worktrees`는 branch 단계 시작 직전에 읽은 목록이다. 그 뒤에 branch가 어딘가에 checkout되더라도
    /// `git branch -d`가 checkout된 branch 삭제를 스스로 거부하므로 안전하다.
    fn delete_branch(
        &self,
        plan: &CleanupPlan,
        candidate: &BranchCandidate,
        worktrees: &[WorktreeEntry],
        result: &mut CleanupResult,
    ) -> Result<()> {
        let branch_ref = format!("{LOCAL_BRANCH_PREFIX}{}", candidate.name);
        let checked_out = worktrees
            .iter()
            .any(|entry| entry.branch.as_deref() == Some(branch_ref.as_str()));
        if checked_out || self.check_branch_commit(plan, candidate)? == Check::Changed {
            result.skipped.push(SkippedItem::new(
                TargetKind::Branch,
                &candidate.name,
                SkipReason::StateChanged,
            ));
            return Ok(());
        }
        self.run_branch_delete(&plan.repository_path, candidate, result)
    }

    /// branch가 Scan 때와 같은 commit을 가리키고 여전히 base에 merge되어 있는지 확인한다.
    ///
    /// 모든 branch 삭제 경로가 이 함수를 거치므로 base branch 이름도 여기서 거른다. planner가 이미
    /// 빼지만, base는 자기 자신에 "merge된" 상태라 다른 확인을 모두 통과하기 때문에 한 번 더 막는다.
    fn check_branch_commit(
        &self,
        plan: &CleanupPlan,
        candidate: &BranchCandidate,
    ) -> Result<Check> {
        if candidate.name == plan.base.branch {
            return Ok(Check::Changed);
        }
        let repo = plan.repository_path.as_path();
        let branch_ref = format!("{LOCAL_BRANCH_PREFIX}{}", candidate.name);
        let current = refs::resolve_commit(self.runner, repo, &branch_ref)?;
        if current.as_ref() != Some(&candidate.commit) {
            return Ok(Check::Changed);
        }
        self.check_merged(repo, &candidate.commit, &plan.base.commit)
    }

    /// `commit`이 base commit의 ancestor인지 `merge-base --is-ancestor`로 확인한다 (PLAN 8.1).
    fn check_merged(&self, repo: &Path, commit: &ObjectId, base: &ObjectId) -> Result<Check> {
        Ok(Check::from_bool(refs::is_ancestor(
            self.runner,
            repo,
            commit,
            base,
        )?))
    }

    /// `git branch -d -- <name>`을 실행하고 결과를 기록한다.
    ///
    /// `-d`는 branch가 upstream(없으면 `HEAD`)에 merge되었는지 Git이 한 번 더 확인한다. main worktree의
    /// `HEAD`가 base가 아닌 다른 branch에 있으면 base에 merge된 branch라도 거부될 수 있는데, 이때는
    /// 강제 삭제하지 않고 Git 오류를 그대로 보여준다.
    fn run_branch_delete(
        &self,
        repo: &Path,
        candidate: &BranchCandidate,
        result: &mut CleanupResult,
    ) -> Result<()> {
        let output = self.runner.run(
            repo,
            &[
                "branch".as_ref(),
                "-d".as_ref(),
                "--".as_ref(),
                candidate.name.as_ref(),
            ],
        )?;
        if !output.success() {
            return Err(output.failure("branch"));
        }
        result.removed_branches.push(candidate.name.clone());
        Ok(())
    }

    /// custom ref 하나를 재검증 후 삭제한다.
    ///
    /// `update-ref --no-deref -d <ref> <old-oid>`는 ref가 `<old-oid>`를 가리킬 때만 원자적으로 지운다.
    /// 확인과 삭제 사이에 ref가 움직여도 Git이 거부하므로 별도 `rev-parse`가 필요 없다.
    fn delete_custom_ref(
        &self,
        plan: &CleanupPlan,
        candidate: &CustomRefCandidate,
        result: &mut CleanupResult,
    ) -> Result<()> {
        let repo = plan.repository_path.as_path();
        let skip = |result: &mut CleanupResult| {
            result.skipped.push(SkippedItem::new(
                TargetKind::CustomRef,
                &candidate.full_ref,
                SkipReason::StateChanged,
            ));
        };
        // guard도 막지만, namespace 밖 ref는 시도조차 하지 않는다.
        if !refs::is_custom_ref(&candidate.full_ref) || refs::is_protected_ref(&candidate.full_ref)
        {
            skip(result);
            return Ok(());
        }
        if self.check_merged(repo, &candidate.commit, &plan.base.commit)? == Check::Changed {
            skip(result);
            return Ok(());
        }

        let output = self.runner.run(
            repo,
            &[
                "update-ref".as_ref(),
                // symbolic ref라면 가리키는 대상이 아니라 그 ref 자신만 지운다.
                "--no-deref".as_ref(),
                "-d".as_ref(),
                candidate.full_ref.as_ref(),
                candidate.commit.as_ref(),
            ],
        )?;
        if output.success() {
            result.removed_custom_refs.push(candidate.full_ref.clone());
        } else if is_ref_moved_refusal(&output) {
            skip(result);
        } else {
            return Err(output.failure("update-ref"));
        }
        Ok(())
    }

    /// 인자가 모두 상수 문자열인 Git 명령을 실행한다.
    fn git(&self, repo: &Path, args: &[&str]) -> Result<GitOutput> {
        let args: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
        self.runner.run(repo, &args)
    }
}

/// plan의 모든 후보를 `StateChanged`로 결과에 기록한다. base가 바뀌었을 때 쓴다.
fn skip_all(plan: &CleanupPlan, result: &mut CleanupResult) {
    result.skipped.reserve(plan.removable_count());
    result
        .skipped
        .extend(plan.worktrees.iter().map(|candidate| {
            SkippedItem::new(
                TargetKind::Worktree,
                candidate.path.display().to_string(),
                SkipReason::StateChanged,
            )
        }));
    result.skipped.extend(plan.branches.iter().map(|candidate| {
        SkippedItem::new(
            TargetKind::Branch,
            &candidate.name,
            SkipReason::StateChanged,
        )
    }));
    result
        .skipped
        .extend(plan.custom_refs.iter().map(|candidate| {
            SkippedItem::new(
                TargetKind::CustomRef,
                &candidate.full_ref,
                SkipReason::StateChanged,
            )
        }));
}

/// `git update-ref -d <ref> <old-oid>` 실패가 "Scan 이후 ref가 움직였거나 사라짐" 때문인지 판별한다.
///
/// 다른 commit으로 움직였으면 `but expected`, 이미 지워졌으면 `unable to resolve reference`가 나온다
/// (Git 2.54, `LC_ALL=C`).
fn is_ref_moved_refusal(output: &GitOutput) -> bool {
    let stderr = output.stderr_lossy();
    stderr.contains("but expected") || stderr.contains("unable to resolve reference")
}

/// `git worktree remove` 실패가 "변경 파일 있음" 또는 "lock됨" 때문인지 판별한다.
///
/// runner가 `LC_ALL=C`로 실행하므로 영어 문구로 판별한다.
fn is_state_change_refusal(output: &GitOutput) -> bool {
    let stderr = output.stderr_lossy();
    stderr.contains("contains modified or untracked files")
        || stderr.contains("locked working tree")
}
