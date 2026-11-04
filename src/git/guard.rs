// SPDX-License-Identifier: GPL-3.0-only

//! 파괴적 Git 명령 차단 guard (PLAN 24장 안전 불변조건).
//!
//! planner/executor에 버그가 있더라도 금지 명령이 실제 process로 나가지 않도록 runner 직전에서
//! 한 번 더 막는 마지막 방어선이다. 이 module의 규칙은 설정으로 끌 수 없다.

use std::ffi::OsStr;

use crate::error::{CleanupError, Result};
use crate::git::refs;

/// subcommand 앞에 올 수 있고 값을 하나 더 받는 git global option.
const GLOBAL_OPTIONS_WITH_VALUE: [&str; 4] = ["-C", "-c", "--git-dir", "--work-tree"];

/// 이 앱이 어떤 경우에도 실행하지 않는 subcommand.
///
/// `push`(remote branch 삭제 포함), `reset`, `clean`, `stash`(자동 stash 금지)는 정리 작업에
/// 필요하지 않으므로 option과 관계없이 통째로 막는다.
const FORBIDDEN_SUBCOMMANDS: [&str; 4] = ["push", "reset", "clean", "stash"];

/// git 인자 목록에서 global option을 건너뛴 첫 subcommand를 찾는다.
#[must_use]
pub fn subcommand<'a>(args: &[&'a OsStr]) -> Option<&'a OsStr> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let text = arg.to_str()?;
        if GLOBAL_OPTIONS_WITH_VALUE.contains(&text) {
            // option 값은 subcommand가 아니므로 함께 건너뛴다.
            iter.next();
        } else if !text.starts_with('-') {
            return Some(arg);
        }
    }
    None
}

/// 금지된 Git 명령이면 [`CleanupError::ForbiddenCommand`]를 반환한다.
///
/// 차단 대상:
/// - `push`, `reset`, `clean`, `stash` 전체
/// - `branch`의 강제 삭제 (`-D`, `-f`, `--force`, `-dD` 같은 short option 묶음 포함)
/// - `branch -r`/`--remotes` (`branch -d -r`는 `refs/remotes/*`를 지운다)
/// - `worktree remove --force` / `-f`
/// - `update-ref -d`로 `refs/remote/*` 밖의 ref를 지우거나, `--no-deref` 없이 지우는 경우
///
/// Git은 long option의 모호하지 않은 앞부분만 써도 받아들이므로 (`--forc` = `--force`)
/// long option은 앞부분 일치로 판별한다.
///
/// # Errors
///
/// 위 규칙에 걸리면 [`CleanupError::ForbiddenCommand`].
pub fn check_forbidden(args: &[&OsStr]) -> Result<()> {
    let Some(sub) = subcommand(args) else {
        return Ok(());
    };
    let sub_text = sub.to_string_lossy();
    let rest = rest_after(args, sub);

    let forbidden = FORBIDDEN_SUBCOMMANDS.contains(&sub_text.as_ref())
        || (sub_text == "branch"
            && (has_option(rest, &['D', 'f'], "--force") || has_option(rest, &['r'], "--remotes")))
        || (sub_text == "worktree"
            && rest.first().is_some_and(|a| *a == "remove")
            && has_option(rest, &['f'], "--force"))
        || (sub_text == "update-ref" && deletes_non_custom_ref(rest));

    if forbidden {
        Err(CleanupError::ForbiddenCommand(
            args.iter()
                .map(|a| a.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" "),
        ))
    } else {
        Ok(())
    }
}

/// `args`에서 `sub` 다음 인자들을 slice로 돌려준다. 복사하지 않는다.
fn rest_after<'s, 'a>(args: &'s [&'a OsStr], sub: &OsStr) -> &'s [&'a OsStr] {
    // subcommand()가 찾은 위치와 같도록 pointer 비교로 정확한 위치를 찾는다.
    let index = args
        .iter()
        .position(|a| std::ptr::eq(*a, sub))
        .map_or(args.len(), |i| i + 1);
    &args[index..]
}

/// `--` 이전 option 중 지정한 short flag 또는 long option(앞부분만 쓴 형태 포함)이 있으면 `true`.
///
/// `--` 뒤는 branch 이름이나 path이므로 검사하지 않는다 (`-D`라는 이름의 branch를 오탐하지 않기 위함).
fn has_option(rest: &[&OsStr], short_flags: &[char], long: &str) -> bool {
    rest.iter()
        .map(|a| a.to_string_lossy())
        .take_while(|a| a != "--")
        .any(|a| {
            if a.starts_with("--") {
                is_long_option_prefix(&a, long)
            } else {
                a.starts_with('-') && a.chars().skip(1).any(|c| short_flags.contains(&c))
            }
        })
}

/// `arg`가 long option `long`을 가리키면 `true`. `=` 뒤 값은 무시하고, 앞부분만 쓴 형태도 포함한다.
///
/// `--f`처럼 여러 option과 겹치는 짧은 형태도 막는다. Git은 그런 인자를 모호하다며 거부하므로
/// 막아도 정상 명령에는 영향이 없다.
fn is_long_option_prefix(arg: &str, long: &str) -> bool {
    let name = arg.split_once('=').map_or(arg, |(name, _)| name);
    name.len() > 2 && long.starts_with(name)
}

/// `update-ref -d <ref>`가 custom ref namespace 밖을 지우거나 `--no-deref` 없이 지우면 `true`.
///
/// `--no-deref`가 없으면 `update-ref -d`는 symbolic ref를 따라가 가리키는 대상을 지운다.
/// `refs/remote/x`가 `refs/heads/main`을 가리키는 symbolic ref라면 `main`이 지워진다.
fn deletes_non_custom_ref(rest: &[&OsStr]) -> bool {
    let is_delete = rest.iter().any(|a| *a == "-d" || *a == "--delete");
    if !is_delete {
        return false;
    }
    let no_deref = rest.iter().any(|a| *a == "--no-deref");
    // option이 아닌 첫 인자가 삭제 대상 ref다.
    let target_is_custom = rest
        .iter()
        .map(|a| a.to_string_lossy())
        .find(|a| !a.starts_with('-'))
        .is_some_and(|target| refs::is_custom_ref(&target));
    !(no_deref && target_is_custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 문자열 인자 목록으로 guard를 실행하는 test helper.
    fn check(args: &[&str]) -> Result<()> {
        let os: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        check_forbidden(&os)
    }

    /// 정리 작업에 필요한 정상 명령은 통과해야 한다.
    #[test]
    fn allows_safe_commands() {
        assert!(check(&["-C", "/repo", "branch", "-d", "--", "feature/x"]).is_ok());
        assert!(check(&["worktree", "remove", "--", "/tmp/wt"]).is_ok());
        assert!(check(&["update-ref", "--no-deref", "-d", "refs/remote/task/001"]).is_ok());
        assert!(check(&["branch", "--format=%(refname)", "--list"]).is_ok());
        assert!(check(&["merge-base", "--is-ancestor", "a", "b"]).is_ok());
    }

    /// `--` 뒤의 branch 이름은 option으로 해석하지 않는다.
    #[test]
    fn ignores_flags_after_double_dash() {
        assert!(check(&["branch", "-d", "--", "-D"]).is_ok());
    }

    /// 강제 삭제 option은 형태와 관계없이 막는다.
    #[test]
    fn blocks_force_branch_deletion() {
        assert!(check(&["branch", "-D", "x"]).is_err());
        assert!(check(&["branch", "-dD", "x"]).is_err());
        assert!(check(&["branch", "-d", "--force", "x"]).is_err());
        assert!(check(&["-C", "/repo", "branch", "-d", "-f", "x"]).is_err());
        // Git은 long option 앞부분만 써도 받아들인다. 실제로 `--forc`로 강제 삭제가 된다.
        assert!(check(&["branch", "-d", "--forc", "x"]).is_err());
        assert!(check(&["branch", "-d", "--fo", "x"]).is_err());
        assert!(check(&["branch", "--delete", "--force", "x"]).is_err());
    }

    /// `branch -d -r`는 `refs/remotes/*`를 지우므로 `-r`/`--remotes`를 막는다.
    #[test]
    fn blocks_remote_tracking_branch_deletion() {
        assert!(check(&["branch", "-d", "-r", "origin/x"]).is_err());
        assert!(check(&["branch", "-dr", "origin/x"]).is_err());
        assert!(check(&["branch", "-d", "--remotes", "origin/x"]).is_err());
        assert!(check(&["branch", "-d", "--remo", "origin/x"]).is_err());
    }

    /// `worktree remove --force`를 막는다.
    #[test]
    fn blocks_forced_worktree_removal() {
        assert!(check(&["worktree", "remove", "--force", "/tmp/wt"]).is_err());
        assert!(check(&["worktree", "remove", "-f", "/tmp/wt"]).is_err());
        assert!(check(&["worktree", "remove", "--forc", "/tmp/wt"]).is_err());
        assert!(check(&["worktree", "remove", "-ff", "/tmp/wt"]).is_err());
    }

    /// 정리에 필요 없는 위험 subcommand는 통째로 막는다.
    #[test]
    fn blocks_forbidden_subcommands() {
        assert!(check(&["push", "--delete", "origin", "x"]).is_err());
        assert!(check(&["reset", "--hard"]).is_err());
        assert!(check(&["clean", "-fd"]).is_err());
        assert!(check(&["stash"]).is_err());
    }

    /// `refs/remotes/*` 등 custom namespace 밖의 ref 삭제를 막는다.
    #[test]
    fn blocks_update_ref_outside_custom_namespace() {
        assert!(check(&["update-ref", "-d", "refs/remotes/origin/main"]).is_err());
        assert!(check(&["update-ref", "-d", "refs/heads/main"]).is_err());
        assert!(check(&["update-ref", "-d", "refs/tags/v1"]).is_err());
        assert!(
            check(&[
                "update-ref",
                "--no-deref",
                "-d",
                "refs/remote/../heads/main"
            ])
            .is_err()
        );
        assert!(check(&["update-ref", "--no-deref", "-d"]).is_err());
    }

    /// symbolic ref를 따라가 대상을 지우지 않도록 `--no-deref` 없는 삭제를 막는다.
    #[test]
    fn requires_no_deref_for_ref_deletion() {
        assert!(check(&["update-ref", "-d", "refs/remote/task/001"]).is_err());
        assert!(check(&["update-ref", "-d", "refs/remote/task/001", "--no-deref"]).is_ok());
    }
}
