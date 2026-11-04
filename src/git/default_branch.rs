// SPDX-License-Identifier: GPL-3.0-only

//! remote default branch 판별 (PLAN 7장).
//!
//! `main`/`master` 같은 이름을 추측하지 않는다. 다음 순서로 판별한다.
//! 1. 사용자 override가 있으면 그 이름을 쓴다.
//! 2. `git symbolic-ref --quiet refs/remotes/<remote>/HEAD` (network 없음)
//! 3. 없으면 (설정이 허용할 때만) `git ls-remote --symref <remote> HEAD`
//! 4. local branch가 있으면 `refs/heads/<branch>`, 없으면 `refs/remotes/<remote>/<branch>`를 base ref로 쓴다.

use std::path::Path;

use crate::error::{CleanupError, Result};
use crate::git::object::ObjectId;
use crate::git::refs::{self, LOCAL_BRANCH_PREFIX, REMOTE_TRACKING_PREFIX};
use crate::git::runner::GitRunner;

/// base branch를 어떤 방법으로 결정했는지. 로그와 상세 화면에 표시한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseBranchSource {
    /// 사용자가 저장소 설정에서 지정했다.
    Override,
    /// local `refs/remotes/<remote>/HEAD` symbolic ref에서 읽었다 (network 없음).
    LocalSymbolicRef,
    /// `git ls-remote --symref`로 remote에 질의했다.
    RemoteQuery,
}

/// merge 판정 기준이 되는 base ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseRef {
    /// local branch가 있어 `refs/heads/<branch>`를 쓴다.
    Local(String),
    /// local branch가 없어 `refs/remotes/<remote>/<branch>`를 쓴다.
    RemoteTracking(String),
}

impl BaseRef {
    /// Git 명령에 넘길 full ref 이름.
    #[must_use]
    pub fn full_ref(&self) -> &str {
        match self {
            Self::Local(r) | Self::RemoteTracking(r) => r,
        }
    }
}

/// 판별을 마친 base branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBase {
    /// 판별에 쓴 remote 이름. Execute 재검증 때 같은 remote의 `HEAD`를 다시 읽는다.
    pub remote: String,
    /// branch 이름 (예: `main`, `trunk`). `refs/heads/` 같은 prefix는 붙지 않는다.
    pub branch: String,
    /// merge 판정에 쓸 ref.
    pub base_ref: BaseRef,
    /// branch 이름을 알아낸 방법.
    pub source: BaseBranchSource,
    /// 판별 시점에 `base_ref`가 가리키던 commit.
    pub commit: ObjectId,
}

/// base branch 판별 입력. 저장소 설정과 앱 설정에서 필요한 값만 빌려 온다.
#[derive(Debug, Clone, Copy)]
pub struct BaseBranchQuery<'a> {
    /// 기준 remote 이름 (기본 `origin`).
    pub remote: &'a str,
    /// 사용자가 지정한 base branch. `None`이면 자동 판별한다.
    pub override_branch: Option<&'a str>,
    /// local symbolic ref가 없을 때 network 질의를 허용할지.
    pub allow_remote_query: bool,
}

/// PLAN 7장 순서로 base branch를 판별한다.
///
/// # Errors
///
/// - remote 질의가 필요한데 remote가 없으면 [`CleanupError::RemoteNotFound`]
/// - 질의 중 인증·network 문제면 [`CleanupError::AuthenticationFailed`] /
///   [`CleanupError::NetworkUnavailable`]
/// - 이름을 알아내지 못했거나, 이름은 알지만 local/remote-tracking ref가 모두 없으면
///   [`CleanupError::DefaultBranchUnknown`] (`git fetch`가 필요하다는 뜻)
/// - 그 밖의 Git 실패는 [`CleanupError::GitCommandFailed`]
pub fn resolve<R: GitRunner + ?Sized>(
    runner: &R,
    repository: &Path,
    query: BaseBranchQuery<'_>,
) -> Result<ResolvedBase> {
    let (branch, source) = if let Some(name) = query.override_branch {
        (name.to_owned(), BaseBranchSource::Override)
    } else if let Some(name) = read_local_symbolic_ref(runner, repository, query.remote)? {
        (name, BaseBranchSource::LocalSymbolicRef)
    } else if query.allow_remote_query {
        (
            query_remote_head(runner, repository, query.remote)?,
            BaseBranchSource::RemoteQuery,
        )
    } else {
        return Err(CleanupError::DefaultBranchUnknown(query.remote.to_owned()));
    };

    let (base_ref, commit) = find_base_ref(runner, repository, query.remote, &branch)?
        .ok_or_else(|| CleanupError::DefaultBranchUnknown(query.remote.to_owned()))?;
    Ok(ResolvedBase {
        remote: query.remote.to_owned(),
        branch,
        base_ref,
        source,
        commit,
    })
}

/// Scan 때 판별한 base가 지금도 그대로인지 확인한다 (PLAN 9장 "base branch가 변경되지 않았는가").
///
/// - base ref가 가리키는 commit이 Scan 때와 같아야 한다. base가 앞으로 움직였어도 기존 판정이
///   틀리지는 않지만, 사용자가 확인한 plan과 다른 상태에서 지우지 않도록 보수적으로 본다.
/// - local symbolic ref로 판별했다면 `refs/remotes/<remote>/HEAD`가 여전히 같은 branch여야 한다.
///   remote 질의로 판별한 경우는 network를 다시 쓰지 않고 commit만 비교한다.
///
/// # Errors
///
/// Git 실행이 실패하면 [`CleanupError::GitCommandFailed`].
pub fn revalidate<R: GitRunner + ?Sized>(
    runner: &R,
    repository: &Path,
    base: &ResolvedBase,
) -> Result<bool> {
    if base.source == BaseBranchSource::LocalSymbolicRef
        && read_local_symbolic_ref(runner, repository, &base.remote)?.as_deref()
            != Some(base.branch.as_str())
    {
        return Ok(false);
    }
    Ok(
        refs::resolve_commit(runner, repository, base.base_ref.full_ref())?.as_ref()
            == Some(&base.commit),
    )
}

/// `refs/remotes/<remote>/HEAD`가 가리키는 branch 이름을 읽는다. 없으면 `None`.
///
/// `--short`를 쓰지 않는다. short 이름은 같은 이름의 다른 ref가 있으면 모호하게 줄어들 수 있어,
/// full ref를 받아 prefix를 직접 떼는 편이 정확하다.
fn read_local_symbolic_ref<R: GitRunner + ?Sized>(
    runner: &R,
    repository: &Path,
    remote: &str,
) -> Result<Option<String>> {
    let head_ref = format!("{REMOTE_TRACKING_PREFIX}{remote}/HEAD");
    let output = runner.run(
        repository,
        &[
            "symbolic-ref".as_ref(),
            "--quiet".as_ref(),
            head_ref.as_ref(),
        ],
    )?;
    match output.exit_code {
        Some(0) => {}
        // `--quiet`에서 exit 1은 "symbolic ref가 아니거나 없음"이다. 오류가 아니라 다음 단계로 간다.
        Some(1) => return Ok(None),
        _ => return Err(output.failure("symbolic-ref")),
    }
    let target = output.stdout_lossy();
    let prefix = format!("{REMOTE_TRACKING_PREFIX}{remote}/");
    // 다른 remote나 namespace를 가리키도록 손으로 바꾼 경우는 신뢰하지 않고 다음 단계로 넘긴다.
    Ok(target
        .trim_end()
        .strip_prefix(&prefix)
        .filter(|name| !name.is_empty())
        .map(str::to_owned))
}

/// `git ls-remote --symref <remote> HEAD`로 remote의 default branch 이름을 질의한다.
///
/// 먼저 `git remote get-url`로 remote가 설정되어 있는지 확인한다. 이름이 없는 remote를
/// `ls-remote`에 넘기면 Git이 path로 해석해 URL 오류와 같은 문구를 내기 때문에 구분이 안 된다.
fn query_remote_head<R: GitRunner + ?Sized>(
    runner: &R,
    repository: &Path,
    remote: &str,
) -> Result<String> {
    let url = runner.run(
        repository,
        &["remote".as_ref(), "get-url".as_ref(), remote.as_ref()],
    )?;
    match url.exit_code {
        Some(0) => {}
        // `error: No such remote '<name>'`는 exit 2다.
        Some(2) => return Err(CleanupError::RemoteNotFound(remote.to_owned())),
        _ => return Err(url.failure("remote")),
    }
    // URL에는 credential이 들어 있을 수 있으므로 값은 쓰지 않고 버린다.
    drop(url);

    let output = runner.run(
        repository,
        &[
            "ls-remote".as_ref(),
            "--symref".as_ref(),
            remote.as_ref(),
            "HEAD".as_ref(),
        ],
    )?;
    if !output.success() {
        return Err(classify_remote_failure(&output.stderr_lossy())
            .unwrap_or_else(|| output.failure("ls-remote")));
    }
    parse_ls_remote_symref(&output.stdout_lossy())
        .map(str::to_owned)
        .ok_or_else(|| CleanupError::DefaultBranchUnknown(remote.to_owned()))
}

/// `ls-remote --symref` 출력에서 `HEAD`가 가리키는 branch 이름을 꺼낸다.
///
/// 예: `ref: refs/heads/main\tHEAD`. 빈 remote이거나 server가 symref를 알려주지 않으면 `None`.
fn parse_ls_remote_symref(stdout: &str) -> Option<&str> {
    stdout.lines().find_map(|line| {
        let (target, name) = line.strip_prefix("ref: ")?.split_once('\t')?;
        (name == "HEAD")
            .then(|| target.strip_prefix(LOCAL_BRANCH_PREFIX))
            .flatten()
            .filter(|branch| !branch.is_empty())
    })
}

/// remote 질의 실패 stderr를 인증/network 오류로 분류한다. 해당 없으면 `None`.
///
/// runner가 `LC_ALL=C`로 실행하므로 영어 문구를 기준으로 본다. 인증 문구를 먼저 본다.
/// `unable to access`처럼 둘 다에 나오는 문구가 있기 때문이다.
fn classify_remote_failure(stderr: &str) -> Option<CleanupError> {
    /// 인증 실패를 뜻하는 stderr 문구.
    const AUTH: [&str; 6] = [
        "Authentication failed",
        "could not read Username",
        "terminal prompts disabled",
        "Permission denied (publickey",
        "Host key verification failed",
        "Access denied",
    ];
    /// network 연결 실패를 뜻하는 stderr 문구.
    const NETWORK: [&str; 6] = [
        "Could not resolve host",
        "Connection refused",
        "timed out",
        "Network is unreachable",
        "Failed to connect",
        "unable to access",
    ];
    if AUTH.iter().any(|p| stderr.contains(p)) {
        Some(CleanupError::AuthenticationFailed)
    } else if NETWORK.iter().any(|p| stderr.contains(p)) {
        Some(CleanupError::NetworkUnavailable)
    } else {
        None
    }
}

/// branch 이름으로 base ref와 그 commit을 찾는다. local branch를 우선한다.
///
/// 두 후보를 `for-each-ref` 한 번으로 조회한다. pattern은 `/` 경계 prefix로도 일치하므로
/// (`refs/heads/main`이 `refs/heads/main/x`도 잡는다) 이름이 정확히 같은 것만 고른다.
fn find_base_ref<R: GitRunner + ?Sized>(
    runner: &R,
    repository: &Path,
    remote: &str,
    branch: &str,
) -> Result<Option<(BaseRef, ObjectId)>> {
    let local = format!("{LOCAL_BRANCH_PREFIX}{branch}");
    let tracking = format!("{REMOTE_TRACKING_PREFIX}{remote}/{branch}");
    let found = refs::list_refs(runner, repository, &[&local, &tracking], None)?;

    let pick = |name: &str| {
        found
            .iter()
            .find(|entry| entry.name == name && entry.is_commit)
            .map(|entry| entry.target.clone())
    };
    if let Some(commit) = pick(&local) {
        return Ok(Some((BaseRef::Local(local), commit)));
    }
    Ok(pick(&tracking).map(|commit| (BaseRef::RemoteTracking(tracking), commit)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ref: refs/heads/<name>\tHEAD` 줄에서 branch 이름을 읽는다. 이름을 추측하지 않는다.
    #[test]
    fn parses_ls_remote_symref() {
        let oid = "e6ec21a92ade4f3a94039927941b1c3c3929b930";
        for branch in ["main", "master", "trunk", "release/2026-개발"] {
            let stdout = format!("ref: refs/heads/{branch}\tHEAD\n{oid}\tHEAD\n");
            assert_eq!(parse_ls_remote_symref(&stdout), Some(branch));
        }
        // symref를 알려주지 않는 server, 빈 remote
        assert_eq!(parse_ls_remote_symref(&format!("{oid}\tHEAD\n")), None);
        assert_eq!(parse_ls_remote_symref(""), None);
        // HEAD가 아닌 symref 줄은 무시한다.
        assert_eq!(
            parse_ls_remote_symref("ref: refs/heads/x\trefs/heads/y\n"),
            None
        );
    }

    /// 인증 실패와 network 실패 문구를 구분한다.
    #[test]
    fn classifies_remote_failures() {
        assert!(matches!(
            classify_remote_failure(
                "fatal: could not read Username for 'https://example.com': terminal prompts disabled"
            ),
            Some(CleanupError::AuthenticationFailed)
        ));
        assert!(matches!(
            classify_remote_failure("git@example.com: Permission denied (publickey)."),
            Some(CleanupError::AuthenticationFailed)
        ));
        assert!(matches!(
            classify_remote_failure(
                "fatal: unable to access 'https://example.com/': Could not resolve host: example.com"
            ),
            Some(CleanupError::NetworkUnavailable)
        ));
        assert!(classify_remote_failure("fatal: something else").is_none());
    }
}
