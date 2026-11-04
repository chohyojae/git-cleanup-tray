// SPDX-License-Identifier: GPL-3.0-only

//! 저장소 단위 작업 queue.
//!
//! 실행 중인 저장소 집합을 관리해 같은 저장소의 중복 실행(버튼 double-click 등)을 막는다.

/// queue에 넣는 작업 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    /// 정리 계획만 만든다 (mutation 없음).
    Scan,
    /// 재검사 후 정리 계획을 실행한다.
    Cleanup,
}
