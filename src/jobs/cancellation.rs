// SPDX-License-Identifier: GPL-3.0-only

//! 작업 취소 신호.
//!
//! 이미 시작한 Git process를 죽이지 않고, 다음 대상으로 넘어가기 전에 확인해 멈춘다.
//! 삭제 도중 process를 강제 종료하면 Git lock 파일이 남을 수 있기 때문이다.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// 여러 thread에서 공유하는 취소 flag. clone 비용은 `Arc` 참조 증가 하나다.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    /// 취소되지 않은 새 token을 만든다.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 취소를 요청한다. 이미 취소된 상태여도 문제없다.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// 취소가 요청되었으면 `true`.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
