// SPDX-License-Identifier: GPL-3.0-only

//! 앱 설정 (`config.json`) 저장과 로드 (PLAN 15장).
//!
//! "dirty worktree skip", "force 삭제 금지", "remote branch 삭제 금지"는 설정 항목이 아니라
//! application invariant이므로 여기에 필드로 두지 않는다.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use std::time::Duration;

/// 저장 파일 이름.
pub const FILE_NAME: &str = "config.json";

/// 사용자 설정.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)] // 각 bool이 독립된 on/off 설정 항목이다.
pub struct AppSettings {
    /// login 시 자동 시작.
    pub start_at_login: bool,
    /// 시작 시 메인 창 없이 tray로만 시작.
    pub start_minimized: bool,
    /// cleanup 완료 시 OS 알림 표시. 알림은 MVP에서 제외해 아직 쓰지 않는다 (설정 화면에도 없다).
    pub notify_on_completion: bool,
    /// tray icon 좌클릭 시 메인 창 열기.
    pub open_on_left_click: bool,
    /// git 실행 파일 경로. `None`이면 PATH에서 찾는다.
    pub git_executable: Option<PathBuf>,
    /// 새 저장소의 기본 remote 이름.
    pub default_remote: String,
    /// local symbolic ref가 없을 때 `git ls-remote`로 remote 질의를 허용할지.
    pub allow_remote_query: bool,
    /// 보호 branch 이름 pattern.
    pub protected_branch_patterns: Vec<String>,
    /// Clean All 전에 확인 dialog를 표시할지.
    pub confirm_clean_all: bool,
    /// 주기 최신화·정리 간격(분). `None`이면 timer와 자동 작업을 만들지 않는다.
    pub maintenance_interval_minutes: Option<MaintenanceInterval>,
}

impl Default for AppSettings {
    /// 첫 실행 시 기본값. tray-first이므로 최소화 시작이 기본이다.
    fn default() -> Self {
        Self {
            start_at_login: false,
            start_minimized: true,
            notify_on_completion: true,
            open_on_left_click: true,
            git_executable: None,
            default_remote: String::from(super::repository_store::DEFAULT_REMOTE),
            allow_remote_query: true,
            protected_branch_patterns: Vec::new(),
            confirm_clean_all: true,
            maintenance_interval_minutes: None,
        }
    }
}

/// `dir`의 `config.json`을 읽는다. 파일이 없으면 [`AppSettings::default`]다.
///
/// # Errors
///
/// 파일을 읽지 못하거나 JSON이 깨졌으면 [`super::load_json`]의 오류. 깨진 설정을 기본값으로
/// 대체하지 않으므로 호출자는 작업을 중단해야 한다.
pub fn load(dir: &Path) -> Result<AppSettings> {
    super::load_json(&dir.join(FILE_NAME))
}

/// `dir`의 `config.json`에 `settings`를 저장한다.
///
/// # Errors
///
/// 저장에 실패하면 [`super::save_json`]의 오류.
pub fn save(dir: &Path, settings: &AppSettings) -> Result<()> {
    super::save_json(&dir.join(FILE_NAME), settings)
}

/// 1분~7일 범위로 검증된 주기 실행 간격. JSON에서는 분 단위 정수로 저장한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct MaintenanceInterval(u32);

impl MaintenanceInterval {
    /// 분 단위 값을 검증한다. 0이나 7일을 넘는 값은 timer 오작동을 막기 위해 거부한다.
    #[must_use]
    pub fn new(minutes: u32) -> Option<Self> {
        (1..=10_080).contains(&minutes).then_some(Self(minutes))
    }

    /// 설정 화면과 timer에 쓸 분 단위 값.
    #[must_use]
    pub fn minutes(self) -> u32 {
        self.0
    }

    /// timer에서 사용할 초 단위 시간으로 바꾼다.
    #[must_use]
    pub fn duration(self) -> Duration {
        Duration::from_secs(u64::from(self.0) * 60)
    }
}

impl TryFrom<u32> for MaintenanceInterval {
    type Error = &'static str;

    /// 손상된 설정 파일의 값으로 빠른 반복 작업이 시작되지 않도록 검증한다.
    fn try_from(value: u32) -> std::result::Result<Self, Self::Error> {
        Self::new(value).ok_or("Maintenance interval must be between 1 and 10080 minutes.")
    }
}

impl From<MaintenanceInterval> for u32 {
    /// JSON에는 기존 설정 항목처럼 단순 정수로 기록한다.
    fn from(value: MaintenanceInterval) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 기존 설정은 자동 작업을 켜지 않고, 새 간격은 범위를 검증해 왕복 저장한다.
    #[test]
    fn interval_is_opt_in_and_validated() {
        let old: AppSettings = serde_json::from_str("{}").expect("old settings");
        assert_eq!(old.maintenance_interval_minutes, None);
        for invalid in [0, 10081, u32::MAX] {
            assert!(
                serde_json::from_str::<AppSettings>(&format!(
                    "{{\"maintenance_interval_minutes\":{invalid}}}"
                ))
                .is_err()
            );
        }
        for minutes in [1, 60, 10080] {
            let settings = AppSettings {
                maintenance_interval_minutes: MaintenanceInterval::new(minutes),
                ..AppSettings::default()
            };
            let json = serde_json::to_string(&settings).expect("serialize");
            assert_eq!(
                serde_json::from_str::<AppSettings>(&json).expect("load"),
                settings
            );
        }
    }
}
