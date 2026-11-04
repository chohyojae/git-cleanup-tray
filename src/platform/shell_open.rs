// SPDX-License-Identifier: GPL-3.0-only

//! 저장소를 Explorer/Finder 또는 terminal로 열기 (PLAN 22장, Phase 6).
//!
//! 경로는 항상 별도 argument로 넘기고 shell command string을 만들지 않는다. 띄운 process는
//! 기다리지 않지만, Unix에서 zombie process가 tray 수명 동안 남지 않도록 짧은 thread에서 회수한다.

use std::io;
use std::path::Path;
use std::process::{Child, Command};

/// `path` 폴더를 Finder(macOS) 또는 File Explorer(Windows)로 연다.
///
/// # Errors
///
/// 폴더가 없거나 열기 program을 실행하지 못하면 [`io::Error`].
pub fn open_folder(path: &Path) -> io::Result<()> {
    if !path.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("folder does not exist: {}", path.display()),
        ));
    }
    reap(imp::open_folder(path)?);
    Ok(())
}

/// `path`를 작업 directory로 하는 terminal을 연다.
///
/// macOS는 Terminal.app, Windows는 Windows Terminal(`wt.exe`)이 있으면 그것을, 없으면 `cmd.exe`를 연다.
///
/// # Errors
///
/// 폴더가 없거나 terminal을 실행하지 못하면 [`io::Error`].
pub fn open_terminal(path: &Path) -> io::Result<()> {
    if !path.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("folder does not exist: {}", path.display()),
        ));
    }
    reap(imp::open_terminal(path)?);
    Ok(())
}

/// 띄운 process가 끝나면 회수한다. `open`/`explorer`는 금방 끝나므로 thread도 곧 종료된다.
fn reap(mut child: Child) {
    let spawned = std::thread::Builder::new()
        .name(String::from("reap-child"))
        .spawn(move || {
            if let Err(error) = child.wait() {
                log::debug!("failed to wait for child process: {error}");
            }
        });
    if let Err(error) = spawned {
        log::debug!("failed to start reaper thread: {error}");
    }
}

#[cfg(target_os = "macos")]
mod imp {
    //! macOS: `open` 명령.

    use super::{Child, Command, Path, io};

    /// `open <path>`로 Finder에서 연다.
    pub(super) fn open_folder(path: &Path) -> io::Result<Child> {
        Command::new("open").arg("--").arg(path).spawn()
    }

    /// `open -a Terminal <path>`로 그 폴더에서 Terminal을 연다.
    pub(super) fn open_terminal(path: &Path) -> io::Result<Child> {
        Command::new("open")
            .args(["-a", "Terminal", "--"])
            .arg(path)
            .spawn()
    }
}

#[cfg(windows)]
mod imp {
    //! Windows: `explorer.exe`, `wt.exe`, `cmd.exe`.

    use std::os::windows::process::CommandExt;

    use super::{Child, Command, Path, io};

    /// Win32 `CREATE_NEW_CONSOLE`. GUI app에서 띄운 `cmd.exe`가 자기 console 창을 갖게 한다.
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

    /// `explorer.exe <path>`로 연다.
    pub(super) fn open_folder(path: &Path) -> io::Result<Child> {
        Command::new("explorer.exe").arg(path).spawn()
    }

    /// Windows Terminal을 먼저 시도하고, 설치되어 있지 않으면 `cmd.exe`를 연다.
    pub(super) fn open_terminal(path: &Path) -> io::Result<Child> {
        match Command::new("wt.exe").arg("-d").arg(path).spawn() {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Command::new("cmd.exe")
                .arg("/K")
                .current_dir(path)
                .creation_flags(CREATE_NEW_CONSOLE)
                .spawn(),
            other => other,
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    //! 지원하지 않는 OS.

    use super::{Child, Path, io};

    /// 항상 [`io::ErrorKind::Unsupported`].
    pub(super) fn open_folder(_path: &Path) -> io::Result<Child> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// 항상 [`io::ErrorKind::Unsupported`].
    pub(super) fn open_terminal(_path: &Path) -> io::Result<Child> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}
