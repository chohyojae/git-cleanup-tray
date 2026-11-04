// SPDX-License-Identifier: GPL-3.0-only

//! Git CLI orchestration layer.
//!
//! Git 실행은 반드시 [`runner::GitRunner`]를 거친다. 다른 module에서 `std::process::Command`로
//! `git`을 직접 실행하지 않는다. merge 판정, ref 저장, worktree metadata는 Git에 맡기고
//! 이 module은 명령 조합과 출력 parsing만 담당한다.

pub mod default_branch;
pub mod guard;
pub mod object;
pub mod refs;
pub mod repository;
pub mod runner;
pub mod worktree;

pub mod sync;
