// SPDX-License-Identifier: GPL-3.0-only

//! Slint UI binding.
//!
//! `.slint`에서 생성된 component를 노출하고, application state를 Slint model로 바꾸는
//! 변환 코드를 둔다. 이 module에서 Git을 직접 실행하지 않는다.

pub mod bridge;
pub mod models;

/// `build.rs`가 `ui/app.slint`에서 생성한 code.
///
/// 생성 code는 우리 주석·lint 규칙의 대상이 아니므로 lint를 끈다.
#[allow(
    missing_docs,
    clippy::all,
    clippy::pedantic,
    clippy::missing_docs_in_private_items,
    clippy::unwrap_used,
    clippy::todo
)]
mod generated {
    slint::include_modules!();
}

pub use generated::{
    AboutWindow, AppTray, CleanupDialog, DetailItem, DetailSection, DetailWindow, DialogStage,
    MainWindow, RepositoryEditor, RepositoryRowData, SettingsWindow,
};
