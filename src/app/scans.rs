// SPDX-License-Identifier: GPL-3.0-only

//! 개별 Scan과 Scan All 요청을 같은 저장소별 queue로 실행하고 완료된 줄부터 갱신한다.

use super::{App, ScanPurpose, deliver_to_ui, tasks};
use crate::error::{CleanupError, Result};
use crate::git::repository::RepositoryId;
use crate::jobs::{self, batch::PlanTotals};

use super::state::{SummaryOutcome, TrayStatus};
use super::tasks::{ScanReport, ScanScope};

impl App {
    /// 다른 저장소의 Scan을 기다리지 않고 대상들을 공유 queue에 넣는다.
    pub(super) fn enqueue_scans(&self, scope: ScanScope) {
        if self.job.borrow().is_some() || self.pending.borrow().is_some() {
            return;
        }
        let mut queue = self.scans.borrow_mut();
        if !queue.has_work() {
            *self.scan_totals.borrow_mut() = PlanTotals::default();
            self.scan_completed.set(0);
            self.scan_cancelled.set(false);
        }
        for entry in self.repositories.borrow().iter() {
            if match scope {
                ScanScope::Enabled => entry.config.enabled,
                ScanScope::Only(id) => entry.config.id == id,
            } {
                queue.enqueue(entry.config.id);
            }
        }
        drop(queue);
        if self.scans.borrow().has_work() {
            self.pause_maintenance();
            self.set_notice("");
            self.start_ready_scans();
        } else {
            self.set_last_result("No repositories to scan");
        }
    }

    /// 빈 실행 자리를 채운다. 결과 전달은 UI event loop로 보내므로 화면에서 Git을 기다리지 않는다.
    fn start_ready_scans(&self) {
        let failures = self.scans.borrow_mut().start_ready(|id| {
            jobs::spawn(
                "scan-repository",
                move |cancel| tasks::scan(ScanScope::Only(id), false, cancel, &mut |_| {}),
                move |report| deliver_to_ui(move |app| app.finish_queued_scan(id, report)),
            )
        });
        for (id, error) in failures {
            self.record_queued_scan(id, Some(Err(CleanupError::Io(error))));
        }
        self.refresh_scan_progress();
        // worker가 하나도 만들어지지 않은 경우에도 timer와 등록 대기를 복구한다.
        self.schedule_registration();
        self.resume_maintenance();
    }

    /// 완료한 저장소의 worker를 회수하고 결과를 반영한 뒤 다음 검사를 시작한다.
    fn finish_queued_scan(&self, id: RepositoryId, report: Option<Result<ScanReport>>) {
        self.scans.borrow_mut().finish(id);
        self.record_queued_scan(id, report);
        self.start_ready_scans();
    }

    /// 검사 결과를 합계에 더한다. 다른 저장소의 요약과 실행 상태는 유지한다.
    fn record_queued_scan(&self, id: RepositoryId, report: Option<Result<ScanReport>>) {
        if let Some(Ok(report)) = report.as_ref() {
            let mut totals = self.scan_totals.borrow_mut();
            totals.repositories += report.totals.repositories;
            totals.worktrees += report.totals.worktrees;
            totals.branches += report.totals.branches;
            totals.custom_refs += report.totals.custom_refs;
            totals.skipped += report.totals.skipped;
            totals.errors += report.totals.errors;
            self.scan_completed
                .set(self.scan_completed.get() + report.scanned);
        } else {
            self.scan_totals.borrow_mut().errors += 1;
            self.scan_completed.set(self.scan_completed.get() + 1);
            let message = match report.as_ref() {
                Some(Err(error)) => error.to_string(),
                _ => String::from("Scan stopped by an internal error"),
            };
            self.update_summary(id, |summary| {
                summary.outcome = SummaryOutcome::Failed(message);
            });
        }
        self.finish_scan(ScanPurpose::Scan(id), report);
    }

    /// 저장소별 실행·대기 상태와 전체 완료 수를 화면과 tray에 반영한다.
    pub(super) fn refresh_scan_progress(&self) {
        let queue = self.scans.borrow();
        let active = queue.running_count();
        let waiting = queue.pending_count();
        let busy = queue.has_work();
        let cancelling = queue.is_cancelling();
        drop(queue);
        let completed = self.scan_completed.get();
        if busy {
            self.set_status(TrayStatus::Scanning);
            self.set_progress(if cancelling {
                format!("Cancelling: waiting for {active} running scans…")
            } else {
                format!("Scanning {active} repositories · {waiting} queued · {completed} completed")
            });
        } else {
            self.progress.borrow_mut().clear();
            let totals = self.scan_totals.borrow();
            let mut summary = super::text::scan_summary(completed, &totals);
            if self.scan_cancelled.get() {
                summary.push_str(" (cancelled)");
            }
            self.set_last_result(&summary);
            self.set_status(if totals.errors > 0 {
                TrayStatus::Error
            } else {
                TrayStatus::Idle
            });
        }
        self.refresh_rows();
    }
}
