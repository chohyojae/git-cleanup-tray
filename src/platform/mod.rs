// SPDX-License-Identifier: GPL-3.0-only

//! Windows/macOS 전용 코드 격리 영역 (PLAN 22장).
//!
//! OS별 `#[cfg]` 분기는 이 module 안에만 둔다. 다른 module은 여기서 제공하는 함수만 호출한다.

pub mod autostart;
pub mod notification;
pub mod shell_open;

use std::path::PathBuf;
use std::process::Command;

/// Git이 출력한 path byte를 [`PathBuf`]로 바꾼다.
///
/// Unix에서는 path가 UTF-8이 아닐 수 있으므로 byte를 그대로 `OsStr`로 감싼다 (복사 1회).
/// Windows의 Git은 path를 UTF-8로 출력하므로 UTF-8로 해석한다. 잘못된 byte는 U+FFFD가 되며,
/// 그런 path는 이후 Git 명령에서 찾지 못해 오류로 드러난다.
#[must_use]
pub fn path_from_git_bytes(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// background에서 실행할 child process에 OS별 옵션을 적용한다.
///
/// Windows GUI subsystem에서 `git`을 실행하면 매번 console 창이 잠깐 떴다 사라지므로
/// `CREATE_NO_WINDOW`를 지정한다. 다른 OS에서는 아무것도 하지 않는다.
pub fn configure_background_command(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        /// Win32 `CREATE_NO_WINDOW` process creation flag.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}
