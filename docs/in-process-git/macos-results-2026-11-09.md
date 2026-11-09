<!-- SPDX-License-Identifier: GPL-3.0-only -->

# gix 조회 구현과 macOS 성능 비교

2026-11-09, `feature/in-process-git-proposals`. 비교 기준은 직전 커밋 `2d21476`의 Git 명령 조회를 유지한 구현입니다. 시간 비교에는 **같은 release 예제 안의 두 조회 구현, 같은 입력, 같은 저장소 동시 실행 수**를 사용했습니다. 동시 실행 4개 기준 p50은 구성에 따라 43.0~97.1% 줄었습니다. 같은 날 후속으로 수행한 [Windows x64 결과](windows-results-2026-11-09.md)는 별도 보고서에 있습니다.

## 구현한 범위

Auto Git 설정의 로컬 조회를 `gix 0.90.0`으로 연결했습니다. 저장소 식별, 로컬 기준 브랜치, ref·object 종류, 병합·조상 관계, 워크트리 목록·잠금, 파일 상태, upstream 설정을 앱 타입으로 반환합니다. Scan 동안만 저장소 객체를 재사용하며 정리·최신화 직전에는 새 상태로 다시 읽습니다.

개별 Scan과 Scan All은 기존 공유 대기 목록과 기본 2~4개 상한을 유지합니다. gix 내부 `parallel` 기능은 끄고 상태 검사 스레드 수는 1로 지정했습니다. 상주 스레드 풀·async runtime·idle polling은 추가하지 않았습니다.

삭제·워크트리 정보 정리·fetch·fast-forward·원격 HEAD 질의는 기존 Git 명령과 guard를 유지합니다. Custom Git, `GIT_DIR`, reftable, sparse checkout/index, fsmonitor, 외부 필터, partial clone, grafts, 비표준 `branch.*.merge` 설정은 Git 명령 조회를 사용합니다. 권한·손상 오류는 성공으로 처리하지 않습니다. [구현 범위와 후속 검증](migration-plan.md)

## 측정 조건

| 항목 | 값 |
|---|---|
| OS·장비 | macOS 27.2 (26B123), Apple M2 Ultra, 논리 코어 24개, 메모리 64GiB |
| 아키텍처·도구 | aarch64-apple-darwin, rustc 1.99.1, Apple Git 2.54.0 (Apple Git-157) |
| 빌드 | release, opt-level 3, fat LTO, codegen-units 1, strip |
| 동시 실행 수 | 두 방식 모두 저장소 1·2·4개. 각 저장소 내부 Git 명령은 기존 순서 유지 |
| 반복 | 방식·구성·동시 실행 수마다 준비 5회 + 기록 30회. 두 방식 실행 순서를 매회 교대 |
| 캐시·설정 | 반복 실행으로 OS 파일 캐시가 사용된 상태. 전역 Git 설정은 빈 파일, 시스템 설정은 비활성화. OS 캐시를 비우지 않음 |
| 측정 범위 | UI 없는 코어의 전체 저장소 Scan 완료 시간. 작업 스레드 생성·종료 포함 |
| 제외 | 임시 저장소 생성·정리, 준비 실행, 전체 계획 비교·CSV 기록. 네트워크·실제 삭제·최신화·UI 응답 시간 |

[측정 코드](../../examples/scan_benchmark.rs)는 두 방식 모두 `scan_all_with_parallelism`을 호출합니다. 매번 후보·제외 사유·기준 커밋·오류까지 전체 계획이 같은지 확인합니다. 모든 측정에서 계획이 일치했고 오류는 없었습니다. 공백·한글 경로를 사용하며 사용자 저장소는 읽지 않습니다.

| 구성 | 저장소 수 | 저장소 하나의 내용 |
|---|---:|---|
| small | 16 | 커밋 1개, 기준 브랜치, 연결 워크트리 없음 |
| refs | 8 | 커밋 202개, 추가 로컬 브랜치 1,000개·custom ref 1,000개, 20%는 미병합 이력. 여러 커밋으로 ref 분산, `git gc`로 압축 |
| worktrees | 8 | 메인 + 연결 워크트리 5개. clean·작업 파일 변경·staged·untracked·locked 혼합, 병합·미병합 브랜치와 custom ref |
| files | 4 | 저장소마다 추적 파일 10,000개, 같은 파일을 가진 clean 연결 워크트리 1개 |

## 전체 Scan 시간

아래 값은 각 구성에 포함된 **모든 저장소가 끝난 시간(ms)**입니다. 개별 저장소 하나의 시간은 아닙니다. p50·p95는 정렬한 30개 표본에서 nearest-rank로 계산했습니다. 단축률은 같은 동시 실행 수의 p50으로 계산했습니다.

| 구성 | 동시 실행 수 | Git 명령 p50 / p95 | gix p50 / p95 | p50 단축 |
|---|---:|---:|---:|---:|
| small | 1 | 791.87 / 808.50 | 14.51 / 16.38 | 98.2% |
| small | 2 | 433.45 / 451.75 | 8.87 / 9.45 | 98.0% |
| small | 4 | 245.74 / 251.50 | 7.15 / 9.21 | 97.1% |
| refs | 1 | 426.26 / 435.14 | 183.39 / 187.77 | 57.0% |
| refs | 2 | 235.04 / 244.18 | 103.99 / 108.10 | 55.8% |
| refs | 4 | 131.36 / 135.90 | 57.48 / 64.57 | 56.2% |
| worktrees | 1 | 712.43 / 744.41 | 51.81 / 57.93 | 92.7% |
| worktrees | 2 | 391.21 / 397.93 | 30.00 / 32.73 | 92.3% |
| worktrees | 4 | 224.73 / 232.95 | 21.36 / 28.94 | 90.5% |
| files | 1 | 898.58 / 940.27 | 631.58 / 723.14 | 29.7% |
| files | 2 | 567.16 / 602.09 | 355.06 / 377.52 | 37.4% |
| files | 4 | 462.87 / 501.24 | 263.67 / 335.27 | 43.0% |

측정 중 Git 실행 수는 전체 Scan 한 번당 small 96회, refs 48회, worktrees 80회, files 28회였고, gix 방식은 네 구성 모두 **0회**였습니다. 이는 `GitRunner::run` 호출 수이며 OS 전체의 프로세스 추적 결과는 아닙니다. 이 구성에는 네트워크·외부 필터가 없습니다.

동시 실행 자체와 인프로세스화 효과를 구분해야 합니다. 예를 들어 Git 명령의 1개 동시 실행과 gix의 4개 동시 실행을 비교해 라이브러리 효과라고 해석하지 않습니다. 작은 저장소에서는 프로세스 실행 비용을 없애는 효과가 컸고, ref·파일 검사 비중이 커지면 개선 비율은 작아졌습니다. 구성별 원인에 대한 해석이며 단계별 CPU 프로파일링 결과는 아닙니다.

## 메모리와 실행 파일 크기

메모리는 별도 실행에서 두 방식에 같은 release 예제를 사용했습니다. 구성마다 새 프로세스를 시작하고 동시 실행 4개, 준비 5회·측정 30회를 수행했습니다. 임시 저장소·계획·작업 스레드가 해제된 뒤 대기하는 시점에 `vmmap --summary <pid>`의 physical footprint를 한 번씩 읽었습니다.

| 구성 | Git 명령 | gix | 차이 |
|---|---:|---:|---:|
| worktrees, 30회 | 3.95MiB | 5.33MiB | +1.38MiB |
| files, 30회 | 3.62MiB | 67.80MiB | +64.17MiB |

이 값은 **코어 측정 예제의 작업 후 메모리**이며 실제 tray 앱의 idle 메모리가 아닙니다. 측정 반복에 따른 분산·장기 누적 증가와 UI를 열고 닫은 비용은 포함하지 않습니다. 원본 CSV의 peak는 저장소 생성과 검증도 포함하므로 Scan만의 최대 메모리로 쓰지 않았습니다. gix object cache는 저장소당 1MiB로 제한했지만 인덱스·그래프·메모리 매핑·할당기 잔류까지 1MiB로 제한되는 것은 아닙니다.

파일이 많은 구성은 **작업 후 메모리가 크게 남았습니다.** 추가로 새 프로세스에서 50회를 실행한 값도 57.3MiB였습니다. 30회보다 낮지만 실행마다 할당기 잔류가 달라지므로 지속 증가 여부나 상한을 확정할 수는 없습니다. 두 실행 모두 실제 할당 중인 메모리는 약 106KiB였고, 30회 실행에서는 `Malloc Small (empty)`가 62.0MiB, malloc zone의 dirty 영역이 64.5MiB였습니다. 작업 객체를 보관해서 생긴 메모리로 보기보다 **macOS 할당기가 해제된 영역을 유지하는 비용**으로 판단했습니다. [추가 측정·진단 근거](benchmarks/macos-memory-analysis.md), [50회 원본](benchmarks/macos-memory-50-2026-11-09.csv)

측정 장비는 메모리가 64GiB여서 여유 메모리가 OS·할당기의 해제 영역 유지에 영향을 줬을 가능성이 있습니다. 메모리 압력을 바꿔 비교하지 않아 이 원인은 확정하지 않았습니다. 위 수치는 실제 관측 비용으로 유지합니다.

후속 Windows 측정에서는 파일 구성의 작업 후 Private Bytes가 4.78→9.87MiB로 약 2.1배 늘었고, 동시 실행 4개 기준 Scan p50은 구성에 따라 79.0~98.1% 줄었습니다. 약 5.09MiB의 메모리 증가보다 속도 개선의 이점이 크다고 판단해 gix를 기본 로컬 조회로 채택합니다. macOS의 추가 메모리 최적화와 실제 tray 측정은 후속 개선으로 관리합니다. 두 OS의 메모리 지표는 서로 달라 수치를 직접 비교하지 않습니다.

같은 release 설정의 앱 실행 파일은 이전 **11,522,544 bytes → 14,205,728 bytes**로 2,683,184 bytes(약 2.56MiB, 23.3%) 커졌습니다. `Cargo.lock`의 전체 플랫폼 패키지는 546개에서 631개로 85개 늘었고 기존 패키지 버전 교체·제거는 없었습니다. 이는 실제 실행 시 모두 로드되는 라이브러리 수를 뜻하지 않습니다.

## 의존성 선택과 검사

도입 시 crates.io를 다시 확인했습니다. gix 0.90.0은 2026-10-22 배포, 최근 다운로드 13,637,347회이고 git2 0.21.0은 2026-05-18 배포, 18,777,583회였습니다. 둘 다 조사 당시 최신 stable이며 최근 6개월 내 배포됐습니다. gix의 Rust API와 작업별 캐시 수명을 선택했고, 이번 측정으로 Git 명령 대비 개선을 확인했습니다. git2와의 속도 우열은 측정하지 않았습니다. [gix 배포 정보](https://crates.io/api/v1/crates/gix), [git2 배포 정보](https://crates.io/api/v1/crates/git2), [후보 비교](alternatives.md)

```toml
# SPDX-License-Identifier: GPL-3.0-only
gix = { version = "0.90.0", default-features = false, features = [
    "revision", "status", "sha1", "sha256", "pack-cache-lru-static",
] }
```

`status`가 attributes·filter 관련 코드를 포함하므로 외부 필터 설정은 Git 명령으로 처리합니다. 네트워크·내부 parallel·worktree-mutation 기능은 켜지 않았습니다. [기능 정의](https://docs.rs/crate/gix/0.90.0/source/Cargo.toml.orig)

| 검사 | 결과 |
|---|---|
| `cargo fmt --check` | 통과 |
| `cargo clippy --all-targets -- -D warnings` | 통과 |
| `cargo test` | 106개 통과 |
| gix 모드 정리·기준 브랜치·안전·경합·최신화 테스트 | 42개 통과. 명령 주입용 recording runner는 기존 Git 실행 유지 |
| `cargo build --release --example scan_benchmark --bin git-cleanup-tray` | 통과 |
| `cargo deny check` | 실패: 기존 Slint 빌드 의존성의 유지보수 중단 권고 1건 |

`cargo deny`의 유일한 오류는 **RUSTSEC-2026-0441: pin-utils 0.1.1 unmaintained**입니다. 경로는 `slint-build/slint-macros 1.18.1 → spin_on 0.1.1 → pin-utils 0.1.1`이며 이전 lockfile에도 있었습니다. 이 권고는 유지보수 중단에 대한 정보로, 이번 검사에서 취약점이라고 판정한 것은 아닙니다. 조사 당시 상위 패키지의 최신 stable도 같은 버전이었습니다. gix 추가에 따른 새 권고·라이선스·금지 의존성·출처 오류는 없었습니다. 기존 정책을 완화하거나 권고를 ignore하지 않았으며 상위 의존성의 교체가 남아 있습니다. [RustSec 권고](https://rustsec.org/advisories/RUSTSEC-2026-0441.html)

## 재현 방법과 후속 확인

macOS에서 저장소 루트 기준으로 실행합니다. 예제가 직접 만든 임시 저장소만 사용합니다.

```sh
cargo build --release --example scan_benchmark
config_file=$(mktemp)
touch "$config_file"
GIT_CONFIG_GLOBAL="$config_file" GIT_CONFIG_NOSYSTEM=1 \
  target/release/examples/scan_benchmark \
  --case all --warmups 5 --iterations 30 --output /tmp/git-read-benchmark.csv
rm "$config_file"
```

`--parallelism 4`로 동시 실행 수를 고정할 수 있습니다. `--backend commands` 또는 `--backend gix`는 한 방식만 실행합니다. 메모리 측정은 `--case worktrees` 또는 `--case files`, `--parallelism 4 --hold-seconds 30`을 추가하고 출력된 `memory_sample_pid`에 `vmmap --summary`를 실행합니다. [시간 원본 720개 표본](benchmarks/macos-2026-11-09.csv), [메모리 원본](benchmarks/macos-memory-2026-11-09.csv)

Windows x64는 [별도 보고서](windows-results-2026-11-09.md)에 같은 예제·동시 실행 수로 측정한 결과를 기록했습니다. Windows ARM64, Git for Windows 설치별 설정, CRLF·attributes·긴 경로·대소문자·파일 잠금, 실제 tray의 작업 후 메모리·idle CPU, 더 긴 이력·많은 워크트리·shallow/alternates는 후속 검증입니다. 이번 macOS 결과를 Windows 개선율로 사용하지 않습니다. 삭제·최신화 전체 시간과 실제 UI에서 개별 요청을 겹쳐 넣는 응답 시간도 이번 수치에 포함하지 않았습니다.
