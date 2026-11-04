// SPDX-License-Identifier: GPL-3.0-only

//! 작업 중에는 남은 시간을 보관하고 timer를 정지하는 주기 실행 시간 계산.

use std::time::{Duration, Instant};

/// Slint와 분리해 실제 대기 없이 pause/resume을 검증하는 상태.
#[derive(Debug, Default)]
pub(super) struct Schedule {
    /// 사용자가 지정한 전체 간격. 꺼져 있으면 `None`.
    interval: Option<Duration>,
    /// 마지막 정지 시점에 남은 시간.
    remaining: Duration,
    /// 현재 timer가 시작된 시각. 정지 중이면 `None`.
    started: Option<Instant>,
}

impl Schedule {
    /// 설정 변경 시 남은 시간을 새 간격으로 바꾼다. timer 시작은 호출자가 맡는다.
    pub(super) fn configure(&mut self, interval: Option<Duration>) {
        self.interval = interval;
        self.remaining = interval.unwrap_or_default();
        self.started = None;
    }

    /// 작업 시작 시 지금까지 흐른 시간만 차감한다. 여러 번 정지해도 중복 차감하지 않는다.
    pub(super) fn pause(&mut self, now: Instant) {
        if let Some(started) = self.started.take() {
            self.remaining = self
                .remaining
                .saturating_sub(now.saturating_duration_since(started));
        }
    }

    /// 정지한 timer를 재개할 때 예약할 시간을 돌려준다. 이미 실행 중이거나 꺼져 있으면 `None`.
    pub(super) fn resume(&mut self, now: Instant) -> Option<Duration> {
        if self.interval.is_none() || self.started.is_some() {
            return None;
        }
        self.started = Some(now);
        Some(self.remaining)
    }

    /// 주기 실행이 시작되면 다음 간격을 준비한다. 이번 작업이 끝날 때까지 정지 상태를 유지한다.
    pub(super) fn fired(&mut self) {
        self.remaining = self.interval.unwrap_or_default();
        self.started = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 여러 작업과 확인 대기를 지나도 남은 시간부터 이어가고, 실행 뒤에는 전체 간격을 쓴다.
    #[test]
    fn preserves_remaining_time_while_busy() {
        let now = Instant::now();
        let mut schedule = Schedule::default();
        assert_eq!(schedule.resume(now), None);
        schedule.configure(Some(Duration::from_secs(60)));
        assert_eq!(schedule.resume(now), Some(Duration::from_secs(60)));
        assert_eq!(schedule.resume(now), None);
        schedule.pause(now + Duration::from_secs(17));
        schedule.pause(now + Duration::from_secs(100));
        assert_eq!(
            schedule.resume(now + Duration::from_secs(200)),
            Some(Duration::from_secs(43))
        );
        schedule.pause(now + Duration::from_secs(210));
        assert_eq!(
            schedule.resume(now + Duration::from_secs(300)),
            Some(Duration::from_secs(33))
        );
        schedule.fired();
        assert_eq!(
            schedule.resume(now + Duration::from_secs(500)),
            Some(Duration::from_secs(60))
        );
        schedule.configure(None);
        assert_eq!(schedule.resume(now), None);
    }
}
