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
                _ => panic!("unknown benchmark option: {key}"),
            }
        }
        assert!(options.iterations > 0);
        assert!(["all", "small", "refs", "worktrees", "files"].contains(&options.case.as_str()));
        assert!(["both", "commands", "gix"].contains(&options.backend.as_str()));
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
    /// 지정한 구성의 실제 Git 저장소를 만든다. 생성 시간은 측정에서 제외한다.
    fn new(case: &str) -> Self {
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

/// 출력 순서와 모든 후보·제외 사유까지 비교해 일부 결과를 빠뜨린 구현을 걸러낸다.
fn fingerprint(scans: &[RepositoryScan]) -> Vec<String> {
    scans
        .iter()
        .map(|scan| {
            let plan = scan.outcome.as_ref().expect("benchmark scan must succeed");
            assert!(plan.errors.is_empty(), "{:?}", plan.errors);
            format!("{plan:?}")
        })
        .collect()
}

/// 두 방식을 같은 스레드 수로 번갈아 실행하고 계획 일치 확인과 원본 표본 저장을 수행한다.
fn benchmark(options: &Options, case: &str, csv: &mut impl std::io::Write) -> std::io::Result<()> {
    let fixture = Fixture::new(case);
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
    let expected = fingerprint(&batch::scan_all_with_parallelism(
        &command,
        &requests,
        &cancel,
        1,
        &mut |_| {},
    ));
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
                assert_eq!(fingerprint(&scans), expected, "{case} / {name} / {limit}");
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

/// 임시 저장소만 측정하고 요청한 경우에만 OS 메모리 측정을 위해 종료 직전에 대기한다.
fn main() -> std::io::Result<()> {
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
