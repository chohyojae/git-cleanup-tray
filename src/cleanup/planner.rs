// SPDX-License-Identifier: GPL-3.0-only

//! mutation 없이 [`CleanupPlan`]을 만드는 planner (PLAN 9장 Scan).
//!
//! 순서: 저장소 식별 → default branch 판별 → ref 목록과 merged 목록 (`for-each-ref` 2회) →
//! worktree 목록 → 보호 규칙·worktree 안전 조건 적용.
//!
//! merge 판정은 `git for-each-ref --merged <base-commit>`으로 ref 전체를 한 번에 한다. 대상마다
//! `merge-base --is-ancestor`를 부르는 것과 판정은 같고 process 수만 줄어든다. Execute 직전
//! 재검증에서는 대상마다 `merge-base --is-ancestor`를 그대로 쓴다.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::cleanup::CleanupPlan;
use crate::cleanup::candidate::{
    BranchCandidate, CustomRefCandidate, SkipReason, SkippedItem, TargetKind, WorktreeCandidate,
};
use crate::error::Result;
use crate::git::default_branch::{self, BaseBranchQuery};
use crate::git::refs::{self, LOCAL_BRANCH_PREFIX, RefEntry};
use crate::git::repository::{self, RepositoryId};
use crate::git::runner::GitRunner;
use crate::git::worktree::{self, WorktreeEntry};

/// Scan 입력. 저장소 설정과 앱 설정에서 필요한 값만 빌려 와 복사하지 않는다.
#[derive(Debug, Clone, Copy)]
pub struct ScanRequest<'a> {
    /// 대상 저장소 식별자. 결과에 그대로 담는다.
    pub repository_id: RepositoryId,
    /// 등록된 저장소 경로 (main 또는 linked worktree 중 하나).
    pub path: &'a Path,
    /// base branch 판별 입력.
    pub base: BaseBranchQuery<'a>,
    /// 정리할 custom ref namespace (예: `refs/remote/`). 검증에 실패한 항목은 무시한다.
    pub custom_ref_namespaces: &'a [String],
    /// 보호 branch 이름 pattern. `*`는 `/`를 포함한 임의 문자열과 일치한다.
    pub protected_branch_patterns: &'a [String],
}

/// 저장소를 검사해 정리 계획을 만든다. 어떤 Git 상태도 바꾸지 않는다.
#[derive(Debug)]
pub struct CleanupPlanner<'r, R: GitRunner + ?Sized> {
    /// Git 실행에 쓰는 runner. test에서는 recording runner를 넣는다.
    runner: &'r R,
}

impl<'r, R: GitRunner + ?Sized> CleanupPlanner<'r, R> {
    /// `runner`를 쓰는 planner를 만든다.
    #[must_use]
    pub fn new(runner: &'r R) -> Self {
        Self { runner }
    }

    /// planner가 쓰는 runner를 돌려준다.
    #[must_use]
    pub fn runner(&self) -> &'r R {
        self.runner
    }

    /// 저장소를 검사해 [`CleanupPlan`]을 만든다.
    ///
    /// 개별 worktree 상태 확인 실패처럼 항목 하나에 국한된 오류는 plan의 `errors`에 담고 그 항목만
    /// 후보에서 뺀다. 저장소 전체를 검사할 수 없는 오류만 `Err`로 돌려준다.
    ///
    /// # Errors
    ///
    /// 저장소 식별 실패([`crate::error::CleanupError::NotARepository`] 등), base branch 판별 실패
    /// ([`crate::error::CleanupError::DefaultBranchUnknown`] 등), ref/worktree 목록 조회 실패.
    pub fn scan(&self, request: &ScanRequest<'_>) -> Result<CleanupPlan> {
        let path = request.path;
        let identity = repository::identify(self.runner, path)?;
        let base = default_branch::resolve(self.runner, path, request.base)?;

        // `refs/heads/`와 검증된 custom namespace를 한 번에 조회한다.
        let mut patterns: Vec<&str> = Vec::with_capacity(1 + request.custom_ref_namespaces.len());
        patterns.push(LOCAL_BRANCH_PREFIX);
        for namespace in request.custom_ref_namespaces {
            if refs::is_valid_custom_namespace(namespace) {
                patterns.push(namespace);
            } else {
                log::warn!("ignoring invalid custom ref namespace: {namespace}");
            }
        }
        let all_refs = refs::list_refs(self.runner, path, &patterns, None)?;
        let merged: HashSet<String> =
            refs::list_refs(self.runner, path, &patterns, Some(&base.commit))?
                .into_iter()
                .map(|entry| entry.name)
                .collect();
        let worktrees = worktree::list(self.runner, path)?;
        // branch ref → 그 branch를 checkout한 worktree. detached worktree는 정리 대상이 아니다.
        let worktree_by_branch: HashMap<&str, &WorktreeEntry> = worktrees
            .iter()
            .filter_map(|entry| entry.branch.as_deref().map(|branch| (branch, entry)))
            .collect();

        let mut plan = CleanupPlan {
            repository_id: request.repository_id,
            repository_path: path.to_path_buf(),
            toplevel: identity.toplevel,
            base,
            worktrees: Vec::new(),
            branches: Vec::new(),
            custom_refs: Vec::new(),
            skipped: Vec::new(),
            errors: Vec::new(),
        };

        for entry in all_refs {
            let is_merged = merged.contains(&entry.name);
            if let Some(branch) = entry.name.strip_prefix(LOCAL_BRANCH_PREFIX) {
                let worktree = worktree_by_branch.get(entry.name.as_str()).copied();
                self.classify_branch(&mut plan, request, branch, &entry, is_merged, worktree);
            } else if refs::is_custom_ref(&entry.name) && !refs::is_protected_ref(&entry.name) {
                classify_custom_ref(&mut plan, entry, is_merged);
            }
        }
        Ok(plan)
    }

    /// local branch 하나를 후보 또는 제외 항목으로 분류해 `plan`에 넣는다.
    ///
    /// 검사 순서는 "싼 판정 먼저": 이름 규칙 → object 종류 → merge 여부 → worktree 상태.
    /// `git status`는 merge된 branch의 linked worktree에만 실행한다.
    fn classify_branch(
        &self,
        plan: &mut CleanupPlan,
        request: &ScanRequest<'_>,
        branch: &str,
        entry: &RefEntry,
        is_merged: bool,
        worktree: Option<&WorktreeEntry>,
    ) {
        // base branch 자체는 후보도 제외 항목도 아니다. 목록에 띄울 이유가 없다.
        if branch == plan.base.branch {
            return;
        }
        let skip = |plan: &mut CleanupPlan, reason| {
            plan.skipped
                .push(SkippedItem::new(TargetKind::Branch, branch, reason));
        };
        if request
            .protected_branch_patterns
            .iter()
            .any(|pattern| glob_match(pattern, branch))
        {
            return skip(plan, SkipReason::Protected);
        }
        if entry.is_symbolic {
            return skip(plan, SkipReason::SymbolicRef);
        }
        if !entry.is_commit {
            return skip(plan, SkipReason::NotACommit);
        }
        if !is_merged {
            return skip(plan, SkipReason::NotMerged);
        }

        let Some(worktree) = worktree else {
            plan.branches.push(BranchCandidate {
                name: branch.to_owned(),
                commit: entry.target.clone(),
            });
            return;
        };
        // main worktree에 checkout된 branch는 main worktree를 바꾸지 않는 한 지울 수 없다.
        if worktree.is_main {
            return skip(plan, SkipReason::CheckedOutInMainWorktree);
        }
        let reason = if super::same_path(&worktree.path, &plan.toplevel) {
            Some(SkipReason::CurrentWorktree)
        } else if worktree.locked.is_some() {
            Some(SkipReason::LockedWorktree)
        } else if worktree.prunable.is_some() || !worktree.path.is_dir() {
            Some(SkipReason::MissingWorktreePath)
        } else {
            match worktree::is_clean(self.runner, &worktree.path) {
                Ok(true) => None,
                Ok(false) => Some(SkipReason::DirtyWorktree),
                Err(error) => {
                    // 상태를 모르면 clean으로 보지 않는다. 오류만 남기고 후보에서 뺀다.
                    plan.errors.push(error);
                    return;
                }
            }
        };
        match reason {
            Some(reason) => plan.skipped.push(SkippedItem::new(
                TargetKind::Worktree,
                worktree.path.display().to_string(),
                reason,
            )),
            None => plan.worktrees.push(WorktreeCandidate {
                path: worktree.path.clone(),
                branch: branch.to_owned(),
                commit: entry.target.clone(),
            }),
        }
    }
}

/// custom ref 하나를 후보 또는 제외 항목으로 분류해 `plan`에 넣는다.
fn classify_custom_ref(plan: &mut CleanupPlan, entry: RefEntry, is_merged: bool) {
    let reason = if entry.is_symbolic {
        // `refs/remote/x` → `refs/heads/main` 같은 symbolic ref를 지우다 대상 branch를 건드리지 않도록
        // 아예 후보로 올리지 않는다. executor의 `--no-deref`와 guard가 그 다음 방어선이다.
        SkipReason::SymbolicRef
    } else if !entry.is_commit {
        SkipReason::NotACommit
    } else if !is_merged {
        SkipReason::NotMerged
    } else {
        plan.custom_refs.push(CustomRefCandidate {
            full_ref: entry.name,
            commit: entry.target,
        });
        return;
    };
    plan.skipped
        .push(SkippedItem::new(TargetKind::CustomRef, entry.name, reason));
}

/// `*` wildcard만 지원하는 단순 glob 일치. `*`는 `/`를 포함한 0개 이상의 문자와 일치한다.
///
/// 보호 branch pattern(`release/*`, `*-keep`) 용도로는 이것으로 충분해 glob crate를 들이지 않는다.
/// backtracking 지점을 마지막 `*` 하나만 기억하므로 O(pattern × name)이다.
fn glob_match(pattern: &str, name: &str) -> bool {
    let (pattern, name) = (pattern.as_bytes(), name.as_bytes());
    let (mut p, mut n) = (0, 0);
    // 마지막으로 만난 `*`의 위치와, 그 `*`가 삼키기 시작한 name 위치.
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            star = Some((p, n));
            p += 1;
        } else if p < pattern.len() && pattern[p] == name[n] {
            p += 1;
            n += 1;
        } else if let Some((star_p, star_n)) = star {
            // `*`가 한 글자 더 삼킨 것으로 보고 다시 맞춰 본다.
            p = star_p + 1;
            n = star_n + 1;
            star = Some((star_p, star_n + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&b| b == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 보호 branch pattern의 `*` 일치 규칙을 확인한다.
    #[test]
    fn matches_protected_patterns() {
        assert!(glob_match("develop", "develop"));
        assert!(!glob_match("develop", "develop2"));
        assert!(glob_match("release/*", "release/1.0"));
        assert!(glob_match("release/*", "release/1.0/hotfix"));
        assert!(!glob_match("release/*", "releases/1.0"));
        assert!(glob_match("*-keep", "기능/한글-keep"));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(!glob_match("a*b*c", "aXXbYY"));
        assert!(glob_match("*", ""));
        assert!(!glob_match("", "x"));
    }
}
