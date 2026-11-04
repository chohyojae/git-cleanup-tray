// SPDX-License-Identifier: GPL-3.0-only

//! application state ↔ Slint component 연결.
//!
//! 창별 property 채우기와 callback 연결은 창 lifetime을 관리하는 `app::windows`에 두었다.
//! 상태를 Slint model로 바꾸는 순수 변환은 [`super::models`]에 있다.
