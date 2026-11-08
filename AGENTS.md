# AGENTS.md

Git Cleanup Tray 작업 지침. 사람과 AI coding agent 모두 이 문서를 따른다.
제품 요구사항과 설계의 원본은 [PLAN.md](PLAN.md)이며, 이 문서와 충돌하면 PLAN.md를 먼저 확인하고 둘 중 하나를 고친다.

## 1. 프로젝트 요약

- 여러 Git 저장소에서 base branch에 병합된 local branch, linked worktree, `refs/remote/*` custom ref를 안전하게 정리하는 tray-first 데스크톱 앱.
- Rust + Slint 1.18 (WebView/Electron/Node.js 금지). 대상: Windows 11 x64, Windows 11 ARM64, macOS Apple Silicon.
- License: `GPL-3.0-only`.

```text
src/main.rs      logger 초기화 + app::run()만 담당하는 얇은 binary
src/lib.rs       core library (integration test가 GUI 없이 사용)
  app/           tray·메인 창·작업을 묶는 application service (UI thread)
  git/           GitRunner, guard, ref/worktree/default branch
  cleanup/       planner(Scan) / executor(Execute) / plan·result 타입
  jobs/          worker thread, 취소
  storage/       app data directory의 JSON 저장
  platform/      OS별 코드 (cfg 분기는 여기에만)
  ui/            Slint 생성 code 노출, state → model 변환
ui/*.slint       declarative UI. 로직 없음
tests/           실제 임시 Git 저장소를 만드는 integration test
```

의존 방향: `ui` → `app` → `jobs` → `cleanup` → `git`. 역방향 의존을 만들지 않는다.

## 2. 빌드와 검증

작업을 끝냈다고 보고하기 전에 아래 명령이 모두 통과해야 한다.

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo deny check          # dependency를 추가·변경했을 때 필수 (cargo install cargo-deny)
cargo build --release     # 성능·크기에 영향을 주는 변경일 때
```

- clippy 경고를 `#[allow]`로 덮지 않는다. 꼭 필요하면 가장 좁은 범위에 붙이고 같은 줄이나 바로 위에 이유를 쓴다.
- 실패한 test를 지우거나 `#[ignore]` 처리해서 통과시키지 않는다.

## 3. 코드 스타일과 주석

### 3.1 주석은 촘촘하게, 모든 함수에 필수

- **모든 함수·메서드(private, test 함수 포함)에 `///` doc comment를 단다.** 무엇을 하는지, 왜 그렇게 하는지, 실패 조건을 쓴다.
- 모든 struct, enum, variant, field, const, trait, module(`//!`)에도 doc comment를 단다.
- `Cargo.toml`의 `missing_docs`, `clippy::missing_docs_in_private_items` lint가 이를 강제한다. 끄지 않는다.
- `Result`를 반환하는 pub 함수는 `# Errors` 절을, panic 가능한 pub 함수는 `# Panics` 절을 쓴다.
- 함수 내부에는 **"왜"** 를 설명하는 주석을 단다. 코드를 그대로 읽어주는 주석("i를 1 증가")은 쓰지 않는다. 안전 정책, Git 동작의 함정, 성능상 선택, OS별 차이처럼 코드만 봐서는 알 수 없는 이유가 대상이다.
- 주석 언어는 한국어, 식별자·Git 명령·ref 이름은 원문 그대로 backtick으로 감싼다 (예: `` `refs/remote/*` ``). 한글 조사가 붙으면 clippy가 식별자를 인식하지 못하므로 `doc_markdown` lint를 껐다. 대신 직접 backtick을 챙긴다.
- `.slint` 파일도 component, property, callback마다 `//` 주석을 단다.
- 모든 소스 파일 첫 줄에 `// SPDX-License-Identifier: GPL-3.0-only` (TOML은 `#`, SVG는 XML 주석).

### 3.2 코드 스타일

- Rust edition 2024, `rustfmt` 기본 + `max_width = 100`.
- 이름: Rust 표준 관례. Git 개념은 Git 용어 그대로 쓴다 (`worktree`, `ref`, `base_ref`).
- 의미 있는 값은 `String` 대신 domain type으로 감싼다 (예: `RepositoryId`, `BaseRef`). 잘못된 문자열 조합을 compile time에 막는 것이 목표다 (PLAN 16장).
- 라이브러리 코드에서 `unwrap()`/`expect()`/`panic!`을 쓰지 않는다. test와 "논리적으로 불가능"을 주석으로 증명할 수 있는 곳만 예외.
- `todo!()`/`unimplemented!()`/`dbg!()`를 commit하지 않는다. 미구현은 문서 주석에 Phase를 적고 log를 남긴다.
- 오류는 `crate::error::CleanupError` category로 표현한다 (PLAN 25장). 문자열 하나짜리 오류를 새로 만들지 않는다.
- `unsafe`는 `platform/` 안의 OS 연동에서만 허용한다. 쓸 때는 `// SAFETY:` 주석으로 불변조건을 문서화한다.

## 4. 성능과 백그라운드 메모리

이 앱은 하루 종일 상주한다. **idle 상태의 CPU 0%, 메모리 최소화가 기능 요구사항이다.** 모든 구현에서 성능을 먼저 고려한다.

### 4.1 idle 상태 (tray만 떠 있을 때)

- polling loop 금지. 기본값에서는 timer나 주기적 Git scan을 만들지 않는다. 사용자가 주기 최신화·정리를 켰을 때만 설정 간격의 일회성 timer를 예약한다. 다른 작업이나 Clean 확인 대기 중에는 timer를 멈추고 남은 시간을 보관하며, 끝나면 남은 시간부터 재개한다. 주기 작업이 끝난 뒤에는 전체 간격으로 다시 예약한다.
- 시작 시 저장소를 자동 scan하지 않고, 메인 창을 미리 만들지 않는다.
- 메인 창은 Open 시점에 만들고, 닫으면 숨기지 말고 instance를 drop한다 (`app::show_main_window` 참고). 단, 같은 process 안에서는 drop해도 renderer·폰트·힙 단편화가 남아 메모리가 돌아오지 않는다 (창을 열었다 닫은 뒤 37MB 측정). GUI를 별도 process로 분리하는 구조 변경을 검토 중이다.
- worker thread를 상주시키지 않는다. 작업 요청 시 생성하고 끝나면 종료한다. thread pool, async runtime(tokio 등)을 들이지 않는다.
- Scan 결과, 이력 등 큰 데이터를 메모리에 계속 들고 있지 않는다. 화면에 필요한 요약만 남기고 상세는 필요할 때 다시 만든다. 이력은 최근 100건으로 제한한다.
- 개별 Scan과 Scan All은 같은 저장소별 대기 목록을 사용하고 CPU 수에 따라 기본 2~4개까지 동시에 실행한다. 같은 저장소의 중복 실행과 변경 작업과의 겹침은 막는다. 완료 이벤트가 오면 다음 대기 항목을 시작하며, 취소하면 대기 항목을 버리고 실행 중인 worker를 회수한다.
- 기준값(2026-11-02, macOS, profiling build, tray only): CPU 0%, physical footprint 14.1MB (Phase 3 시점 약 16MB). 참고로 Slint 없이 AppKit `NSStatusItem`만 띄운 최소 앱도 12.7MB이며, 이 값이 macOS 트레이 앱의 하한이다. 변경 후 이 값이 눈에 띄게 늘면 원인을 찾아 PR에 적는다.
- 측정: `cargo build --profile profiling` 후 `vmmap --summary <pid> | grep "Physical footprint"`. 할당 위치는 `MallocStackLogging=lite`로 실행하고 `malloc_history <pid> -allBySize`로 본다.

### 4.2 실행 성능

- Git process 실행 횟수를 줄인다. ref 목록은 `git for-each-ref --format=...` 한 번으로 받고, 대상마다 따로 `rev-parse`를 부르지 않는다. 가능하면 `--stdin`/batch 형태 명령을 쓴다.
- Git 출력은 필요한 만큼만 읽는다. 큰 출력은 `Vec<u8>` 그대로 parsing하고 불필요한 `String` 변환·복사를 피한다 (`Cow`, slice, iterator 활용).
- 반복문 안에서 `clone()`, `format!`, `to_string()`으로 할당하지 않는다. `Vec`은 크기를 알면 `with_capacity`로 만든다.
- hot path가 아닌 곳에서 성급하게 최적화하지 않는다. 대신 성능이 걸리는 변경은 측정값(시간, `footprint`)을 남긴다.
- UI event loop를 block하지 않는다. Git 실행, 파일 I/O는 모두 worker thread에서 하고 결과만 `slint::invoke_from_event_loop`로 넘긴다.
- Slint model은 `VecModel`을 재사용해 바뀐 행만 갱신한다. 매번 전체 model을 새로 만들지 않는다.

### 4.3 빌드 설정

- Slint는 `default-features = false`로 쓰고 `renderer-software`를 고른다. OpenGL renderer(femtovg/skia)는 GPU context 때문에 상주 메모리가 커서 쓰지 않는다. 바꾸려면 측정값과 함께 근거를 남긴다.
- release profile: `lto = "fat"`, `codegen-units = 1`, `strip = true`. `panic = "abort"`는 worker panic이 tray process를 죽이므로 쓰지 않는다.
- `log`는 `release_max_level_info`로 debug/trace 호출을 release build에서 제거한다. hot path 로그는 `debug!` 이하로 둔다.

## 5. 라이브러리 선정

### 5.1 원칙

1. **항상 crates.io의 최신 stable 버전**을 쓴다. `Cargo.toml`에는 patch까지 명시한다 (`"1.18.1"`).
2. 후보가 여럿이면 다음 순서로 고른다.
   1. **업데이트가 꾸준한 것** — 최근 6개월 내 release, issue/PR 대응, RUSTSEC unmaintained 경고 없음.
   2. **성능이 좋은 것** — 실행 속도, 상주 메모리, binary 크기, 끌고 오는 transitive dependency 수.
   3. **사용자가 많은 것** — crates.io 최근 다운로드 수, 주요 프로젝트의 채택 여부.
3. 표준 라이브러리로 충분하면 crate를 추가하지 않는다 (예: 취소 flag는 `AtomicBool`, channel은 `std::sync::mpsc`).
4. default feature를 끄고 필요한 feature만 켠다.
5. GPL-3.0-only 배포와 호환되는 license만 쓴다 (`deny.toml`의 allow 목록). WebView 계열 crate는 `deny.toml`에서 금지했다.
6. 새 dependency를 추가할 때 PR/commit 설명에 후보 비교(최근 release 일자, 성능, 다운로드 수)와 선택 이유를 남긴다. 버전 확인은 `cargo search <name>` 또는 `https://crates.io/api/v1/crates/<name>`.

### 5.2 현재 선정 (2026-11-01 기준)

| 용도 | crate | 선정 이유 |
|---|---|---|
| GUI, tray | `slint` 1.18.1 | PLAN 확정. `SystemTrayIcon` 내장 |
| 오류 타입 | `thiserror` 2.0.21 | std `Error` derive만 생성, runtime 비용 없음 |
| 직렬화 | `serde` 1.0.229, `serde_json` 1.0.154 | 사실상 표준 |
| 로그 facade | `log` 0.4.35 | 2026-10 release로 `tracing`(2025-12)보다 꾸준하고 더 가볍다 |
| 로그 출력 | `env_logger` 0.11.11 | default feature를 꺼서 regex 등 제외 |
| app data 경로 | `dirs` 7.0.0 | `directories`(2025-01), `etcetera`(2025-10)보다 최근 release, 다운로드 최다 |
| 폴더 선택 창 | `rfd` 0.17.2 | Slint에 native 파일 선택 창이 없다. 2026-10 release, 최근 다운로드 약 1,700만. 경로 직접 입력 방식 대신 사용자가 선택 (2026-11-02). default feature는 Linux 전용이라 끈다 |
| 자동 시작 (Windows만) | `winreg` 0.56.0 | `HKCU\...\Run` 값 하나만 쓴다. `auto-launch`(2026-01, 약 280만)보다 최근 release이고 사용자가 많다 (약 4,900만). macOS는 LaunchAgent plist를 직접 쓴다 |
| test 임시 폴더 | `tempfile` 3.27.0 | 사실상 표준 |

## 6. 안전 불변조건 (설정으로 끌 수 없음)

PLAN 8, 15, 24장의 요약. 이 규칙을 약화하는 변경은 하지 않는다.

- Git 실행은 반드시 `git::runner::GitRunner`를 거친다. 다른 곳에서 `Command::new("git")`을 쓰지 않는다. UI에서 Git을 직접 실행하지 않는다.
- shell command string을 만들지 않는다. executable과 argument를 분리해서 넘기고, branch 이름·path 앞에는 `--`를 둔다.
- `git::guard`가 `branch -D/-f/--force`, `branch -r`, `worktree remove --force`, `push`, `reset`, `clean`, `stash`, `refs/remote/*` 밖이거나 `--no-deref` 없는 `update-ref -d`를 차단한다. Git은 long option 앞부분(`--forc`)도 받아들이므로 guard도 앞부분 일치로 판별한다. guard를 우회하거나 완화하지 않는다.
- force 삭제, remote branch 삭제, 자동 stash, worktree 폴더 직접 삭제 기능을 만들지 않는다. `.git` 내부 파일을 직접 수정하지 않는다.
- `refs/remote/*`(정리 대상)와 `refs/remotes/*`(절대 정리 안 함) 판별은 `git::refs`의 함수만 쓴다. 문자열 `starts_with`를 여기저기 새로 쓰지 않는다.
- base branch 이름을 추측하지 않는다 (`main`/`master` 하드코딩 금지).
- planner는 아무것도 바꾸지 않는다. executor는 mutation 직전에 대상마다 상태를 다시 확인하고, 바뀌었으면 `SkipReason::StateChanged`로 넘긴다.
- 정리는 dirty, locked, main worktree를 건드리지 않는다. 최신화는 dirty/locked worktree를 건드리지 않고, clean main worktree는 `merge --ff-only --no-autostash`로 갱신할 수 있다.
- Phase 2(safety test)가 끝나기 전에는 UI의 Clean 버튼을 executor에 연결하지 않는다.

## 7. 테스트

- 순수 parsing·판정 로직은 같은 파일의 `#[cfg(test)] mod tests`에 unit test를 둔다.
- Git 동작은 `tests/`에서 `tempfile`로 실제 임시 저장소를 만들어 검증한다. mock만으로 끝내지 않는다.
- 모든 Git 관련 test에 공백·한글 경로 케이스를 최소 하나 포함한다.
- 안전 불변조건을 건드리는 변경은 `tests/destructive_guards.rs` 또는 race-condition test(Scan 후 Execute 전에 상태 변경)를 함께 추가한다.
- test는 사용자의 실제 저장소나 전역 Git 설정을 건드리지 않는다. 필요하면 `GIT_CONFIG_GLOBAL`, `GIT_CONFIG_NOSYSTEM`으로 격리한다.

## 8. 플랫폼

- `#[cfg(windows)]`/`#[cfg(target_os = "macos")]` 분기는 `src/platform/` 안에만 둔다.
- 경로는 `Path`/`PathBuf`/`OsStr`로 다룬다. `to_str().unwrap()`으로 UTF-8을 가정하지 않는다.
- Windows에서 child process를 띄울 때는 `platform::configure_background_command`를 적용한다 (console 창 깜빡임 방지).
- macOS 배포 bundle은 Dock 아이콘이 뜨지 않도록 `Info.plist`에 `LSUIElement = true`를 넣는다 (Phase 7).
- login 자동 시작은 macOS `~/Library/LaunchAgents/GitCleanupTray.plist`, Windows `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 값으로 직접 구현했다 (`platform::autostart`). 등록되는 경로는 현재 실행 파일이다.
- cleanup 완료 OS 알림은 MVP에서 제외했다 (2026-11-02). 결과는 tray "Last result"와 Clean dialog로 보여준다. `AppSettings::notify_on_completion`은 저장 형식 호환을 위해 남겨 두었고 설정 화면에는 없다.

## 9. 로그와 개인정보

- 로그에 credential, token, password, credential이 들어간 remote URL을 남기지 않는다. Git 명령은 subcommand 이름만 기록한다 (`git::runner::describe_subcommand`).
- 사용자에게 보여줄 오류는 읽기 쉬운 문장으로, 상세 화면에는 Git stderr와 exit code를 그대로 보여준다.

## 10. 작업 방식

- PLAN.md 30장의 Phase 순서를 따른다. Phase 6(OS integration)까지 구현했고, Phase 7(Release)은 아직 진행하지 않았다.
- 한 번에 하나의 관심사만 바꾼다. 리팩터링과 기능 추가를 한 변경에 섞지 않는다.
- 설계를 바꾸는 결정(dependency 추가, renderer 변경, 저장 형식 변경 등)은 PLAN.md나 이 문서에 반영한다.
- 문서보다 동작하는 Git core와 자동화 test를 먼저 만든다.

- 저장소 추가는 native 폴더 다중 선택과 메인 창 외부 드롭을 지원한다. 입력한 폴더만 검증하며 하위 저장소를 재귀 검색하지 않는다. 같은 common Git directory는 한 번만 등록하고, 정상 항목을 일괄 저장한다. 다른 작업 중 들어온 드롭은 메모리 대기 목록에 보관했다가 작업 완료 후 처리한다.
- 최신화는 등록된 remote를 fetch하고 upstream이 있는 로컬 브랜치만 fast-forward한다. prune, tag 갱신, 자동 stash, rebase, merge commit을 만들지 않는다. fetch/갱신 오류가 있는 저장소는 해당 주기에서 정리하지 않는다. 주기 실행은 기본 OFF이며, 1~10080분 간격으로 사용자가 미리 동의한 최신화 → Scan → Clean을 확인 창 없이 실행한다. `config.json`의 `maintenance_interval_minutes`가 `null`이거나 없으면 OFF다.
- 외부 드롭은 Slint의 `unstable-winit-030` 창 이벤트 API를 사용한다. 업데이트 시 Windows Explorer와 macOS Finder의 다중 폴더 드롭을 확인한다. 드롭을 모으는 timer와 사용자가 켠 주기 작업 timer는 일회성으로 예약하며 idle polling은 하지 않는다.
