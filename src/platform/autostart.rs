// SPDX-License-Identifier: GPL-3.0-only

//! login 시 자동 시작 등록/해제 (PLAN 22장, Phase 6).
//!
//! - macOS: `~/Library/LaunchAgents/<LABEL>.plist` LaunchAgent. `SMAppService`는 서명된 `.app`
//!   bundle이 필요해, bundle 없이도 동작하는 LaunchAgent plist를 쓴다.
//! - Windows: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` registry 값.
//!
//! 등록하는 실행 파일은 현재 실행 중인 binary다. 앱을 다른 위치로 옮기면 설정에서 다시 켜야 한다.

use std::io;

/// LaunchAgent label과 registry 값 이름.
pub const ENTRY_NAME: &str = "GitCleanupTray";

/// 자동 시작을 켜거나 끈다. 이미 그 상태면 아무것도 바꾸지 않은 것과 같다.
///
/// # Errors
///
/// 현재 실행 파일 경로를 모르거나, plist/registry를 쓰지 못하면 [`io::Error`]. 지원하지 않는
/// OS에서는 [`io::ErrorKind::Unsupported`].
pub fn set_enabled(enabled: bool) -> io::Result<()> {
    let executable = std::env::current_exe()?;
    imp::set_enabled(enabled, &executable)
}

#[cfg(target_os = "macos")]
mod imp {
    //! macOS LaunchAgent 구현.

    use std::io;
    use std::path::{Path, PathBuf};

    /// LaunchAgent plist 경로 (`~/Library/LaunchAgents/GitCleanupTray.plist`).
    fn plist_path() -> io::Result<PathBuf> {
        let home = dirs::home_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory is unknown"))?;
        Ok(home
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", super::ENTRY_NAME)))
    }

    /// plist를 쓰거나 지운다. launchd는 다음 login 때 이 파일을 읽으므로 `launchctl`은 부르지 않는다.
    pub(super) fn set_enabled(enabled: bool, executable: &Path) -> io::Result<()> {
        let path = plist_path()?;
        if !enabled {
            return match std::fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        let executable = executable.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "executable path is not valid UTF-8",
            )
        })?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, plist(executable))
    }

    /// LaunchAgent plist 내용. 경로는 XML escape한다 (`&`, `<` 등이 들어간 폴더 이름).
    pub(super) fn plist(executable: &str) -> String {
        let mut escaped = String::with_capacity(executable.len());
        for ch in executable.chars() {
            match ch {
                '&' => escaped.push_str("&amp;"),
                '<' => escaped.push_str("&lt;"),
                '>' => escaped.push_str("&gt;"),
                '"' => escaped.push_str("&quot;"),
                '\'' => escaped.push_str("&apos;"),
                _ => escaped.push(ch),
            }
        }
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{escaped}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
            label = super::ENTRY_NAME,
        )
    }

    #[cfg(test)]
    mod tests {
        /// 실행 파일 경로의 XML 특수문자를 escape하는지 확인한다.
        #[test]
        fn plist_escapes_path() {
            let text = super::plist("/Applications/A & B <앱>/tray");
            assert!(
                text.contains("<string>/Applications/A &amp; B &lt;앱&gt;/tray</string>"),
                "{text}"
            );
        }
    }
}

#[cfg(windows)]
mod imp {
    //! Windows `Run` registry 구현.

    use std::io;
    use std::path::Path;

    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};

    /// 현재 사용자 login 시 실행할 program 목록 key.
    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    /// `Run` key에 값을 쓰거나 지운다. 공백이 든 경로를 위해 값 전체를 따옴표로 감싼다.
    pub(super) fn set_enabled(enabled: bool, executable: &Path) -> io::Result<()> {
        let run =
            RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)?;
        if enabled {
            let mut value = std::ffi::OsString::from("\"");
            value.push(executable.as_os_str());
            value.push("\"");
            run.set_value(super::ENTRY_NAME, &value)
        } else {
            match run.delete_value(super::ENTRY_NAME) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    //! 지원하지 않는 OS.

    use std::io;
    use std::path::Path;

    /// 항상 [`io::ErrorKind::Unsupported`].
    pub(super) fn set_enabled(_enabled: bool, _executable: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}
