// SPDX-License-Identifier: GPL-3.0-only

//! tray와 GUI가 함께 쓰는 application state (PLAN 11장 Tray 상태).
//!
//! 저장소마다 화면에 필요한 요약만 들고, Scan 결과(plan) 같은 큰 데이터는 들고 있지 않는다
//! (AGENTS 4.1). 상세 화면은 필요할 때 다시 Scan해서 만든다.

use crate::storage::repository_store::RepositoryConfig;

/// tray icon/tooltip에 반영할 전체 상태.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TrayStatus {
    /// 실행 중인 작업 없음.
    #[default]
    Idle,
    /// 검사 중.
    Scanning,
    /// 정리 실행 중.
    CleaningUp,
    /// fetch 및 로컬 브랜치 최신화 중.
    Updating,
    /// 사용자가 확인해야 할 skipped/warning 항목이 있다.
    AttentionRequired,
    /// 마지막 작업이 오류로 끝났다.
    Error,
}

impl TrayStatus {
    /// tooltip에 붙일 짧은 상태 문구.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Scanning => "Scanning",
            Self::Updating => "Updating",
            Self::CleaningUp => "Cleanup in progress",
            Self::AttentionRequired => "Attention required",
            Self::Error => "Error",
        }
    }

    /// 작업 실행 중이면 `true`.
    #[must_use]
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Scanning | Self::CleaningUp | Self::Updating)
    }
}

/// 등록 저장소 하나와 그 화면 요약.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryEntry {
    /// `repositories.json`의 설정.
    pub config: RepositoryConfig,
    /// 마지막 Scan / cleanup 요약. 앱을 다시 시작하면 cleanup 시각 외에는 비어 있다.
    pub summary: RepositorySummary,
}

/// 저장소 줄에 보여줄 요약.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepositorySummary {
    /// 마지막 Scan에서 판별한 base branch 이름.
    pub base: Option<String>,
    /// 마지막 작업 결과.
    pub outcome: SummaryOutcome,
    /// 마지막 Scan 시각 (UNIX epoch 초).
    pub last_scan: Option<u64>,
    /// 마지막 cleanup 시각 (UNIX epoch 초). `history.json`에서 읽는다.
    pub last_cleanup: Option<u64>,
}

/// 저장소의 마지막 작업 결과.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SummaryOutcome {
    /// 이번 실행에서 아직 Scan하지 않았다.
    #[default]
    NotScanned,
    /// Scan 결과.
    Scanned {
        /// 삭제할 항목 수.
        removable: usize,
        /// 건너뛴 항목과 오류 수 (merge되지 않은 branch 제외).
        warnings: usize,
    },
    /// 저장소 전체를 검사하지 못했다. 사용자에게 보여줄 오류 문구를 담는다.
    Failed(String),
    /// fetch와 로컬 브랜치 최신화 결과.
    Updated {
        /// fast-forward한 브랜치 수.
        updated: usize,
        /// 안전 조건 때문에 건너뛴 브랜치 수.
        skipped: usize,
        /// 브랜치별 오류 수.
        errors: usize,
    },
    /// cleanup 결과. 다시 Scan하기 전까지의 상태다.
    Cleaned {
        /// 삭제한 항목 수.
        removed: usize,
        /// 건너뛴 항목 수.
        skipped: usize,
        /// 오류 수.
        errors: usize,
    },
}
