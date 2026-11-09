// SPDX-License-Identifier: GPL-3.0-only

//! 같은 동시 실행 수와 입력으로 Git 명령과 gix 조회를 비교하고 원본 CSV를 저장한다.

#[path = "../tests/common/mod.rs"]
mod common;

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::{Sandbox, default_namespaces, request};
use git_cleanup_tray::cleanup::planner::ScanRequest;
use git_cleanup_tray::git::in_process::InProcessGitRunner;
use git_cleanup_tray::git::repository::RepositoryId;
use git_cleanup_tray::git::runner::{GitOutput, GitRunner, ProcessGitRunner};
use git_cleanup_tray::jobs::batch::{self, RepositoryScan};
use git_cleanup_tray::jobs::cancellation::CancellationToken;

/// 측정에 사용한 Git process 수를 정확히 세는 가벼운 runner.
#[derive(Clone, Default)]
struct CountedGit {
    /// 원격 URL 등 인자는 보관하지 않고 실행 횟수만 센다.
    calls: Arc<AtomicUsize>,
}

impl GitRunner for CountedGit {
    /// 모든 Git 실행을 기존 guard와 runner에 맡긴다.
    fn run(
        &self,
        path: &Path,
        args: &[&std::ffi::OsStr],
    ) -> git_cleanup_tray::error::Result<GitOutput> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        ProcessGitRunner::new().run(path, args)
    }
}

/// 동일한 조건을 재현할 측정 옵션.
struct Options {
    /// 저장소 구성. all은 네 구성을 모두 실행한다.
    case: String,
    /// 저장소 동시 실행 수. 없으면 1·2·4를 각각 측정한다.
    parallelism: Option<usize>,
    /// 단일 방식만 실행할 경우 commands 또는 gix. 기본 both.
    backend: String,
    /// 준비 실행을 제외한 반복 수.
    iterations: usize,
    /// 각 방식의 준비 실행 수.
    warmups: usize,
    /// 원본 표본을 저장할 CSV 경로.
    output: PathBuf,
    /// 메모리 측정을 위해 스레드 종료 뒤 일시 대기할 초. 기본 0.
    hold_seconds: u64,
    /// 메모리 비교에서 외부에서 준비한 파일 구성의 네 저장소를 읽기 전용으로 재사용한다.
    fixture_manifest: Option<PathBuf>,
}

impl Options {
    /// 의존성을 추가하지 않고 측정에 필요한 옵션만 읽는다. 잘못된 입력은 실패시킨다.
    fn parse() -> Self {
        let mut options = Self {
            case: String::from("all"),
            parallelism: None,
            backend: String::from("both"),
            iterations: 30,
            warmups: 5,
            output: std::env::temp_dir().join("git-read-benchmark.csv"),
            hold_seconds: 0,
            fixture_manifest: None,
        };
        let mut args = std::env::args().skip(1);
        while let Some(key) = args.next() {
            let value = args.next().expect("each option needs a value");
            match key.as_str() {
                "--case" => options.case = value,
                "--parallelism" => {
                    options.parallelism = Some(value.parse().expect("parallelism integer"));
                }
                "--backend" => options.backend = value,
                "--iterations" => options.iterations = value.parse().expect("iterations integer"),
                "--warmups" => options.warmups = value.parse().expect("warmups integer"),
                "--output" => options.output = value.into(),
                "--hold-seconds" => {
                    options.hold_seconds = value.parse().expect("hold seconds integer");
                }
                "--fixture-manifest" => options.fixture_manifest = Some(value.into()),
                _ => panic!("unknown benchmark option: {key}"),
            }
        }
        assert!(options.iterations > 0);
        assert!(["all", "small", "refs", "worktrees", "files"].contains(&options.case.as_str()));
        assert!(["both", "commands", "gix"].contains(&options.backend.as_str()));
        assert!(options.fixture_manifest.is_none() || options.case == "files");
        options
    }
}

/// 측정이 끝날 때까지 임시 저장소를 보관하고 사용자 저장소는 사용하지 않는다.
struct Fixture {
    /// 측정 뒤 삭제할 임시 저장소 소유자.
    _sandboxes: Vec<Sandbox>,
    /// 검사할 등록 경로.
    paths: Vec<PathBuf>,
}

impl Fixture {
    /// 준비된 입력을 열거나 지정한 구성의 임시 Git 저장소를 만든다. 생성 시간은 제외한다.
    fn new(case: &str, manifest: Option<&Path>) -> Self {
        if let Some(manifest) = manifest {
            let paths: Vec<PathBuf> = serde_json::from_slice(
                &std::fs::read(manifest).expect("read prepared fixture manifest"),
            )
            .expect("prepared fixture path array");
            assert_eq!(paths.len(), 4, "files fixture needs four repositories");
            assert!(paths.iter().all(|path| path.is_absolute() && path.is_dir()));
            // 입력 경로의 소유권은 가져오지 않는다. 외부 저장소를 drop 시 삭제하면 안 된다.
            return Self {
                _sandboxes: Vec::new(),
                paths,
            };
        }
        if case == "worktrees" {
            let mut sandboxes = Vec::new();
            let mut paths = Vec::new();
            for _ in 0..8 {
                let sandbox = Sandbox::new();
                paths.push(sandbox.scenario().work);
                sandboxes.push(sandbox);
            }
            return Self {
                _sandboxes: sandboxes,
                paths,
            };
        }
        let sandbox = Sandbox::new();
        let count = match case {
            "refs" => 8,
            "files" => 4,
            _ => 16,
        };
        let mut paths = Vec::with_capacity(count);
        for index in 0..count {
            let path = sandbox.clone_with_default("trunk", &format!("측정 저장소 {index}"));
            if case == "refs" {
                make_history_and_refs(&sandbox, &path);
            }
            if case == "files" {
                make_files_and_worktree(&sandbox, &path, index);
            }
            paths.push(path);
        }
        Self {
            _sandboxes: vec![sandbox],
            paths,
        }
    }
}

/// 커밋 200개와 미병합 이력, 로컬 브랜치·custom ref 각각 1000개를 일괄 생성한다.
fn make_history_and_refs(sandbox: &Sandbox, path: &Path) {
    let initial = sandbox.oid(path, "HEAD");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let mut input = String::new();
    for index in 1..=200 {
        let parent = if index == 1 {
            initial.to_string()
        } else {
            format!(":{}", index - 1)
        };
        let message = format!("history {index}");
        let contents = format!("history {index}\n");
        writeln!(input, "commit refs/heads/trunk\nmark :{index}\ncommitter fixture <fixture@example.com> {} +0000\ndata {}\n{message}\nfrom {parent}\nM 100644 inline history.txt\ndata {}\n{contents}", timestamp + index, message.len(), contents.len()).expect("fixture string");
    }
    writeln!(input, "commit refs/heads/unmerged\ncommitter fixture <fixture@example.com> {} +0000\ndata 8\nunmerged\nfrom {initial}\nM 100644 inline unmerged.txt\ndata 4\nkeep\n", timestamp + 201).expect("unmerged history");
    sandbox.git_input(path, &["fast-import", "--quiet"], input.as_bytes());
    sandbox.git(path, &["reset", "--hard", "-q", "trunk"]);
    let history = sandbox.git(path, &["rev-list", "trunk"]);
    let ids: Vec<_> = history.lines().collect();
    let unmerged = sandbox.oid(path, "unmerged");
    input.clear();
    for index in 0..1000 {
        let id = if index % 5 == 0 {
            unmerged.as_str()
        } else {
            ids[index % ids.len()]
        };
        writeln!(
            input,
            "create refs/heads/완료-{index:04} {id}\ncreate refs/remote/작업-{index:04} {id}"
        )
        .expect("ref input");
    }
    sandbox.git_input(path, &["update-ref", "--stdin"], input.as_bytes());
    sandbox.git(path, &["gc", "--quiet"]);
}

/// 연결된 clean 워크트리 하나에 파일 10000개를 두어 파일 검사 비용을 비교한다.
fn make_files_and_worktree(sandbox: &Sandbox, path: &Path, index: usize) {
    let files = path.join("많은 파일");
    std::fs::create_dir(&files).expect("file fixture directory");
    for file in 0..10_000 {
        std::fs::write(files.join(format!("file-{file:05}.txt")), "unchanged\n")
            .expect("fixture file");
    }
    sandbox.git(path, &["add", "."]);
    sandbox.commit(path, "tracked files");
    sandbox.git(path, &["branch", "완료"]);
    sandbox.commit(path, "base advances");
    sandbox.add_worktree(path, &format!("파일 검사 worktree {index}"), "완료");
}

/// 출력 순서와 계획의 모든 필드를 비교한다. 조회 실패나 결과 차이가 있으면 중단한다.
fn assert_same_plans(scans: &[RepositoryScan], expected: &[RepositoryScan]) {
    assert_eq!(scans.len(), expected.len());
    for (scan, expected) in scans.iter().zip(expected) {
        assert_eq!(scan.repository_id, expected.repository_id);
        let plan = scan.outcome.as_ref().expect("benchmark scan must succeed");
        let expected = expected
            .outcome
            .as_ref()
            .expect("reference scan must succeed");
        assert!(plan.errors.is_empty(), "{:?}", plan.errors);
        assert!(expected.errors.is_empty(), "{:?}", expected.errors);
        assert_eq!(plan.repository_id, expected.repository_id);
        // Windows의 `/`와 `\`는 같은 경로 구분자다. Debug 문자열은 달라도
        // `Path` 비교는 이를 같은 경로로 판정하며 후보 안의 경로에도 동일하게 적용된다.
        assert_eq!(plan.repository_path, expected.repository_path);
        assert_eq!(plan.toplevel, expected.toplevel);
        assert_eq!(plan.base, expected.base);
        assert_eq!(plan.worktrees, expected.worktrees);
        assert_eq!(plan.branches, expected.branches);
        assert_eq!(plan.custom_refs, expected.custom_refs);
        assert_eq!(plan.skipped, expected.skipped);
    }
}

/// 두 방식을 같은 스레드 수로 번갈아 실행하고 계획 일치 확인과 원본 표본 저장을 수행한다.
fn benchmark(options: &Options, case: &str, csv: &mut impl std::io::Write) -> std::io::Result<()> {
    let fixture = Fixture::new(case, options.fixture_manifest.as_deref());
    let namespaces = default_namespaces();
    let requests: Vec<_> = fixture
        .paths
        .iter()
        .zip(1_u64..)
        .map(|(path, id)| ScanRequest {
            repository_id: RepositoryId(id),
            ..request(path, &namespaces, &[])
        })
        .collect();
    let command = CountedGit::default();
    let native = InProcessGitRunner::new(command.clone());
    let readers: [(&str, &dyn GitRunner); 2] = [("commands", &command), ("gix", &native)];
    let cancel = CancellationToken::new();
    let expected = batch::scan_all_with_parallelism(&command, &requests, &cancel, 1, &mut |_| {});
    let parallelism = options
        .parallelism
        .map_or_else(|| vec![1, 2, 4], |limit| vec![limit.max(1)]);
    for limit in parallelism {
        let mut samples = [Vec::new(), Vec::new()];
        for iteration in 0..options.warmups + options.iterations {
            for backend in [iteration % 2, 1 - iteration % 2] {
                let (name, runner) = readers[backend];
                if options.backend != "both" && options.backend != name {
                    continue;
                }
                command.calls.store(0, Ordering::Relaxed);
                let start = Instant::now();
                let scans = batch::scan_all_with_parallelism(
                    runner,
                    &requests,
                    &cancel,
                    limit,
                    &mut |_| {},
                );
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                let calls = command.calls.load(Ordering::Relaxed);
                assert_same_plans(&scans, &expected);
                if iteration >= options.warmups {
                    writeln!(
                        csv,
                        "{case},{name},{limit},{},{},{elapsed:.6},{calls}",
                        iteration - options.warmups + 1,
                        requests.len()
                    )?;
                    samples[backend].push(elapsed);
                }
            }
        }
        for (backend, values) in samples.iter_mut().enumerate() {
            if values.is_empty() {
                continue;
            }
            values.sort_by(f64::total_cmp);
            let p50 = values[(values.len() * 50).div_ceil(100) - 1];
            let p95 = values[(values.len() * 95).div_ceil(100) - 1];
            println!(
                "case={case} backend={} parallelism={limit} repositories={} samples={} p50_ms={p50:.3} p95_ms={p95:.3}",
                readers[backend].0,
                requests.len(),
                values.len()
            );
        }
        csv.flush()?;
    }
    Ok(())
}

/// 임시 저장소나 명시한 읽기 전용 fixture를 측정하고 필요할 때 종료 직전에 대기한다.
///
/// # Errors
/// 결과 CSV를 만들거나 기록하지 못하면 I/O 오류를 반환한다.
///
/// # Panics
/// 인자·fixture 생성·manifest 읽기에 실패하거나 두 조회 결과가 다르면 중단한다.
pub(crate) fn main() -> std::io::Result<()> {
    let options = Options::parse();
    let mut csv = std::io::BufWriter::new(std::fs::File::create(&options.output)?);
    writeln!(
        csv,
        "case,backend,parallelism,iteration,repositories,elapsed_ms,git_processes"
    )?;
    let cases: &[&str] = if options.case == "all" {
        &["small", "refs", "worktrees", "files"]
    } else {
        &[options.case.as_str()]
    };
    for case in cases {
        benchmark(&options, case, &mut csv)?;
    }
    csv.flush()?;
    if options.hold_seconds > 0 {
        println!("memory_sample_pid={}", std::process::id());
        std::io::stdout().flush()?;
        std::thread::sleep(Duration::from_secs(options.hold_seconds));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! 재사용 입력을 읽어도 해당 디렉터리의 소유권을 가져오지 않는지 확인한다.

    use super::Fixture;

    /// 외부에서 준비한 입력은 측정용 객체를 버린 뒤에도 삭제되면 안 된다.
    #[test]
    fn prepared_fixture_does_not_remove_input_directories() {
        let temporary = tempfile::tempdir().expect("test fixture directory");
        let paths: Vec<_> = (0..4)
            .map(|index| temporary.path().join(format!("준비된 저장소 {index}")))
            .collect();
        for path in &paths {
            std::fs::create_dir(path).expect("prepared directory");
        }
        let manifest = temporary.path().join("경로 manifest.json");
        std::fs::write(&manifest, serde_json::to_vec(&paths).expect("path array"))
            .expect("manifest");
        let fixture = Fixture::new("files", Some(&manifest));
        assert_eq!(fixture.paths, paths);
        drop(fixture);
        assert!(paths.iter().all(|path| path.is_dir()));
    }

    /// 구성과 다른 저장소 수를 조용히 비교하면 결과가 왜곡되므로 거부한다.
    #[test]
    #[should_panic(expected = "files fixture needs four repositories")]
    fn prepared_fixture_rejects_wrong_repository_count() {
        let temporary = tempfile::tempdir().expect("test fixture directory");
        let manifest = temporary.path().join("잘못된 경로 manifest.json");
        std::fs::write(
            &manifest,
            serde_json::to_vec(&[temporary.path()]).expect("path array"),
        )
        .expect("manifest");
        let _fixture = Fixture::new("files", Some(&manifest));
    }
}
