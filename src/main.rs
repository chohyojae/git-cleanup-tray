// SPDX-License-Identifier: GPL-3.0-only

//! Git Cleanup Tray 실행 파일 entry point.
//!
//! 실제 로직은 모두 library crate(`git_cleanup_tray`)에 있고, 여기서는 logger 초기화와
//! application 실행만 담당한다.

// Windows release build에서는 console 창을 띄우지 않는다.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::process::ExitCode;

/// logger를 초기화하고 tray application을 실행한다.
///
/// 정상 종료(사용자가 Quit 선택)면 `ExitCode::SUCCESS`, 초기화나 event loop 실패면
/// 오류를 로그로 남기고 `ExitCode::FAILURE`를 반환한다.
fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    match git_cleanup_tray::app::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            log::error!("application terminated with error: {error}");
            ExitCode::FAILURE
        }
    }
}
