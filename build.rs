// SPDX-License-Identifier: GPL-3.0-only

//! `.slint` UI 파일을 Rust code로 compile하는 build script.

/// `ui/app.slint`를 entry로 Slint compiler를 실행한다.
///
/// `ui/app.slint`가 tray와 window component를 모두 export하므로 이 파일 하나만 넘기면
/// import된 나머지 `.slint` 파일도 함께 compile되고 변경 감지 대상이 된다.
fn main() {
    if let Err(error) = slint_build::compile("ui/app.slint") {
        panic!("Slint UI compile failed: {error}");
    }
}
