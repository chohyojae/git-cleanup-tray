# Git Cleanup Tray 작업 인계

## 현재 상태 (2026-11-04, 재부팅 후 재개)

사용자가 작업 재개를 요청했습니다. **Scan 최적화를 제외한 등록·드롭 구현과 자동 검증, Windows release 빌드를 완료했습니다.**
실제 다중 폴더 선택·외부 드롭 UI 조작은 아래 도구 연결 문제로 확인하지 못했습니다.
완료한 무거운 테스트나 빌드를 변경 없이 다시 실행하지 마세요. 기존 IDE나 사용자 앱 프로세스는 종료하지 마세요.

- 작업 범위: native 폴더 다중 선택, 일괄 등록, 메인 창 외부 드롭.
- Scan 최적화와 성능 실측은 보류입니다. `src/git/`와 `src/cleanup/`는 변경하지 않았습니다.
- 사용자 실제 저장소와 앱 설정은 변경하지 않았습니다. 기동 검증에서는 설정을 읽기만 했습니다.
- 사용자 요청으로 첫 커밋을 amend해 `.idea` 추적 파일을 제거하고 `.gitignore`에 `.idea/` 전체를 등록했습니다.
  로컬 `.idea` 파일은 보존했습니다. 등록·드롭 구현과 검증 결과는 별도 main 커밋으로 정리합니다.

## 구현

- `src/app/registration.rs`: 정상 항목 일괄 저장, common Git directory 중복 제외, 실패 상세 최대 50개.
- `src/app/mod.rs`: `rfd::AsyncFileDialog::pick_folders()`, 드롭 대기, 일회성 100ms timer, 결과 표시.
- `src/app/windows.rs`: Slint `unstable-winit-030`의 hover/drop 이벤트 연결.
- `ui/main-window.slint`: 다중 선택·드롭 안내와 hover 표시.
- worker 생성 실패 시 등록 경로가 사라지던 문제를 수정했습니다.
  `RegistrationQueue::start_ready`가 `Arc<Vec<PathBuf>>`로 경로를 넘기고, 시작 실패 시 대기 목록에 되돌립니다.
  정상 실행에서는 경로 목록 전체를 복사하지 않습니다. 실패 후 새 입력을 합쳐 재시도하는 단위 테스트도 있습니다.
- 시작 시 자동 Scan, 상주 worker, idle polling은 추가하지 않았습니다.

## 검증 결과

모든 단위·통합 테스트 **75개가 분할 실행으로 통과**했습니다. 일부 묶음은 재부팅 전 통과 결과를 유지했습니다.

| 묶음 | 통과 수 | 확인 시점 |
| --- | ---: | --- |
| 단위 테스트 | 29 | 재부팅 전 |
| batch | 3 | 재부팅 전 |
| cleanup | 5 | 재부팅 전 |
| default_branch | 5 | 재부팅 전 |
| destructive_guards | 4 | 재부팅 전 |
| race_conditions | 13 | 재부팅 후 |
| registration | 6 | 재부팅 후 |
| safety | 10 | 재부팅 후, 실패한 fixture 수정 후 해당 케이스 재실행 |

- `cargo fmt --check`: 최종 테스트 수정 후 통과.
- `cargo clippy --locked --all-targets -j 1 -- -D warnings`: 최종 테스트 수정 후 통과.
- 재부팅 후 실행: `cargo test --locked -j 1 --test race_conditions --test registration --test safety -- --test-threads=1`.
  race_conditions 13개와 registration 6개 통과. safety는 9개 통과 후 기존 fixture 1개에서 실패했습니다.
- `tests/safety.rs`의 `special_character_names_are_not_interpreted`가 Windows 파일명에 쓸 수 없는
  `>`와 `"`를 포함한 ref를 만들다가 실패했습니다. shell 해석 방지 검증을 유지하면서 모든 대상 OS에서
  파일명으로 쓸 수 있는 `$()`, `${IFS}`, 세미콜론, 작은따옴표·백틱 payload로 바꿨습니다.
  테스트를 삭제하거나 ignore하지 않았습니다.
- 수정 후 `cargo test --locked -j 1 --test safety special_character_names_are_not_interpreted -- --exact --test-threads=1`: 통과.
- `cargo test --locked -j 1 --doc`: 통과 (문서 예제 0개).
- `target/tools/bin/cargo-deny.exe --locked check`: 재부팅 전 통과.
  `advisories ok, bans ok, licenses ok, sources ok` 확인. 중복 버전과 사용하지 않은 license 허용 항목 경고는 남습니다.
  결과 로그: `target/deny-check.log`. 이후 dependency 변경은 없습니다.
- `cargo build --release --locked -j 1`: 통과. 낮은 프로세스 우선순위(BelowNormal)로 순차 실행했고 3분 43초 걸렸습니다.
- `git diff --check`: 통과.

## 실행 파일과 기동 확인

- 실행 파일: `target/release/git-cleanup-tray.exe` (14,006,272 bytes).
- sandbox 기동에서는 `Shell_NotifyIconW(NIM_ADD) failed`가 발생했습니다.
- 실제 사용자 세션에서 다시 기동했을 때는 해당 오류 없이 `tray started`와 저장소 목록 로드 로그를 확인했습니다.
  20초 동안 조기 종료가 없었고, 마지막 5초의 CPU 사용 시간은 0ms였습니다.
- 참고 표본: working set 16,728,064 bytes, private bytes 5,328,896 bytes.
  짧은 Windows 관측값이며 macOS physical footprint나 장시간 메모리 측정을 대신하지 않습니다.
- 로그: `target/startup-desktop.stderr.log`, 관측값: `target/startup-desktop.json`.
- 기동 검증에서 만든 프로세스는 모두 종료했습니다. 사용자 IDE와 기존 앱은 종료하지 않았습니다.

## 남은 실제 UI 확인

- 재부팅 후에도 `cua.getState()`가 `Computer Use native pipe is unavailable`와 파일 없음(os error 2)을 반환했습니다.
  `@oai/sky` 초기화도 시간 초과였습니다.
- 따라서 실제 Windows 다중 폴더 선택·Explorer 다중 드롭·작업 중 드롭 대기를 UI로 조작하지 못했습니다.
  등록·중복 처리·대기 목록·worker 생성 실패 후 재시도는 자동 테스트로 확인했습니다.
- macOS Finder 드롭과 Apple Silicon 기동은 이 Windows 환경에서 확인하지 못했습니다.
- 도구 연결이 복구되면 실제 UI 조작만 추가 확인하세요. 이미 통과한 테스트와 빌드를 자동으로 반복하지 마세요.

## Scan 조사 메모 (구현 보류)

- 현재 기본 Git 호출은 `6 + K`회이며, ref 조회 정보를 합치면 `4 + K`회로 줄이는 대안이 있습니다.
- `cat-file --batch-command`는 객체 조회용이며 범용 Git 명령 서버가 아닙니다.
- `git2`/`gix` 도입은 시간·메모리·바이너리 크기와 현재 안전 정책 호환성을 비교한 뒤 결정해야 합니다.
- Execute의 GitRunner·guard·삭제 직전 재검증은 유지해야 합니다.
- `gix::Repository::is_dirty()`는 untracked를 제외하므로 그대로 대체하면 안 됩니다.
- `D:\GitRepositories\aip-1`의 느림 원인은 앱의 ProcessGitRunner를 직접 계측하지 않아 확정하지 않았습니다.
