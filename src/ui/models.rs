// SPDX-License-Identifier: GPL-3.0-only

//! application state → Slint model 변환.
//!
//! 목록은 `VecModel`을 재사용해 바뀐 줄만 갱신하고, 전체 model을 매번 새로 만들지 않는다
//! (AGENTS 4.2). 같은 값을 다시 넣으면 Slint가 해당 줄을 다시 그리므로 값이 다를 때만 쓴다.

use slint::{Model, VecModel};

use crate::app::state::RepositoryEntry;
use crate::app::text;
use crate::ui::RepositoryRowData;

/// 저장소 목록을 화면 줄로 바꾼다. 순서는 `entries`와 같아 줄 index로 저장소를 찾을 수 있다.
#[must_use]
pub fn repository_rows(entries: &[RepositoryEntry], now: u64) -> Vec<RepositoryRowData> {
    let home = dirs::home_dir();
    entries
        .iter()
        .map(|entry| RepositoryRowData {
            name: entry.config.name.as_str().into(),
            path: text::abbreviate_path(&entry.config.path, home.as_deref()).into(),
            base: entry.summary.base.as_deref().unwrap_or_default().into(),
            status: text::repository_status(&entry.summary.outcome).into(),
            activity: text::repository_activity(&entry.summary, now).into(),
            enabled: entry.config.enabled,
            scan_pending: false,
        })
        .collect()
}

/// `model`을 `rows`와 같게 만든다. 값이 바뀐 줄만 쓰고, 길이 차이만큼 끝에서 넣거나 뺀다.
pub fn sync_rows(model: &VecModel<RepositoryRowData>, rows: Vec<RepositoryRowData>) {
    let old_len = model.row_count();
    let new_len = rows.len();
    for (index, row) in rows.into_iter().enumerate() {
        if index >= old_len {
            model.push(row);
        } else if model.row_data(index).as_ref() != Some(&row) {
            model.set_row_data(index, row);
        }
    }
    for index in (new_len..old_len).rev() {
        model.remove(index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// test용 줄.
    fn row(name: &str) -> RepositoryRowData {
        RepositoryRowData {
            name: name.into(),
            ..RepositoryRowData::default()
        }
    }

    /// 늘어나고 줄어드는 경우 모두 결과가 `rows`와 같은지 확인한다.
    #[test]
    fn sync_rows_matches_target() {
        let model = VecModel::from(vec![row("a"), row("b")]);
        sync_rows(&model, vec![row("a"), row("B"), row("c")]);
        let names: Vec<_> = model.iter().map(|r| r.name.to_string()).collect();
        assert_eq!(names, ["a", "B", "c"]);

        sync_rows(&model, vec![row("x")]);
        let names: Vec<_> = model.iter().map(|r| r.name.to_string()).collect();
        assert_eq!(names, ["x"]);
    }
}
