// SPDX-License-Identifier: GPL-3.0-only

//! tray와 GUI가 공유하는 사용자 요청 목록.
//!
//! tray 메뉴와 메인 창 버튼이 같은 command를 보내므로 처리 로직이 한 곳에만 있다.

use crate::git::repository::RepositoryId;

/// 사용자 요청.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCommand {
    /// 메인 창 열기.
    OpenMainWindow,
    /// tray icon 좌클릭 (Windows). 설정에 따라 메인 창을 연다.
    TrayClicked,
    /// enabled 저장소 전체를 공유 상한 안에서 동시 검사.
    ScanAll,
    /// 전체 재검사 → 요약 → 사용자 확인 → 순차 실행.
    CleanAll,
    /// enabled 저장소 전체 fetch와 로컬 브랜치 최신화.
    UpdateAll,
    /// 저장소 하나 fetch와 로컬 브랜치 최신화.
    UpdateRepository(RepositoryId),
    /// 진행 중인 Scan / Clean 취소. 진행 중인 저장소까지 끝내고 멈춘다.
    CancelJob,
    /// 폴더를 골라 저장소로 등록.
    AddRepository,
    /// 저장소 하나 검사.
    ScanRepository(RepositoryId),
    /// 저장소 하나 재검사 → 확인 → 실행.
    CleanRepository(RepositoryId),
    /// 저장소 하나를 재검사해 상세 화면 표시.
    ShowDetails(RepositoryId),
    /// 저장소 설정 편집 창 열기.
    EditRepository(RepositoryId),
    /// Scan All / Clean All 대상 여부 전환.
    ToggleRepository(RepositoryId),
    /// 등록 해제. 저장소 자체는 건드리지 않는다.
    RemoveRepository(RepositoryId),
    /// 저장소 폴더를 Finder/Explorer로 열기.
    OpenFolder(RepositoryId),
    /// 저장소 폴더에서 terminal 열기.
    OpenTerminal(RepositoryId),
    /// 설정 창 열기.
    OpenSettings,
    /// About 정보 표시 (GPL source 안내 포함).
    ShowAbout,
    /// 앱 종료. 창을 닫는 것만으로는 종료하지 않는다.
    Quit,
}
