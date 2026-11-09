// SPDX-License-Identifier: GPL-3.0-only

//! 시간 비교와 같은 측정 로직을 메모리 비교용 진입점으로 제공한다.

/// 데이터 생성·조회·계획 검증을 원래 예제와 공유한다.
#[path = "scan_benchmark.rs"]
mod shared_benchmark;

/// 같은 측정 경로를 사용해 준비된 읽기 전용 fixture의 메모리를 비교한다.
/// 결과 파일을 기록하지 못하면 I/O 오류를 반환한다.
fn main() -> std::io::Result<()> {
    shared_benchmark::main()
}
