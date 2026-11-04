// SPDX-License-Identifier: GPL-3.0-only

//! Git Cleanup Tray core library.
//!
//! 등록한 Git 저장소에서 base branch에 이미 병합된 local branch, linked worktree,
//! `refs/remote/*` custom ref를 안전하게 정리한다. GUI 없이 integration test에서
//! core를 검증할 수 있도록 binary(`main.rs`)와 분리된 library로 둔다.
//!
//! 의존 방향은 `ui` → `app` → `jobs` → `cleanup` → `git` 순서이며, 역방향 의존은 두지 않는다.
//! `storage`와 `platform`은 `app`에서만 사용한다.

pub mod app;
pub mod cleanup;
pub mod error;
pub mod git;
pub mod jobs;
pub mod platform;
pub mod storage;
pub mod ui;
