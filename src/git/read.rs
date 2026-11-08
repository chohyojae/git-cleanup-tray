// SPDX-License-Identifier: GPL-3.0-only

//! Git 명령과 라이브러리가 같은 타입으로 결과를 반환하는 읽기 전용 인터페이스.

use std::path::Path;

use crate::error::Result;

use super::object::ObjectId;
use super::refs::RefEntry;
use super::repository::RepositoryIdentity;
use super::runner::GitRunner;
use super::worktree::WorktreeEntry;

/// 최신화에 필요한 로컬 브랜치와 upstream 설정.
pub struct TrackedBranch {
    /// 로컬 브랜치의 full ref.
    pub name: String,
    /// 조회 시점의 커밋. 변경 직전에 다시 확인한다.
    pub commit: ObjectId,
    /// 앱의 제한된 fetch 목적지에 해당하는 원격 추적 ref.
    pub upstream: String,
    /// upstream의 remote 이름. 로컬 upstream은 최신화하지 않는다.
    pub remote: String,
}

/// 작업 스레드 안에서만 사용하는 저장소 조회 상태. 변경 API는 제공하지 않는다.
pub trait GitReadRepository {
    /// 작업 경로와 공통 Git 디렉터리를 식별한다.
    ///
    /// # Errors
    /// 저장소 접근·식별 실패를 반환한다.
    fn identity(&mut self) -> Result<RepositoryIdentity>;

    /// symbolic ref를 따라간 최종 ref 이름을 읽는다. 일반 ref이거나 없으면 `None`.
    ///
    /// # Errors
    /// 순환 참조·권한·저장소 오류를 반환한다.
    fn symbolic_ref(&mut self, name: &str) -> Result<Option<String>>;

    /// 정확한 이름 또는 `/` 경계 prefix에 해당하는 ref를 조회한다.
    ///
    /// # Errors
    /// ref·object·커밋 관계를 읽지 못하면 오류를 반환한다.
    fn list_refs(
        &mut self,
        patterns: &[&str],
        merged_into: Option<&ObjectId>,
    ) -> Result<Vec<RefEntry>>;

    /// ref를 커밋으로 푼다. ref가 없거나 커밋이 아니면 `None`.
    ///
    /// # Errors
    /// 손상된 object·ref 등 조회 오류를 반환한다.
    fn resolve_commit(&mut self, name: &str) -> Result<Option<ObjectId>>;

    /// 동일한 커밋을 포함해 조상 관계를 확인한다.
    ///
    /// # Errors
    /// 커밋이나 이력을 읽지 못하면 오류를 반환한다.
    fn is_ancestor(&mut self, ancestor: &ObjectId, descendant: &ObjectId) -> Result<bool>;

    /// 메인 워크트리를 첫 항목으로, 연결된 워크트리와 잠금 상태를 조회한다.
    ///
    /// # Errors
    /// 워크트리 등록·경로·상태를 읽지 못하면 오류를 반환한다.
    fn worktrees(&mut self) -> Result<Vec<WorktreeEntry>>;

    /// 지정한 워크트리에 스테이징·작업 파일·추적하지 않는 파일 변경이 없는지 확인한다.
    ///
    /// # Errors
    /// 상태가 불명확하면 변경 없음으로 간주하지 않고 오류를 반환한다.
    fn is_clean(&mut self) -> Result<bool>;

    /// 최신화할 일반 로컬 브랜치의 upstream 설정을 읽는다.
    ///
    /// # Errors
    /// ref·커밋·upstream 설정을 읽지 못하면 오류를 반환한다.
    fn tracked_branches(&mut self) -> Result<Vec<TrackedBranch>>;
}

/// 기존 Git 명령 조회를 타입 기반 인터페이스에 연결한다. guard를 우회하지 않는다.
pub(crate) struct CommandRead<'a, R: GitRunner + ?Sized> {
    /// 실제 Git 실행과 테스트 기록을 담당하는 runner.
    runner: &'a R,
    /// Git 명령을 실행할 저장소 경로.
    path: &'a Path,
}

impl<'a, R: GitRunner + ?Sized> CommandRead<'a, R> {
    /// 저장소를 미리 읽지 않는 가벼운 조회 상태를 만든다.
    pub(crate) fn new(runner: &'a R, path: &'a Path) -> Self {
        Self { runner, path }
    }
}

impl<R: GitRunner + ?Sized> GitReadRepository for CommandRead<'_, R> {
    /// 기존 `rev-parse` 조회를 실행한다.
    fn identity(&mut self) -> Result<RepositoryIdentity> {
        super::repository::identify_process(self.runner, self.path)
    }

    /// `symbolic-ref --quiet`의 없음과 실패를 구분한다.
    fn symbolic_ref(&mut self, name: &str) -> Result<Option<String>> {
        let output = self.runner.run(
            self.path,
            &["symbolic-ref".as_ref(), "--quiet".as_ref(), name.as_ref()],
        )?;
        match output.exit_code {
            Some(0) => Ok(Some(output.stdout_lossy().trim_end().to_owned())),
            Some(1) => Ok(None),
            _ => Err(output.failure("symbolic-ref")),
        }
    }

    /// 기존 일괄 ref 조회를 실행한다.
    fn list_refs(
        &mut self,
        patterns: &[&str],
        merged_into: Option<&ObjectId>,
    ) -> Result<Vec<RefEntry>> {
        super::refs::list_refs_process(self.runner, self.path, patterns, merged_into)
    }

    /// 기존 커밋 조회를 실행한다.
    fn resolve_commit(&mut self, name: &str) -> Result<Option<ObjectId>> {
        super::refs::resolve_commit_process(self.runner, self.path, name)
    }

    /// 기존 조상 관계 명령의 0·1 종료 코드를 해석한다.
    fn is_ancestor(&mut self, ancestor: &ObjectId, descendant: &ObjectId) -> Result<bool> {
        super::refs::is_ancestor_process(self.runner, self.path, ancestor, descendant)
    }

    /// 기존 NUL 구분 워크트리 목록을 읽는다.
    fn worktrees(&mut self) -> Result<Vec<WorktreeEntry>> {
        super::worktree::list_process(self.runner, self.path)
    }

    /// 지정한 워크트리의 기존 파일 상태 검사를 실행한다.
    fn is_clean(&mut self) -> Result<bool> {
        super::worktree::is_clean_process(self.runner, self.path)
    }

    /// 기존 upstream 일괄 조회를 실행한다.
    fn tracked_branches(&mut self) -> Result<Vec<TrackedBranch>> {
        super::sync::read_branches_process(self.runner, self.path)
    }
}
