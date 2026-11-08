// SPDX-License-Identifier: GPL-3.0-only

//! 저장소 단위 작업 queue.
//!
//! 실행 중인 저장소 집합을 관리해 같은 저장소의 중복 실행(버튼 double-click 등)을 막는다.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::git::repository::RepositoryId;

use super::Job;

/// Git의 파일 I/O를 겹치되 저장소가 많아도 process를 무제한 생성하지 않을 기본 동시 검사 수.
#[must_use]
pub fn scan_parallelism() -> usize {
    std::thread::available_parallelism().map_or(2, |count| count.get().clamp(2, 4))
}

/// 개별 Scan과 Scan All이 공유하는 대기 목록과 실행 중인 worker.
///
/// UI thread에서 요청·완료 이벤트가 올 때만 진행한다. idle polling이나 상주 worker는 없다.
pub struct ScanQueue {
    /// 동시에 실행할 저장소 수. 생성 시 최소 1로 제한한다.
    limit: usize,
    /// 요청 순서대로 실행할 저장소.
    pending: VecDeque<RepositoryId>,
    /// 대기·실행 중인 저장소를 합쳐 중복 요청을 막는다.
    requested: HashSet<RepositoryId>,
    /// 결과를 아직 회수하지 않은 worker. 결과가 도착하면 join하고 제거한다.
    active: HashMap<RepositoryId, Job>,
    /// 취소 중에는 이미 실행 중인 저장소가 끝날 때까지 새 요청을 받지 않는다.
    cancelling: bool,
}

impl Default for ScanQueue {
    /// 장치별 기본 동시 검사 수로 빈 queue를 만든다.
    fn default() -> Self {
        Self::new(scan_parallelism())
    }
}

impl ScanQueue {
    /// `limit`개까지 실행하는 빈 queue를 만든다. 0은 1로 처리한다.
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            pending: VecDeque::new(),
            requested: HashSet::new(),
            active: HashMap::new(),
            cancelling: false,
        }
    }

    /// 새 저장소 검사를 예약한다. 같은 저장소가 이미 있거나 취소 중이면 `false`.
    pub fn enqueue(&mut self, id: RepositoryId) -> bool {
        if self.cancelling || !self.requested.insert(id) {
            return false;
        }
        self.pending.push_back(id);
        true
    }

    /// 빈 자리만큼 worker를 시작한다. 생성에 실패한 저장소는 queue에서 빼고 오류를 돌려준다.
    pub fn start_ready(
        &mut self,
        mut start: impl FnMut(RepositoryId) -> std::io::Result<Job>,
    ) -> Vec<(RepositoryId, std::io::Error)> {
        let mut failures = Vec::new();
        while self.active.len() < self.limit {
            let Some(id) = self.pending.pop_front() else {
                break;
            };
            match start(id) {
                Ok(job) => {
                    self.active.insert(id, job);
                }
                Err(error) => {
                    self.requested.remove(&id);
                    failures.push((id, error));
                }
            }
        }
        failures
    }

    /// 완료된 worker를 회수한다. 알 수 없는 완료 이벤트는 무시한다.
    pub fn finish(&mut self, id: RepositoryId) {
        if let Some(job) = self.active.remove(&id) {
            job.join();
            self.requested.remove(&id);
        }
        if self.active.is_empty() {
            self.cancelling = false;
        }
    }

    /// 대기 중인 검사는 버리고 실행 중인 검사에는 취소를 요청한다. Git process를 강제 종료하지 않는다.
    pub fn cancel(&mut self) {
        for id in self.pending.drain(..) {
            self.requested.remove(&id);
        }
        for job in self.active.values() {
            job.cancel();
        }
        self.cancelling = !self.active.is_empty();
    }

    /// 앱 종료 때 실행 중인 모든 worker의 종료를 기다린다.
    pub fn join(&mut self) {
        self.cancel();
        for (_, job) in self.active.drain() {
            job.join();
        }
        self.requested.clear();
        self.cancelling = false;
    }

    /// 이 저장소가 대기 또는 실행 중이면 `true`.
    #[must_use]
    pub fn contains(&self, id: RepositoryId) -> bool {
        self.requested.contains(&id)
    }

    /// 이 저장소의 worker가 실행 중이면 `true`.
    #[must_use]
    pub fn is_running(&self, id: RepositoryId) -> bool {
        self.active.contains_key(&id)
    }

    /// 대기 또는 실행 중인 검사가 있으면 `true`.
    #[must_use]
    pub fn has_work(&self) -> bool {
        !self.requested.is_empty()
    }

    /// 취소를 요청한 worker를 기다리는 중이면 `true`.
    #[must_use]
    pub fn is_cancelling(&self) -> bool {
        self.cancelling
    }

    /// 현재 실행 중인 저장소 수.
    #[must_use]
    pub fn running_count(&self) -> usize {
        self.active.len()
    }

    /// 아직 시작하지 않은 저장소 수.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

/// queue에 넣는 작업 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    /// 정리 계획만 만든다 (mutation 없음).
    Scan,
    /// 재검사 후 정리 계획을 실행한다.
    Cleanup,
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::Duration;

    use super::*;

    /// worker가 시작·완료되는 순서를 제어해 시간 측정 없이 동시 실행과 상한을 검증한다.
    struct Workers {
        /// 테스트가 허용한 worker만 검사를 마치도록 하는 신호.
        releases: HashMap<RepositoryId, Sender<()>>,
        /// 실제 시작한 worker가 알려 주는 저장소.
        started: Sender<RepositoryId>,
        /// worker의 결과와 취소 상태.
        finished: Sender<(RepositoryId, bool)>,
    }

    impl Workers {
        /// 신호를 기다리는 실제 worker를 시작한다. 테스트가 실패해도 영원히 대기하지 않는다.
        fn start(&mut self, id: RepositoryId) -> std::io::Result<Job> {
            let (release, gate) = mpsc::channel();
            self.releases.insert(id, release);
            let started = self.started.clone();
            let finished = self.finished.clone();
            super::super::spawn(
                "queue-test",
                move |cancel| {
                    started.send(id).expect("report start");
                    gate.recv_timeout(Duration::from_secs(5))
                        .expect("release worker");
                    cancel.is_cancelled()
                },
                move |cancelled| {
                    finished
                        .send((id, cancelled.expect("worker result")))
                        .expect("report completion");
                },
            )
        }

        /// 지정한 worker만 완료시킨다.
        fn release(&self, id: RepositoryId) {
            self.releases[&id].send(()).expect("release worker");
        }
    }

    /// 신호가 오지 않는 회귀가 생겨도 테스트를 무한 대기시키지 않는다.
    fn receive<T>(receiver: &Receiver<T>) -> T {
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("worker signal")
    }

    /// Scan All과 개별 요청이 같은 상한을 공유하고, 먼저 완료한 자리부터 대기 요청을 시작한다.
    #[test]
    fn individual_requests_join_running_batch_without_duplicates() {
        let (started, starts) = mpsc::channel();
        let (finished, results) = mpsc::channel();
        let mut workers = Workers {
            releases: HashMap::new(),
            started,
            finished,
        };
        let mut queue = ScanQueue::new(2);
        let [first, second, third] = [RepositoryId(1), RepositoryId(2), RepositoryId(3)];
        assert!(queue.enqueue(first));
        assert!(queue.start_ready(|id| workers.start(id)).is_empty());
        assert_eq!(receive(&starts), first);
        assert!(queue.enqueue(second));
        assert!(queue.enqueue(third));
        assert!(!queue.enqueue(first));
        assert!(!queue.enqueue(third));
        assert!(queue.start_ready(|id| workers.start(id)).is_empty());
        assert_eq!(receive(&starts), second);
        assert_eq!(queue.running_count(), 2);
        assert_eq!(queue.pending_count(), 1);
        workers.release(second);
        assert_eq!(receive(&results), (second, false));
        queue.finish(second);
        assert!(queue.start_ready(|id| workers.start(id)).is_empty());
        assert_eq!(receive(&starts), third);
        assert!(queue.is_running(first));
        workers.release(third);
        workers.release(first);
        for _ in 0..2 {
            let (id, cancelled) = receive(&results);
            assert!(!cancelled);
            queue.finish(id);
        }
        assert!(!queue.has_work());
    }

    /// 취소하면 대기 항목은 실행하지 않고, 이미 시작한 worker는 회수한 뒤 새 요청을 받는다.
    #[test]
    fn cancellation_discards_pending_and_waits_for_active_workers() {
        let (started, starts) = mpsc::channel();
        let (finished, results) = mpsc::channel();
        let mut workers = Workers {
            releases: HashMap::new(),
            started,
            finished,
        };
        let mut queue = ScanQueue::new(1);
        assert!(queue.enqueue(RepositoryId(1)));
        assert!(queue.enqueue(RepositoryId(2)));
        assert!(queue.start_ready(|id| workers.start(id)).is_empty());
        let active = receive(&starts);
        queue.cancel();
        assert_eq!(queue.pending_count(), 0);
        assert!(!queue.contains(RepositoryId(2)));
        assert!(queue.is_cancelling());
        assert!(!queue.enqueue(RepositoryId(3)));
        workers.release(active);
        assert_eq!(receive(&results), (active, true));
        queue.finish(active);
        assert!(!queue.has_work());
        assert!(!queue.is_cancelling());
        assert!(queue.enqueue(RepositoryId(3)));
        queue.join();
        assert!(!queue.has_work());
    }

    /// thread 생성 실패가 다른 저장소의 실행이나 이후 재시도를 막지 않는다.
    #[test]
    fn failed_start_releases_repository_and_continues() {
        let mut queue = ScanQueue::new(1);
        let (done, results) = mpsc::channel();
        queue.enqueue(RepositoryId(1));
        queue.enqueue(RepositoryId(2));
        let failures = queue.start_ready(|id| {
            if id == RepositoryId(1) {
                return Err(std::io::Error::other("thread creation failed"));
            }
            let done = done.clone();
            super::super::spawn(
                "queue-test",
                |_| (),
                move |_| done.send(id).expect("completion"),
            )
        });
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, RepositoryId(1));
        assert!(!queue.contains(RepositoryId(1)));
        queue.finish(receive(&results));
        assert!(!queue.has_work());
        assert!(queue.enqueue(RepositoryId(1)));
        queue.join();
    }
}
