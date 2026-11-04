// SPDX-License-Identifier: GPL-3.0-only

//! 사용자에게 보여줄 짧은 문구 (tray "Last result", 저장소 상태, 건너뛴 이유 등).
//!
//! UI 문구는 모두 여기서 만들어 같은 상태가 화면마다 다르게 표현되지 않게 한다.

use std::path::Path;

use crate::app::state::{RepositorySummary, SummaryOutcome};
use crate::cleanup::candidate::SkipReason;
use crate::jobs::batch::{PlanTotals, ResultTotals};

/// 건너뛴 이유를 사용자에게 보여줄 짧은 문구로 바꾼다.
#[must_use]
pub fn skip_reason(reason: SkipReason) -> &'static str {
    match reason {
        SkipReason::NotMerged => "Not merged",
        SkipReason::Protected => "Protected",
        SkipReason::NotACommit => "Not a commit",
        SkipReason::SymbolicRef => "Symbolic ref",
        SkipReason::CheckedOutInMainWorktree => "Checked out in the main worktree",
        SkipReason::CurrentWorktree => "Registered worktree",
        SkipReason::DirtyWorktree => "Worktree contains uncommitted or untracked changes",
        SkipReason::LockedWorktree => "Worktree is locked",
        SkipReason::MissingWorktreePath => "Worktree folder is missing",
        SkipReason::StateChanged => "Changed since scan",
    }
}

/// Scan 결과를 tray "Last result" 한 줄로 요약한다.
#[must_use]
pub fn scan_summary(scanned: usize, totals: &PlanTotals) -> String {
    if scanned == 0 {
        return String::from("No repositories to scan");
    }
    let repositories = plural(scanned, "repository", "repositories");
    let removable = totals.removable();
    let mut text = if removable == 0 {
        format!("{repositories}, nothing to clean")
    } else {
        format!("{repositories}, {removable} removable")
    };
    if totals.errors > 0 {
        push_count(&mut text, totals.errors, "error", "errors");
    }
    text
}

/// Clean 결과를 tray "Last result" 한 줄로 요약한다.
#[must_use]
pub fn cleanup_summary(totals: &ResultTotals, cancelled: bool) -> String {
    let mut text = format!("Removed {}", plural(totals.removed(), "item", "items"));
    if totals.skipped > 0 {
        push_count(&mut text, totals.skipped, "skipped", "skipped");
    }
    if totals.errors > 0 {
        push_count(&mut text, totals.errors, "error", "errors");
    }
    if cancelled {
        text.push_str(" (cancelled)");
    }
    text
}

/// 저장소 줄의 상태 문구 (PLAN 10장: "3 removable / 0 warning", "Clean").
#[must_use]
pub fn repository_status(outcome: &SummaryOutcome) -> String {
    match outcome {
        SummaryOutcome::Updated {
            updated,
            skipped,
            errors,
        } => format!("Updated {updated} / {skipped} skipped / {errors} errors"),
        SummaryOutcome::NotScanned => String::from("Not scanned"),
        SummaryOutcome::Scanned {
            removable: 0,
            warnings: 0,
        } => String::from("Clean"),
        SummaryOutcome::Scanned {
            removable,
            warnings,
        } => format!(
            "{removable} removable / {}",
            plural(*warnings, "warning", "warnings")
        ),
        SummaryOutcome::Failed(message) => format!("Error: {message}"),
        SummaryOutcome::Cleaned {
            removed,
            skipped,
            errors,
        } => {
            let mut text = format!("Removed {removed}");
            if *skipped > 0 {
                push_count(&mut text, *skipped, "skipped", "skipped");
            }
            if *errors > 0 {
                push_count(&mut text, *errors, "error", "errors");
            }
            text
        }
    }
}

/// 저장소 줄의 마지막 작업 시각 문구 ("Scanned 5 min ago · Cleaned 2 h ago").
#[must_use]
pub fn repository_activity(summary: &RepositorySummary, now: u64) -> String {
    let scan = summary
        .last_scan
        .map(|at| format!("Scanned {}", relative_time(now, at)));
    let cleanup = summary
        .last_cleanup
        .map(|at| format!("Cleaned {}", relative_time(now, at)));
    match (scan, cleanup) {
        (Some(scan), Some(cleanup)) => format!("{scan} · {cleanup}"),
        (Some(text), None) | (None, Some(text)) => text,
        (None, None) => String::new(),
    }
}

/// `then`이 `now`로부터 얼마나 전인지 사람이 읽기 쉬운 문구로 바꾼다.
///
/// 시간대 처리 dependency 없이 쓸 수 있도록 절대 시각 대신 상대 시각을 쓴다. 창을 열어 둔 동안
/// 문구를 갱신하는 timer는 두지 않는다 (AGENTS 4.1 idle timer 금지).
#[must_use]
pub fn relative_time(now: u64, then: u64) -> String {
    let elapsed = now.saturating_sub(then);
    match elapsed {
        0..60 => String::from("just now"),
        60..3_600 => format!("{} min ago", elapsed / 60),
        3_600..86_400 => format!("{} h ago", elapsed / 3_600),
        _ => {
            plural(
                usize::try_from(elapsed / 86_400).unwrap_or(usize::MAX),
                "day",
                "days",
            ) + " ago"
        }
    }
}

/// home directory 아래 경로를 `~`로 줄인다. 그 밖의 경로는 그대로 둔다.
#[must_use]
pub fn abbreviate_path(path: &Path, home: Option<&Path>) -> String {
    if let Some(rest) = home.and_then(|home| path.strip_prefix(home).ok()) {
        if rest.as_os_str().is_empty() {
            return String::from("~");
        }
        return format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display());
    }
    path.display().to_string()
}

/// 개수와 단수/복수 명사를 붙인다 (예: `1 item`, `2 items`).
#[must_use]
pub fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// 요약 문구 끝에 `, <개수> <명사>`를 붙인다.
fn push_count(text: &mut String, count: usize, one: &str, many: &str) {
    text.push_str(", ");
    text.push_str(&plural(count, one, many));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 경과 시간 구간별 문구를 확인한다. 미래 시각(시계 변경)은 "just now"로 본다.
    #[test]
    fn relative_time_buckets() {
        assert_eq!(relative_time(1_000, 1_000), "just now");
        assert_eq!(relative_time(1_000, 2_000), "just now");
        assert_eq!(relative_time(1_000, 1_000 - 59), "just now");
        assert_eq!(relative_time(1_000, 1_000 - 60), "1 min ago");
        assert_eq!(relative_time(10_000, 10_000 - 7_200), "2 h ago");
        assert_eq!(relative_time(200_000, 200_000 - 86_400), "1 day ago");
        assert_eq!(relative_time(400_000, 400_000 - 3 * 86_400), "3 days ago");
    }

    /// home 아래 경로만 `~`로 줄이는지 확인한다.
    #[test]
    fn abbreviates_home_paths() {
        let home = Path::new("/Users/사용자");
        assert_eq!(abbreviate_path(home, Some(home)), "~");
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(
            abbreviate_path(&home.join("작업 폴더"), Some(home)),
            format!("~{sep}작업 폴더")
        );
        assert_eq!(
            abbreviate_path(Path::new("/opt/repo"), Some(home)),
            "/opt/repo"
        );
        assert_eq!(abbreviate_path(Path::new("/opt/repo"), None), "/opt/repo");
    }

    /// 저장소 상태 문구가 PLAN 10장 예시 형식인지 확인한다.
    #[test]
    fn repository_status_texts() {
        let scanned = |removable, warnings| SummaryOutcome::Scanned {
            removable,
            warnings,
        };
        assert_eq!(repository_status(&scanned(0, 0)), "Clean");
        assert_eq!(
            repository_status(&scanned(3, 0)),
            "3 removable / 0 warnings"
        );
        assert_eq!(repository_status(&scanned(2, 1)), "2 removable / 1 warning");
        assert_eq!(
            repository_status(&SummaryOutcome::Cleaned {
                removed: 4,
                skipped: 1,
                errors: 0
            }),
            "Removed 4, 1 skipped"
        );
    }
}
