// SPDX-License-Identifier: GPL-3.0-only

//! 격리된 임시 저장소에서 순차 Git 검사와 동시 Git 검사의 경과 시간을 비교한다.

#[path = "../tests/common/mod.rs"]
mod common;

use std::time::Instant;

use common::{Sandbox, default_namespaces, request};
use git_cleanup_tray::cleanup::planner::ScanRequest;
use git_cleanup_tray::git::repository::RepositoryId;
use git_cleanup_tray::git::runner::ProcessGitRunner;
use git_cleanup_tray::jobs::batch::{self, RepositoryScan};
use git_cleanup_tray::jobs::cancellation::CancellationToken;

/// 동일한 임시 저장소 16개를 만들고 두 방식의 실행 순서를 번갈아 측정한다.
fn main() {
    let sandbox = Sandbox::new();
    let repositories: Vec<_> = (0..16)
        .map(|index| sandbox.clone_with_default("trunk", &format!("측정 저장소 {index}")))
        .collect();
    let namespaces = default_namespaces();
    let requests: Vec<_> = repositories
        .iter()
        .zip(1_u64..)
        .map(|(path, id)| ScanRequest {
            repository_id: RepositoryId(id),
            ..request(path, &namespaces, &[])
        })
        .collect();
    let runner = ProcessGitRunner::new();
    let cancel = CancellationToken::new();
    let run = |parallel: bool| {
        let start = Instant::now();
        let scans: Vec<RepositoryScan> = if parallel {
            batch::scan_all(&runner, &requests, &cancel, &mut |_| {})
        } else {
            requests
                .iter()
                .flat_map(|one| {
                    batch::scan_all(&runner, std::slice::from_ref(one), &cancel, &mut |_| {})
                })
                .collect()
        };
        assert_eq!(scans.len(), requests.len());
        assert!(scans.iter().all(|scan| scan.outcome.is_ok()));
        start.elapsed().as_secs_f64() * 1000.0
    };
    for _ in 0..5 {
        run(false);
        run(true);
    }
    let mut sequential = Vec::new();
    let mut parallel = Vec::new();
    for index in 0..30 {
        for simultaneous in [index % 2 == 0, index % 2 != 0] {
            let sample = run(simultaneous);
            if simultaneous {
                parallel.push(sample);
            } else {
                sequential.push(sample);
            }
        }
    }
    sequential.sort_by(f64::total_cmp);
    parallel.sort_by(f64::total_cmp);
    println!(
        "repositories=16 samples=30 warmups=5 parallelism={}",
        git_cleanup_tray::jobs::queue::scan_parallelism()
    );
    println!(
        "sequential p50={:.2}ms p95={:.2}ms",
        sequential[14], sequential[28]
    );
    println!(
        "parallel p50={:.2}ms p95={:.2}ms",
        parallel[14], parallel[28]
    );
}
