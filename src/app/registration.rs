// SPDX-License-Identifier: GPL-3.0-only

//! 폴더 다중 선택과 외부 드롭이 공유하는 일괄 등록과 대기 목록.
//!
//! 검증과 저장은 worker에서만 수행한다. 잘못된 폴더가 섞여 있어도 나머지를 등록하고,
//! 같은 묶음 안에서 먼저 추가한 저장소도 common Git directory 중복 판별에 포함한다.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::tasks::{ChangeError, ProgressSink, new_repository};
use crate::error::Result;
use crate::storage::repository_store::{self, RepositoryConfig};
use crate::storage::settings_store;

/// 오류 상세 상한. 다량의 입력에서도 UI에 큰 오류 목록을 계속 보관하지 않는다.
const MAX_PROBLEMS: usize = 50;

/// 작업 중 들어온 경로를 보관해 드롭 요청이 누락되지 않게 한다.
#[derive(Debug, Default)]
pub(super) struct RegistrationQueue {
    /// 아직 worker에 넘기지 않은 경로.
    paths: Vec<PathBuf>,
}

impl RegistrationQueue {
    /// 경로를 대기 목록에 합친다. 동일 경로의 중복 입력은 한 번만 검사한다.
    pub(super) fn extend(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for path in paths {
            if !self.paths.contains(&path) {
                self.paths.push(path);
            }
        }
    }

    /// 대기 경로가 없는지 확인해 불필요한 timer 예약을 피한다.
    pub(super) fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// 대기 경로를 worker에 넘긴다. 시작하지 못하면 다음 사용자 요청까지 그대로 보관한다.
    pub(super) fn start_ready(
        &mut self,
        busy: bool,
        start: impl FnOnce(Arc<Vec<PathBuf>>) -> bool,
    ) {
        if busy || self.paths.is_empty() {
            return;
        }
        // worker 생성 실패 시 소유권을 되찾되, 정상 경로에서는 전체 경로 목록을 복사하지 않는다.
        let paths = Arc::new(std::mem::take(&mut self.paths));
        if !start(Arc::clone(&paths)) {
            self.paths = Arc::unwrap_or_clone(paths);
        }
    }
}

/// 등록하지 못한 폴더와 이유.
#[derive(Debug)]
pub struct RegistrationProblem {
    /// 선택하거나 놓은 원래 경로.
    pub path: PathBuf,
    /// Git 저장소가 아니거나 접근하지 못한 이유.
    pub message: String,
}

/// 전체 등록 목록과 일괄 추가 결과.
#[derive(Debug)]
pub struct RegistrationReport {
    /// 저장을 마친 등록 목록.
    pub repositories: Vec<RepositoryConfig>,
    /// 새로 등록한 저장소 수.
    pub added: usize,
    /// 기존 목록이나 이번 입력에서 중복된 저장소 수.
    pub duplicates: usize,
    /// Git 저장소가 아니거나 확인하지 못한 폴더 수.
    pub failed: usize,
    /// 실패한 폴더 상세. 중복은 오류로 보지 않는다.
    pub problems: Vec<RegistrationProblem>,
}

/// 여러 폴더를 확인하고 정상 항목을 한 번에 저장한다. 하위 폴더를 재귀 검색하지 않는다.
///
/// # Errors
///
/// 설정이나 목록을 읽지 못하거나 최종 저장에 실패하면 해당 오류를 반환한다.
/// 개별 폴더 오류는 결과에 담고 다음 폴더를 계속 확인한다.
pub fn add_repositories(
    dir: &Path,
    paths: &[PathBuf],
    progress: ProgressSink<'_>,
) -> Result<RegistrationReport> {
    let mut repositories = repository_store::load(dir)?;
    let settings = settings_store::load(dir)?;
    let mut report = RegistrationReport {
        repositories: Vec::new(),
        added: 0,
        duplicates: 0,
        failed: 0,
        problems: Vec::new(),
    };
    for (index, path) in paths.iter().enumerate() {
        progress(format!(
            "Adding repositories: {} of {}",
            index + 1,
            paths.len()
        ));
        match new_repository(&repositories, &settings, path) {
            Ok(config) => {
                log::info!("registering repository {}", config.id.0);
                repositories.push(config);
                report.added += 1;
            }
            Err(ChangeError::AlreadyRegistered(_)) => report.duplicates += 1,
            Err(error) => {
                report.failed += 1;
                if report.problems.len() < MAX_PROBLEMS {
                    report.problems.push(RegistrationProblem {
                        path: path.clone(),
                        message: error.to_string(),
                    });
                }
            }
        }
    }
    // 등록한 항목이 없으면 기존 저장 파일을 불필요하게 다시 쓰지 않는다.
    if report.added > 0 {
        repository_store::save(dir, &repositories)?;
    }
    report.repositories = repositories;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 작업 중 들어온 여러 드롭을 유지하고 동일 경로는 한 번만 worker에 넘긴다.
    #[test]
    fn queued_drops_survive_busy_worker() {
        let first = PathBuf::from("한글 저장소/첫 번째");
        let second = PathBuf::from("한글 저장소/두 번째");
        let mut queue = RegistrationQueue::default();
        queue.extend([first.clone()]);
        queue.start_ready(true, |_| panic!("busy queue must not start a worker"));
        queue.extend([first.clone(), second.clone()]);
        queue.start_ready(false, |paths| {
            assert_eq!(paths.as_slice(), [first, second]);
            true
        });
        assert!(queue.is_empty());
        queue.start_ready(false, |_| panic!("empty queue must not start a worker"));
    }

    /// worker 생성 실패 후 새 입력을 합쳐 재시도해도 경로가 누락되거나 중복되지 않는다.
    #[test]
    fn failed_worker_start_keeps_paths_for_retry() {
        let first = PathBuf::from("한글 저장소/첫 번째");
        let second = PathBuf::from("한글 저장소/두 번째");
        let mut queue = RegistrationQueue::default();
        queue.extend([first.clone()]);
        queue.start_ready(false, |paths| {
            assert_eq!(paths.as_slice(), std::slice::from_ref(&first));
            false
        });
        assert!(!queue.is_empty());
        queue.extend([first.clone(), second.clone()]);
        queue.start_ready(false, |paths| {
            assert_eq!(paths.as_slice(), [first, second]);
            true
        });
        assert!(queue.is_empty());
    }
}
