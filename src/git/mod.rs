// SPDX-License-Identifier: GPL-3.0-only

//! 읽기 전용 Git 라이브러리와 변경·네트워크용 Git 명령을 연결한다.
//!
//! Git 실행은 반드시 [`runner::GitRunner`]를 거친다. 다른 module에서 `std::process::Command`로
//! `git`을 직접 실행하지 않는다. 라이브러리 조회도 [`read::GitReadRepository`]만 노출하며
//! ref 저장과 worktree 변경에는 기존 guard를 통과한 Git 명령을 사용한다.

pub mod default_branch;
pub mod guard;
pub mod in_process;
pub mod object;
pub mod read;
pub mod refs;
pub mod repository;
pub mod runner;
pub mod worktree;

pub mod sync;
