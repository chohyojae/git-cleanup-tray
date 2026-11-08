// SPDX-License-Identifier: GPL-3.0-only

//! background Git 작업 관리 (PLAN 19장).
//!
//! Slint UI event loop를 Git 작업으로 block하지 않는다. 작업은 worker thread에서 실행하고
//! 완료 결과만 `slint::invoke_from_event_loop`로 UI thread에 전달한다. 이 module은 Slint에
//! 의존하지 않으므로, UI thread로 넘기는 일은 호출자(`app`)가 `deliver` callback에서 한다.
//!
//! - 같은 저장소에 두 작업을 동시에 실행하지 않는다.
//! - 개별 Scan과 Scan All은 저장소별 queue에서 제한된 수만큼 동시에 실행한다.
//! - Clean All은 저장소를 순차 처리한다.
//! - idle 상태에서는 worker thread를 남겨 두지 않는다 (요청 시 생성, 끝나면 종료).

pub mod batch;
pub mod cancellation;
pub mod queue;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::thread::JoinHandle;

use cancellation::CancellationToken;

/// 실행 중인 background 작업 하나.
#[derive(Debug)]
pub struct Job {
    /// worker thread handle. 종료 시 [`Job::join`]으로 끝까지 기다린다.
    handle: JoinHandle<()>,
    /// 작업에 넘긴 취소 flag.
    cancel: CancellationToken,
}

impl Job {
    /// 작업 취소를 요청한다. 진행 중인 Git 명령은 끝까지 기다리고 다음 저장소로 넘어가기 전에 멈춘다.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// worker thread가 끝날 때까지 기다린다.
    ///
    /// 앱 종료 시 삭제 도중에 process가 끝나 Git lock 파일이 남지 않도록 호출한다. worker의 panic은
    /// [`spawn`]에서 이미 잡으므로 여기서는 로그만 남긴다.
    pub fn join(self) {
        if self.handle.join().is_err() {
            log::error!("worker thread terminated abnormally");
        }
    }
}

/// `work`를 새 worker thread에서 실행하고, 끝나면 그 thread에서 `deliver`를 호출한다.
///
/// `work`가 panic하면 `deliver(None)`을 호출한다. 결과가 오지 않아 UI가 계속 "작업 중" 상태로
/// 남는 일을 막기 위해서다. thread는 작업 하나가 끝나면 종료되고 상주하지 않는다.
///
/// # Errors
///
/// OS가 thread를 만들지 못하면 [`std::io::Error`].
pub fn spawn<T, W, D>(name: &str, work: W, deliver: D) -> std::io::Result<Job>
where
    T: Send + 'static,
    W: FnOnce(&CancellationToken) -> T + Send + 'static,
    D: FnOnce(Option<T>) + Send + 'static,
{
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let handle = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            // `work`가 panic한 뒤에는 그 결과를 쓰지 않으므로 unwind safety를 따질 상태가 남지 않는다.
            let output = catch_unwind(AssertUnwindSafe(|| work(&token)));
            if output.is_err() {
                log::error!("worker panicked");
            }
            deliver(output.ok());
        })?;
    Ok(Job { handle, cancel })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// 정상 결과와 panic이 모두 `deliver`로 전달되는지 확인한다.
    #[test]
    fn spawn_delivers_result_and_panic() {
        let (sender, receiver) = mpsc::channel();
        let ok_sender = sender.clone();
        spawn(
            "test-ok",
            |_| 42,
            move |value| ok_sender.send(value).expect("send"),
        )
        .expect("spawn")
        .join();
        assert_eq!(receiver.recv().expect("recv"), Some(42));

        spawn(
            "test-panic",
            |_| -> i32 { panic!("boom") },
            move |value| sender.send(value).expect("send"),
        )
        .expect("spawn")
        .join();
        assert_eq!(receiver.recv().expect("recv"), None);
    }
}
