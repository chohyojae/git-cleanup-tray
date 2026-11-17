// SPDX-License-Identifier: GPL-3.0-only

//! Clean 확인 창의 체크박스 목록과 사용자가 고른 항목을 계획에 반영하는 로직.
//!
//! 화면 줄과 계획 항목은 따로 id를 두지 않고 순서로 짝짓는다. 저장소 순서대로 worktree → local
//! branch → custom ref → 강제 삭제 후보를 나열하며, [`items`], [`counts`], [`apply`]가 모두 같은
//! 순서를 쓴다. 이 module은 Git을 실행하지 않는다.

use std::fmt::Write as _;

use crate::app::tasks::PendingCleanup;
use crate::cleanup::CleanupPlan;
use crate::cleanup::candidate::{ForceCandidate, ForceRisk};
use crate::git::repository::RepositoryId;

/// 확인 창 목록의 한 줄.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionItem {
    /// 종류와 대상 이름 (예: "Worktree + branch: feature/x").
    pub title: String,
    /// 저장소 이름, 지워도 되는 이유 또는 잃는 것, worktree 경로.
    pub note: String,
    /// 지우면 커밋하지 않은 변경이나 merge되지 않은 commit을 잃는 항목이면 `true`.
    pub risky: bool,
    /// 처음 체크 상태. 안전한 항목만 체크해 둔다.
    pub checked: bool,
}

/// 현재 체크 상태로 계산한 확인 창 숫자.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SelectionCounts {
    /// 고른 항목이 하나라도 있는 저장소 수.
    pub repositories: usize,
    /// 제거할 linked worktree 수.
    pub worktrees: usize,
    /// 삭제할 local branch 수. worktree와 함께 지울 branch도 센다 (실행 결과와 같은 기준).
    pub branches: usize,
    /// 삭제할 custom ref 수.
    pub custom_refs: usize,
    /// 고른 항목 중 강제 삭제 대상 수. 0보다 크면 확인 창이 경고를 보여준다.
    pub forced: usize,
}

/// plan 하나에서 체크박스로 보여줄 항목 수.
fn slot_count(plan: &CleanupPlan) -> usize {
    plan.worktrees.len()
        + plan.branches.len()
        + plan.custom_refs.len()
        + plan.force_candidates.len()
}

/// 체크박스로 보여줄 항목 총수. 0이면 확인 창에 고를 것이 없다.
#[must_use]
pub fn selectable_count(pending: &PendingCleanup) -> usize {
    pending.plans.iter().map(slot_count).sum()
}

/// 사용자가 골라도 지울 수 없어 계속 건너뛰는 항목 수를 구할 때 `skipped` 합계에서 뺄 값.
///
/// dirty worktree는 제외 항목으로도 세어지지만 확인 창에서는 체크박스 줄로 따로 보여주므로,
/// "Skipped" 숫자와 겹치지 않게 그만큼 뺀다.
#[must_use]
pub fn selectable_dirty_count(pending: &PendingCleanup) -> usize {
    pending
        .plans
        .iter()
        .flat_map(|plan| &plan.force_candidates)
        .filter(|candidate| candidate.risk == ForceRisk::DirtyWorktree)
        .count()
}

/// `id` 저장소의 표시 이름. 목록에 없으면 "?".
fn repository_name(pending: &PendingCleanup, id: RepositoryId) -> &str {
    pending
        .names
        .iter()
        .find(|(candidate, _)| *candidate == id)
        .map_or("?", |(_, name)| name.as_str())
}

/// 확인 창에 보여줄 줄을 만든다. 순서는 module 문서의 순서와 같다.
#[must_use]
pub fn items(pending: &PendingCleanup) -> Vec<SelectionItem> {
    let mut items = Vec::with_capacity(selectable_count(pending));
    for plan in &pending.plans {
        let repository = repository_name(pending, plan.repository_id);
        let base = plan.base.branch.as_str();
        items.extend(plan.worktrees.iter().map(|candidate| SelectionItem {
            title: format!("Worktree + branch: {}", candidate.branch),
            note: format!(
                "{repository} · Merged into {base} · {}",
                candidate.path.display()
            ),
            risky: false,
            checked: true,
        }));
        items.extend(plan.branches.iter().map(|candidate| SelectionItem {
            title: format!("Branch: {}", candidate.name),
            note: format!("{repository} · Merged into {base}"),
            risky: false,
            checked: true,
        }));
        items.extend(plan.custom_refs.iter().map(|candidate| SelectionItem {
            title: format!("Custom ref: {}", candidate.full_ref),
            note: format!("{repository} · Contained in {base}"),
            risky: false,
            checked: true,
        }));
        items.extend(
            plan.force_candidates
                .iter()
                .map(|candidate| force_item(repository, base, candidate)),
        );
    }
    items
}

/// 강제 삭제 후보 한 줄. 무엇을 잃는지 문구로 알리고 체크하지 않은 상태로 둔다.
fn force_item(repository: &str, base: &str, candidate: &ForceCandidate) -> SelectionItem {
    let kind = if candidate.worktree.is_some() {
        "Worktree + branch"
    } else {
        "Branch"
    };
    let mut note = match (candidate.risk, candidate.worktree.is_some()) {
        (ForceRisk::DirtyWorktree, _) => {
            format!("{repository} · Uncommitted changes will be permanently deleted")
        }
        (ForceRisk::NotMerged, true) => format!(
            "{repository} · Not merged into {base}: its commits and any uncommitted changes will be lost"
        ),
        (ForceRisk::NotMerged, false) => {
            format!("{repository} · Not merged into {base}: its commits may be lost")
        }
    };
    if let Some(path) = &candidate.worktree {
        // `String`에 쓰는 것은 실패하지 않는다.
        let _ = write!(note, " · {}", path.display());
    }
    SelectionItem {
        title: format!("{kind}: {}", candidate.branch),
        note,
        risky: true,
        checked: false,
    }
}

/// 체크 상태 `checked`로 확인 창 숫자를 계산한다. 길이가 항목 수와 다르면 아무것도 고르지 않은 것으로 본다.
#[must_use]
pub fn counts(pending: &PendingCleanup, checked: &[bool]) -> SelectionCounts {
    let mut totals = SelectionCounts::default();
    if checked.len() != selectable_count(pending) {
        return totals;
    }
    let mut flags = checked.iter().copied();
    let mut take = |count: usize| flags.by_ref().take(count).filter(|flag| *flag).count();
    for plan in &pending.plans {
        let worktrees = take(plan.worktrees.len());
        let branches = take(plan.branches.len());
        let custom_refs = take(plan.custom_refs.len());
        let mut forced = 0;
        let mut forced_worktrees = 0;
        for candidate in &plan.force_candidates {
            if take(1) == 1 {
                forced += 1;
                forced_worktrees += usize::from(candidate.worktree.is_some());
            }
        }
        totals.repositories += usize::from(worktrees + branches + custom_refs + forced > 0);
        totals.worktrees += worktrees + forced_worktrees;
        totals.branches += branches + worktrees + forced;
        totals.custom_refs += custom_refs;
        totals.forced += forced;
    }
    totals
}

/// 체크한 항목만 계획에 남긴다. 체크한 강제 삭제 후보는 `approved_force`로 옮긴다.
///
/// 체크하지 않은 항목은 계획에서 빠져 executor에 전달되지 않는다. 고른 것이 없는 저장소의 plan은
/// 버린다. `checked`의 길이가 항목 수와 다르면 화면과 계획이 어긋난 것이므로 아무것도 바꾸지 않고
/// `false`를 돌려준다. 이때 호출자는 실행하지 않아야 한다.
#[must_use]
pub fn apply(pending: &mut PendingCleanup, checked: &[bool]) -> bool {
    if checked.len() != selectable_count(pending) {
        return false;
    }
    let mut flags = checked.iter().copied();
    // `Vec::retain`은 원래 순서대로 한 번씩 방문하므로 flag를 차례로 꺼내면 줄과 짝이 맞는다.
    let mut keep = || flags.next().unwrap_or(false);
    for plan in &mut pending.plans {
        plan.worktrees.retain(|_| keep());
        plan.branches.retain(|_| keep());
        plan.custom_refs.retain(|_| keep());
        let mut approved = std::mem::take(&mut plan.force_candidates);
        approved.retain(|_| keep());
        plan.approved_force = approved;
    }
    pending.plans.retain(|plan| plan.removable_count() > 0);
    true
}
