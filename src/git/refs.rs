// SPDX-License-Identifier: GPL-3.0-only

//! ref namespace 구분과 branch/custom ref scanning (PLAN 5, 8장).
//!
//! `refs/remote/*`(AI agent custom ref, 정리 대상)와 `refs/remotes/*`(remote-tracking ref,
//! 정리 대상 아님)는 한 글자 차이라 혼동하기 쉽다. 두 namespace 판별은 반드시 이 module의
//! 함수를 쓴다.
//!
//! ref 목록은 `git for-each-ref` 한 번으로 받는다. 대상마다 `rev-parse`를 따로 부르지 않는다
//! (AGENTS 4.2).

use std::path::Path;

use crate::error::Result;
use crate::git::object::ObjectId;
use crate::git::runner::GitRunner;

/// AI coding agent가 남기는 custom ref의 기본 namespace. 정리 대상이다.
pub const CUSTOM_REF_PREFIX: &str = "refs/remote/";

/// remote-tracking ref namespace. 절대 정리하지 않는다.
pub const REMOTE_TRACKING_PREFIX: &str = "refs/remotes/";

/// local branch namespace.
pub const LOCAL_BRANCH_PREFIX: &str = "refs/heads/";

/// 어떤 경우에도 자동 삭제하지 않는 namespace (PLAN 8.4장).
pub const PROTECTED_PREFIXES: [&str; 3] = [REMOTE_TRACKING_PREFIX, "refs/tags/", "refs/notes/"];

/// full ref 이름이 custom ref namespace(`refs/remote/*`)에 속하면 `true`.
///
/// `refs/remotes/...`는 `refs/remote/` 다음 글자가 `/`가 아니라 `s`이므로 여기에 걸리지 않는다.
/// `refs/remote/../heads/main`처럼 namespace를 벗어나는 `..`/`.`/빈 component가 있으면 거부한다.
/// Git도 이런 이름을 거부하지만, guard가 Git의 검사에만 기대지 않게 한다.
#[must_use]
pub fn is_custom_ref(full_ref: &str) -> bool {
    full_ref
        .strip_prefix(CUSTOM_REF_PREFIX)
        .is_some_and(|rest| {
            !rest.is_empty()
                && rest
                    .split('/')
                    .all(|component| !matches!(component, "" | "." | ".."))
        })
}

/// full ref 이름이 보호 namespace에 속하거나 `refs/stash`이면 `true`.
#[must_use]
pub fn is_protected_ref(full_ref: &str) -> bool {
    full_ref == "refs/stash" || PROTECTED_PREFIXES.iter().any(|p| full_ref.starts_with(p))
}

/// custom ref namespace 설정값이 안전한지 확인한다.
///
/// guard는 `refs/remote/*` 밖의 `update-ref -d`를 막으므로, 설정에 그 밖의 namespace가
/// 들어오면 Scan에서 후보로 올려도 Execute에서 차단된다. 처음부터 받아들이지 않는다.
/// `/`로 끝나야 하며 (`refs/remote/x`가 `refs/remote/xy`까지 잡지 않도록) 보호 namespace와
/// 겹치면 안 된다.
#[must_use]
pub fn is_valid_custom_namespace(namespace: &str) -> bool {
    namespace.strip_suffix('/').is_some_and(|base| {
        namespace == CUSTOM_REF_PREFIX || (is_custom_ref(base) && !is_protected_ref(namespace))
    })
}

/// `git for-each-ref` 한 줄: ref 이름과 가리키는 object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefEntry {
    /// full ref 이름 (예: `refs/heads/feature/x`).
    pub name: String,
    /// ref가 가리키는 object id.
    pub target: ObjectId,
    /// 가리키는 object가 commit이면 `true`. tag/tree/blob을 가리키는 ref는 merge 판정을 하지 않는다.
    pub is_commit: bool,
    /// symbolic ref(다른 ref를 가리키는 ref)이면 `true`. `target`은 따라간 끝의 object다.
    pub is_symbolic: bool,
}

/// `patterns`에 해당하는 ref를 나열한다. `merged_into`가 있으면 그 commit에서 도달 가능한
/// (즉 이미 merge된) ref만 돌려준다.
///
/// `--merged <commit>`은 `merge-base --is-ancestor <ref> <commit>`과 같은 판정을 ref 전체에 대해
/// process 한 번으로 해 준다. ref 이름에는 NUL과 줄바꿈이 들어갈 수 없으므로 (Git 규칙) field는
/// NUL, record는 줄바꿈으로 구분해도 안전하다.
///
/// pattern은 Git 규칙대로 "완전 일치 또는 `/` 경계까지 prefix 일치"다. 따라서 `refs/remote/`는
/// `refs/remotes/...`를 잡지 않는다.
///
/// # Errors
///
/// Git 실행이 실패하면 [`crate::error::CleanupError::GitCommandFailed`].
pub(crate) fn list_refs_process<R: GitRunner + ?Sized>(
    runner: &R,
    repository: &Path,
    patterns: &[&str],
    merged_into: Option<&ObjectId>,
) -> Result<Vec<RefEntry>> {
    let mut args: Vec<&std::ffi::OsStr> = Vec::with_capacity(4 + patterns.len());
    args.push("for-each-ref".as_ref());
    args.push("--format=%(refname)%00%(objectname)%00%(objecttype)%00%(symref)".as_ref());
    if let Some(commit) = merged_into {
        args.push("--merged".as_ref());
        args.push(commit.as_ref());
    }
    // pattern 앞의 `--`는 for-each-ref가 받지 않으므로 쓰지 않는다. pattern은 모두 `refs/`로
    // 시작하는 내부 상수나 검증된 namespace라 option으로 오인될 일이 없다.
    args.extend(patterns.iter().map(|p| std::ffi::OsStr::new(*p)));

    let output = runner.run(repository, &args)?;
    if !output.success() {
        return Err(output.failure("for-each-ref"));
    }
    Ok(parse_for_each_ref(&output.stdout))
}

/// [`list_refs`] 출력 (`<name>\0<oid>\0<type>\0<symref target>\n` 반복)을 parsing한다.
///
/// 형식이 맞지 않는 줄은 건너뛴다. 존재하지 않는 ref를 후보로 만드는 것보다 빠뜨리는 쪽이 안전하다.
fn parse_for_each_ref(stdout: &[u8]) -> Vec<RefEntry> {
    let text = String::from_utf8_lossy(stdout);
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let name = fields.next()?;
            let target = ObjectId::parse(fields.next()?)?;
            let is_commit = fields.next()? == "commit";
            // 일반 ref는 마지막 field가 비어 있다.
            let is_symbolic = !fields.next()?.is_empty();
            Some(RefEntry {
                name: name.to_owned(),
                target,
                is_commit,
                is_symbolic,
            })
        })
        .collect()
}

/// full ref가 지금 가리키는 commit을 돌려준다. ref가 없거나 commit으로 풀리지 않으면 `None`.
///
/// Execute 직전 재검증에서 "Scan 이후 ref가 움직였는지" 확인하는 데 쓴다.
///
/// # Errors
///
/// `--verify --quiet`는 ref가 없을 때 exit 1로 조용히 끝난다. 그 밖의 실패는
/// [`crate::error::CleanupError::GitCommandFailed`].
pub(crate) fn resolve_commit_process<R: GitRunner + ?Sized>(
    runner: &R,
    repository: &Path,
    full_ref: &str,
) -> Result<Option<ObjectId>> {
    // `^{commit}`을 붙여 tag object 등이 아닌 commit으로 풀리는 경우만 받는다.
    let spec = format!("{full_ref}^{{commit}}");
    let output = runner.run(
        repository,
        &[
            "rev-parse".as_ref(),
            "--verify".as_ref(),
            "--quiet".as_ref(),
            spec.as_ref(),
        ],
    )?;
    match output.exit_code {
        Some(0) => Ok(ObjectId::parse(output.stdout_lossy().trim_end())),
        Some(1) => Ok(None),
        _ => Err(output.failure("rev-parse")),
    }
}

/// `ancestor`가 `descendant`의 ancestor(또는 같은 commit)이면 `true`.
///
/// PLAN 8.1의 merge 판정 명령 `git merge-base --is-ancestor`를 그대로 쓴다.
///
/// # Errors
///
/// exit code 0(ancestor), 1(아님) 외에는 [`crate::error::CleanupError::GitCommandFailed`].
pub(crate) fn is_ancestor_process<R: GitRunner + ?Sized>(
    runner: &R,
    repository: &Path,
    ancestor: &ObjectId,
    descendant: &ObjectId,
) -> Result<bool> {
    let output = runner.run(
        repository,
        &[
            "merge-base".as_ref(),
            "--is-ancestor".as_ref(),
            ancestor.as_ref(),
            descendant.as_ref(),
        ],
    )?;
    match output.exit_code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(output.failure("merge-base")),
    }
}

/// 선택한 이름·namespace의 ref와 object 종류를 읽고 필요하면 병합된 ref만 반환한다.
///
/// # Errors
/// 저장소 접근과 해당 조회 작업의 오류를 반환한다.
pub fn list_refs<R: GitRunner + ?Sized>(
    runner: &R,
    repository: &Path,
    patterns: &[&str],
    merged_into: Option<&ObjectId>,
) -> Result<Vec<RefEntry>> {
    runner
        .open_read(repository)?
        .list_refs(patterns, merged_into)
}

/// ref가 가리키는 커밋을 읽는다. 없거나 커밋이 아니면 `None`을 반환한다.
///
/// # Errors
/// 저장소 접근과 해당 조회 작업의 오류를 반환한다.
pub fn resolve_commit<R: GitRunner + ?Sized>(
    runner: &R,
    repository: &Path,
    full_ref: &str,
) -> Result<Option<ObjectId>> {
    runner.open_read(repository)?.resolve_commit(full_ref)
}

/// 같은 커밋을 포함해 첫 커밋이 두 번째 커밋의 조상인지 확인한다.
///
/// # Errors
/// 저장소 접근과 해당 조회 작업의 오류를 반환한다.
pub fn is_ancestor<R: GitRunner + ?Sized>(
    runner: &R,
    repository: &Path,
    ancestor: &ObjectId,
    descendant: &ObjectId,
) -> Result<bool> {
    runner
        .open_read(repository)?
        .is_ancestor(ancestor, descendant)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// for-each-ref 출력에서 ref 이름, object id, commit 여부를 읽는다.
    #[test]
    fn parses_for_each_ref_output() {
        let oid = "e6ec21a92ade4f3a94039927941b1c3c3929b930";
        let stdout = format!(
            "refs/heads/기능/한글\0{oid}\0commit\0\nrefs/remote/t\0{oid}\0tag\0refs/heads/x\nbroken line\n"
        );
        let refs = parse_for_each_ref(stdout.as_bytes());
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].name, "refs/heads/기능/한글");
        assert_eq!(refs[0].target.as_str(), oid);
        assert!(refs[0].is_commit && !refs[0].is_symbolic);
        assert!(!refs[1].is_commit && refs[1].is_symbolic);
    }

    /// custom namespace 설정은 `refs/remote/` 아래이면서 `/`로 끝나야 한다.
    #[test]
    fn validates_custom_namespaces() {
        assert!(is_valid_custom_namespace("refs/remote/"));
        assert!(is_valid_custom_namespace("refs/remote/agent/"));
        assert!(!is_valid_custom_namespace("refs/remote/agent"));
        assert!(!is_valid_custom_namespace("refs/remotes/"));
        assert!(!is_valid_custom_namespace("refs/heads/"));
        assert!(!is_valid_custom_namespace("refs/tags/"));
        assert!(!is_valid_custom_namespace("refs/remote/../heads/"));
        assert!(!is_valid_custom_namespace("refs/remote//"));
    }

    /// `refs/remote/*`만 custom ref로 본다.
    #[test]
    fn distinguishes_remote_and_remotes() {
        assert!(is_custom_ref("refs/remote/refactor/task001"));
        assert!(!is_custom_ref("refs/remotes/origin/main"));
        assert!(!is_custom_ref("refs/remote/"));
        assert!(!is_custom_ref("refs/heads/main"));
        assert!(!is_custom_ref("refs/remote/../heads/main"));
        assert!(!is_custom_ref("refs/remote/a//b"));
        assert!(!is_custom_ref("refs/remote/a/"));
    }

    /// 보호 namespace는 custom ref가 아니며 보호 대상으로 판별된다.
    #[test]
    fn protects_reserved_namespaces() {
        for r in [
            "refs/remotes/origin/main",
            "refs/tags/v1.0",
            "refs/notes/commits",
            "refs/stash",
        ] {
            assert!(is_protected_ref(r), "{r} must be protected");
            assert!(!is_custom_ref(r), "{r} must not be a custom ref");
        }
        assert!(!is_protected_ref("refs/remote/review/pr-123"));
    }
}
