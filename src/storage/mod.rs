// SPDX-License-Identifier: GPL-3.0-only

//! 설정·저장소 목록·실행 이력 저장 (PLAN 20장).
//!
//! OS별 app data directory에 JSON으로 저장하며, 등록한 Git 저장소 안에는 어떤 파일도 만들지 않는다.
//! 향후 SQLite로 바꿀 수 있도록 store별 interface를 분리해 둔다.
//!
//! 파일은 작업을 시작할 때마다 worker thread에서 새로 읽고 메모리에 계속 들고 있지 않는다
//! (AGENTS 4.1 idle 메모리).

pub mod history_store;
pub mod repository_store;
pub mod settings_store;

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{CleanupError, Result};

/// app data directory 아래에 쓰는 앱 전용 폴더 이름.
pub const APP_DIR_NAME: &str = "GitCleanupTray";

/// 설정 파일을 저장할 앱 전용 directory 경로를 돌려준다.
///
/// - Windows: `%APPDATA%\GitCleanupTray`
/// - macOS: `~/Library/Application Support/GitCleanupTray`
///
/// OS가 경로를 알려주지 않으면 `None`. directory를 만들지는 않는다.
#[must_use]
pub fn app_data_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|base| base.join(APP_DIR_NAME))
}

/// `path`의 JSON 파일을 읽어 `T`로 해석한다. 파일이 없으면 `T::default()`를 돌려준다.
///
/// 첫 실행에는 파일이 없는 것이 정상이므로 오류로 보지 않는다. 반면 파일이 있는데 해석에
/// 실패하면 기본값으로 대체하지 않고 오류를 돌려준다. 손상된 설정을 조용히 기본값으로 바꾸면
/// 보호 branch pattern 같은 사용자 설정이 빠진 채 정리가 실행될 수 있기 때문이다.
///
/// # Errors
///
/// 파일을 읽지 못하면 [`CleanupError::Io`], JSON 해석에 실패하면
/// [`CleanupError::InvalidConfigFile`].
pub fn load_json<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(T::default()),
        Err(error) => return Err(CleanupError::Io(error)),
    };
    serde_json::from_slice(&bytes).map_err(|source| CleanupError::InvalidConfigFile {
        path: path.to_path_buf(),
        source,
    })
}

/// `value`를 JSON으로 `path`에 저장한다. 상위 directory가 없으면 만든다.
///
/// 같은 directory의 임시 파일에 먼저 쓴 뒤 rename한다. 쓰는 도중 앱이 끝나거나 disk가 가득 차도
/// 기존 파일이 반쯤 쓰인 상태로 남지 않는다 (rename은 같은 volume 안에서 원자적이다).
///
/// # Errors
///
/// directory 생성, 파일 쓰기, rename에 실패하면 [`CleanupError::Io`].
pub fn save_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 파일이 없으면 기본값, 깨진 JSON이면 [`CleanupError::InvalidConfigFile`]인지 확인한다.
    #[test]
    fn load_json_handles_missing_and_corrupt_files() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("한글 설정.json");

        let missing: Vec<u32> = load_json(&path).expect("missing file is default");
        assert!(missing.is_empty(), "{missing:?}");

        std::fs::write(&path, "[1, 2,").expect("write");
        let error = load_json::<Vec<u32>>(&path).expect_err("corrupt file");
        assert!(
            matches!(&error, CleanupError::InvalidConfigFile { path: p, .. } if *p == path),
            "{error:?}"
        );

        std::fs::write(&path, "[1, 2]").expect("write");
        assert_eq!(load_json::<Vec<u32>>(&path).expect("valid"), [1, 2]);
    }

    /// 없는 directory도 만들어 저장하고, 다시 읽으면 같은 값인지 확인한다.
    #[test]
    fn save_json_round_trips_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("새 폴더").join("값.json");
        save_json(&path, &[3_u32, 4]).expect("save");
        assert_eq!(load_json::<Vec<u32>>(&path).expect("load"), [3, 4]);
        let names: Vec<_> = std::fs::read_dir(path.parent().expect("parent"))
            .expect("read dir")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names, ["값.json"]);
    }
}
