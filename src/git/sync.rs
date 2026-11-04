// SPDX-License-Identifier: GPL-3.0-only

//! remote를 fetch하고 upstream이 있는 로컬 브랜치를 fast-forward로만 최신화한다.
//! 정리와 달리 깨끗한 main worktree도 갱신하지만, dirty/locked worktree는 변경하지 않는다.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;

use crate::error::{CleanupError, Result};
use crate::git::object::ObjectId;
use crate::git::runner::{GitOutput, GitRunner};
use crate::git::{refs, repository, worktree};

/// 최신화 결과. fetch 자체의 실패는 이 값 대신 오류로 반환한다.
#[derive(Debug, Default)]
pub struct SyncResult {
    /// fast-forward한 로컬 브랜치 수.
    pub updated: usize,
    /// 안전 조건이나 upstream 설정 때문에 갱신하지 않은 브랜치와 이유.
    pub skipped: Vec<String>,
    /// 브랜치별 Git 실행 오류. 다른 브랜치는 계속 처리한다.
    pub errors: Vec<CleanupError>,
}

/// 최신화 직전에 다시 확인할 로컬 브랜치 상태.
struct Branch {
    /// `refs/heads/`로 시작하는 이름.
    name: String,
    /// 처음 읽은 commit. 갱신 전에 동일한지 확인한다.
    commit: ObjectId,
    /// Git 설정에서 읽은 upstream ref.
    upstream: String,
    /// upstream remote 이름. 다른 remote나 로컬 upstream은 갱신하지 않는다.
    remote: String,
}

/// remote를 fetch한 뒤, 해당 remote의 upstream을 추적하는 브랜치만 최신화한다.
///
/// 원격 추적 ref만 fetch하며 prune/tag 갱신은 하지 않는다. 로컬 변경을 버리거나 merge commit을
/// 만들지 않고, checkout하지 않은 브랜치는 Git의 fetch 검사로 checkout 경합까지 방어한다.
///
/// # Errors
///
/// 저장소나 remote 이름이 잘못되었거나 fetch/ref 조회에 실패하면 해당 오류.
pub fn synchronize<R: GitRunner + ?Sized>(
    runner: &R,
    path: &Path,
    remote: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<SyncResult> {
    if cancelled() {
        return Ok(SyncResult::default());
    }
    repository::identify(runner, path)?;
    let destination = format!("refs/remotes/{remote}/check");
    if remote.is_empty() || remote.starts_with('-') || remote == "." {
        return Err(CleanupError::RemoteNotFound(remote.to_owned()));
    }
    checked(
        runner,
        path,
        &["check-ref-format".as_ref(), destination.as_ref()],
    )?;
    checked(
        runner,
        path,
        &[
            "remote".as_ref(),
            "get-url".as_ref(),
            "--".as_ref(),
            remote.as_ref(),
        ],
    )?;
    let tracking_prefix = format!("refs/remotes/{remote}/");
    let tracking_head = format!("{tracking_prefix}HEAD");
    for entry in refs::list_refs(runner, path, &[&tracking_prefix], None)? {
        if entry.is_symbolic && entry.name != tracking_head {
            return Err(CleanupError::StateChanged(format!(
                "symbolic remote-tracking ref cannot be fetched safely: {}",
                entry.name,
            )));
        }
    }
    // 설정된 refspec/prune/mirror로 로컬 branch나 tag가 바뀌지 않도록 목적지를 제한한다.
    let refspec = format!("+refs/heads/*:refs/remotes/{remote}/*");
    checked(
        runner,
        path,
        &[
            "fetch".as_ref(),
            "--show-forced-updates".as_ref(),
            "--no-prune".as_ref(),
            "--no-prune-tags".as_ref(),
            "--no-tags".as_ref(),
            "--no-recurse-submodules".as_ref(),
            "--no-write-fetch-head".as_ref(),
            "--no-auto-maintenance".as_ref(),
            "--refmap=".as_ref(),
            "--".as_ref(),
            remote.as_ref(),
            refspec.as_ref(),
            // 일반적인 symbolic HEAD는 fetch 목적지에서 제외해 따라가 갱신하지 않는다.
            "^refs/heads/HEAD".as_ref(),
        ],
    )?;
    let branches = read_branches(runner, path)?;
    let targets: HashMap<_, _> = refs::list_refs(runner, path, &["refs/remotes/"], None)?
        .into_iter()
        .filter(|entry| entry.is_commit && !entry.is_symbolic)
        .map(|entry| (entry.name, entry.target))
        .collect();
    let mut result = SyncResult::default();
    for branch in branches {
        if cancelled() {
            break;
        }
        if branch.remote != remote || branch.upstream.is_empty() {
            result
                .skipped
                .push(format!("{}: no upstream on {remote}", branch.name));
            continue;
        }
        let Some(target) = targets.get(&branch.upstream) else {
            result
                .skipped
                .push(format!("{}: upstream is missing", branch.name));
            continue;
        };
        if &branch.commit == target {
            continue;
        }
        match update_branch(runner, path, &branch, target) {
            Ok(None) => result.updated += 1,
            Ok(Some(reason)) => result.skipped.push(format!("{}: {reason}", branch.name)),
            Err(error) => result.errors.push(error),
        }
    }
    Ok(result)
}

/// upstream과 commit을 한 번에 읽는다. symbolic branch는 갱신 대상에서 제외한다.
fn read_branches<R: GitRunner + ?Sized>(runner: &R, path: &Path) -> Result<Vec<Branch>> {
    let output = checked(runner, path, &[
        "for-each-ref".as_ref(),
        "--format=%(refname)%00%(objectname)%00%(upstream:remoteref)%00%(upstream:remotename)%00%(symref)".as_ref(),
        "refs/heads/".as_ref(),
    ])?;
    Ok(output
        .stdout_lossy()
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let name = fields.next()?.to_owned();
            let commit = ObjectId::parse(fields.next()?)?;
            let source = fields.next()?;
            let remote = fields.next()?.to_owned();
            // fetch refspec이 로컬 branch를 가리켜도 방금 fetch한 tracking ref만 사용한다.
            let upstream = source
                .strip_prefix("refs/heads/")
                .map_or_else(String::new, |branch| {
                    format!("refs/remotes/{remote}/{branch}")
                });
            if !fields.next()?.is_empty() {
                return None;
            }
            Some(Branch {
                name,
                commit,
                upstream,
                remote,
            })
        })
        .collect())
}

/// branch/HEAD/worktree 상태를 재검사한 후 fast-forward한다. 안전하게 갱신할 수 없으면 이유를 반환한다.
fn update_branch<R: GitRunner + ?Sized>(
    runner: &R,
    path: &Path,
    branch: &Branch,
    target: &ObjectId,
) -> Result<Option<&'static str>> {
    let ancestor = runner.run(
        path,
        &[
            "merge-base".as_ref(),
            "--is-ancestor".as_ref(),
            branch.commit.as_ref(),
            target.as_ref(),
        ],
    )?;
    match ancestor.exit_code {
        Some(0) => {}
        Some(1) => return Ok(Some("local commits are ahead or diverged")),
        _ => return Err(ancestor.failure("merge-base")),
    }
    if refs::resolve_commit(runner, path, &branch.name)?.as_ref() != Some(&branch.commit) {
        return Ok(Some("branch changed during update"));
    }
    let symbolic = runner.run(
        path,
        &[
            "symbolic-ref".as_ref(),
            "--quiet".as_ref(),
            branch.name.as_ref(),
        ],
    )?;
    match symbolic.exit_code {
        Some(0) => return Ok(Some("branch became symbolic during update")),
        Some(1) => {}
        _ => return Err(symbolic.failure("symbolic-ref")),
    }
    let worktrees = worktree::list(runner, path)?;
    if let Some(tree) = worktrees
        .iter()
        .find(|tree| tree.branch.as_deref() == Some(&branch.name))
    {
        return update_worktree(runner, path, branch, target, tree);
    }
    // `fetch`는 다른 worktree에 checkout된 branch도 거부하므로 `update-ref`보다 안전하다.
    // `+`를 붙이지 않아 확인 후 branch가 진행되어도 non-fast-forward 갱신을 거부한다.
    let refspec = format!("{}:{}", target.as_str(), branch.name);
    checked(
        runner,
        path,
        &[
            "fetch".as_ref(),
            "--no-tags".as_ref(),
            "--show-forced-updates".as_ref(),
            "--no-prune".as_ref(),
            "--no-prune-tags".as_ref(),
            "--no-recurse-submodules".as_ref(),
            "--no-write-fetch-head".as_ref(),
            "--no-auto-maintenance".as_ref(),
            "--refmap=".as_ref(),
            "--".as_ref(),
            ".".as_ref(),
            refspec.as_ref(),
        ],
    )?;
    Ok(None)
}

/// checkout된 worktree의 파일과 진행 중인 Git 작업을 보호한 뒤 fast-forward한다.
fn update_worktree<R: GitRunner + ?Sized>(
    runner: &R,
    path: &Path,
    branch: &Branch,
    target: &ObjectId,
    tree: &worktree::WorktreeEntry,
) -> Result<Option<&'static str>> {
    if tree.locked.is_some() || tree.prunable.is_some() || !tree.path.is_dir() {
        return Ok(Some("worktree is locked or unavailable"));
    }
    if !worktree::is_clean(runner, &tree.path)? {
        return Ok(Some("worktree contains uncommitted or untracked changes"));
    }
    let identity = repository::identify(runner, &tree.path)?;
    // clean 상태인 rebase/cherry-pick 중에도 HEAD를 움직이지 않도록 내부 상태를 읽기만 한다.
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
        "BISECT_LOG",
    ] {
        if identity.git_dir.join(marker).try_exists()? {
            return Ok(Some("another Git operation is in progress"));
        }
    }
    let head = checked(
        runner,
        &tree.path,
        &["symbolic-ref".as_ref(), "--quiet".as_ref(), "HEAD".as_ref()],
    )?;
    if head.stdout_lossy().trim_end() != branch.name
        || refs::resolve_commit(runner, &tree.path, "HEAD")?.as_ref() != Some(&branch.commit)
    {
        return Ok(Some("worktree changed during update"));
    }
    // status 확인 중 다른 도구가 worktree를 잠그거나 branch를 바꾸었는지도 확인한다.
    let fresh = worktree::list(runner, path)?;
    if !fresh.iter().any(|current| {
        current.path == tree.path
            && current.branch.as_deref() == Some(branch.name.as_str())
            && current.head.as_ref() == Some(&branch.commit)
            && current.locked.is_none()
            && current.prunable.is_none()
    }) {
        return Ok(Some("worktree changed during update"));
    }
    // `merge.autostash` 설정과 관계없이 stash를 만들지 않고 ignore된 파일도 보호한다.
    checked(
        runner,
        &tree.path,
        &[
            "merge".as_ref(),
            "--ff-only".as_ref(),
            "--no-autostash".as_ref(),
            "--no-overwrite-ignore".as_ref(),
            "--no-edit".as_ref(),
            "--".as_ref(),
            target.as_ref(),
        ],
    )?;
    Ok(None)
}

/// Git 실패를 오류로 바꿔 후속 변경을 막는다.
fn checked<R: GitRunner + ?Sized>(runner: &R, path: &Path, args: &[&OsStr]) -> Result<GitOutput> {
    let output = runner.run(path, args)?;
    if output.success() {
        Ok(output)
    } else {
        Err(output.failure(&crate::git::runner::describe_subcommand(args)))
    }
}
