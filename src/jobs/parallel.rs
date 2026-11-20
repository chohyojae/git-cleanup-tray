// SPDX-License-Identifier: GPL-3.0-only

//! 저장소 단위 작업을 제한된 수만큼 동시에 실행하는 공통 본체.
//!
//! Scan All, Clean All, Update All이 같은 방식으로 저장소를 나눠 처리한다. thread는 호출 한 번 동안만
//! 살아 있고 끝나면 모두 회수하므로 상주 worker나 thread pool을 두지 않는다 (AGENTS 4.1).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::thread;

use super::cancellation::CancellationToken;

/// `items`를 최대 `limit`개씩 동시에 `work`로 처리하고, 시작한 항목의 결과를 입력 순서대로 돌려준다.
///
/// - 항목은 입력 순서대로 시작하며, 시작 직전에 호출한 thread에서 `on_start(index)`를 부른다.
///   진행 상황 callback이 `Send`가 아니어도 되도록 worker thread에서는 부르지 않는다.
/// - 취소되면 새 항목을 시작하지 않고 이미 시작한 항목이 끝나기를 기다린다. 실행 중인 Git
///   process를 죽이지 않기 위해서다. 따라서 결과는 항상 입력의 앞부분에 해당한다.
/// - 한 항목의 panic이나 thread 생성 실패가 다른 항목의 결과를 잃게 하지 않도록, 그 항목의 결과를
///   `on_failure`로 대신 만든다.
/// - 항목이 하나이거나 `limit`이 1이면 thread를 만들지 않고 호출자가 직접 처리한다. `limit` 0은 1로 본다.
pub fn map_bounded<T, O>(
    items: &[T],
    limit: usize,
    cancel: &CancellationToken,
    thread_name: &str,
    on_start: &mut dyn FnMut(usize),
    work: &(dyn Fn(&T) -> O + Sync),
    on_failure: &(dyn Fn(&T, std::io::Error) -> O + Sync),
) -> Vec<O>
where
    T: Sync,
    O: Send,
{
    let limit = limit.max(1);
    if items.len() <= 1 || limit == 1 {
        return items
            .iter()
            .enumerate()
            .take_while(|_| !cancel.is_cancelled())
            .map(|(index, item)| {
                on_start(index);
                work(item)
            })
            .collect();
    }
    let mut outputs: Vec<Option<O>> = std::iter::repeat_with(|| None).take(items.len()).collect();
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| {
        let mut next = 0;
        let mut running = 0;
        loop {
            while running < limit && next < items.len() && !cancel.is_cancelled() {
                let index = next;
                next += 1;
                let item = &items[index];
                on_start(index);
                let sender = sender.clone();
                let spawned = thread::Builder::new()
                    .name(thread_name.to_owned())
                    .spawn_scoped(scope, move || {
                        // `work`가 panic한 뒤에는 그 결과를 쓰지 않으므로 unwind safety를 따질 상태가 남지 않는다.
                        let output =
                            catch_unwind(AssertUnwindSafe(|| work(item))).unwrap_or_else(|_| {
                                on_failure(item, std::io::Error::other("worker panicked"))
                            });
                        // 받는 쪽은 scope가 끝날 때까지 살아 있어 실패하지 않는다.
                        let _ = sender.send((index, output));
                    });
                match spawned {
                    Ok(_) => running += 1,
                    Err(error) => outputs[index] = Some(on_failure(item, error)),
                }
            }
            if running == 0 {
                break;
            }
            if let Ok((index, output)) = receiver.recv() {
                outputs[index] = Some(output);
                running -= 1;
            }
        }
    });
    outputs.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    /// 완료 순서와 관계없이 결과가 입력 순서를 따르고, 시작 알림도 입력 순서로 온다.
    #[test]
    fn keeps_input_order() {
        let items: Vec<u64> = (0..8).collect();
        let mut started = Vec::new();
        let outputs = map_bounded(
            &items,
            4,
            &CancellationToken::new(),
            "test",
            &mut |index| started.push(index),
            // 앞 항목일수록 늦게 끝나게 해 완료 순서를 뒤집는다.
            &|item| {
                thread::sleep(Duration::from_millis((8 - item) * 3));
                item * 10
            },
            &|_, _| u64::MAX,
        );
        assert_eq!(outputs, [0, 10, 20, 30, 40, 50, 60, 70]);
        assert_eq!(started, [0, 1, 2, 3, 4, 5, 6, 7]);
    }

    /// 동시에 실행 중인 항목 수가 `limit`을 넘지 않으면서 실제로 겹쳐 실행된다.
    #[test]
    fn overlaps_work_up_to_the_limit() {
        let items = [(); 6];
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        // 두 항목이 함께 도착해야 진행한다. 순차 구현이면 제한 시간에 걸린다.
        let gate = (Mutex::new(0_usize), Condvar::new());
        let outputs = map_bounded(
            &items,
            2,
            &CancellationToken::new(),
            "test",
            &mut |_| {},
            &|()| {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                let (count, ready) = &gate;
                let mut arrived = count.lock().expect("lock gate");
                *arrived += 1;
                ready.notify_all();
                let (guard, timeout) = ready
                    .wait_timeout_while(arrived, Duration::from_secs(5), |arrived| *arrived < 2)
                    .expect("wait for overlap");
                drop(guard);
                running.fetch_sub(1, Ordering::SeqCst);
                !timeout.timed_out()
            },
            &|(), _| false,
        );
        assert_eq!(outputs, [true; 6]);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    /// 취소 뒤에는 새 항목을 시작하지 않고, 이미 시작한 항목의 결과는 돌려준다.
    #[test]
    fn stops_starting_after_cancellation() {
        let items: Vec<usize> = (0..6).collect();
        let cancel = CancellationToken::new();
        let mut started = 0;
        let outputs = map_bounded(
            &items,
            2,
            &cancel,
            "test",
            &mut |_| {
                started += 1;
                if started == 2 {
                    cancel.cancel();
                }
            },
            &|item| *item,
            &|_, _| usize::MAX,
        );
        assert_eq!(outputs, [0, 1]);

        let already = CancellationToken::new();
        already.cancel();
        let none = map_bounded(
            &items,
            1,
            &already,
            "test",
            &mut |_| {},
            &|item| *item,
            &|_, _| usize::MAX,
        );
        assert!(none.is_empty(), "{none:?}");
    }

    /// 한 항목이 panic해도 나머지 결과는 그대로 오고, 그 항목은 `on_failure` 결과로 채워진다.
    #[test]
    fn replaces_a_panicked_item_with_the_failure_output() {
        let items = [1, 2, 3];
        let outputs = map_bounded(
            &items,
            3,
            &CancellationToken::new(),
            "test",
            &mut |_| {},
            &|item| {
                assert_ne!(*item, 2, "boom");
                item * 2
            },
            &|item, _| -item,
        );
        assert_eq!(outputs, [2, -2, 6]);
    }
}
