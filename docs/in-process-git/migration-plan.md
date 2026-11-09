<!-- SPDX-License-Identifier: GPL-3.0-only -->

# 동시 검사와 gix 조회 전환

2026-11-09 구현 반영. [macOS 측정 결과](macos-results-2026-11-09.md), [Windows x64 측정 결과](windows-results-2026-11-09.md), [추가 측정 계획](windows-benchmark.md)을 함께 봅니다.

## 연결 구조

개별 Scan과 Scan All이 공유하는 기존 대기 목록·동시 실행 상한을 유지하고 조회 구현만 바꿨습니다.

```text
화면 → 앱 → 저장소별 작업 관리 → 정리 계획·실행 직전 확인
                                  └─ GitRunner::open_read
                                     ├─ gix: 작업 스레드 안에서 열고 닫기
                                     └─ Git 명령: 기존 출력 파싱·guard

삭제·fetch·fast-forward·원격 HEAD 질의 → GitRunner::run → guard → git 실행 파일
```

[GitReadRepository](../../src/git/read.rs)는 `RepositoryIdentity`, `RefEntry`, `ObjectId`, `WorktreeEntry`, `TrackedBranch`를 반환합니다. 라이브러리 타입은 `git/` 밖으로 노출하지 않습니다. [기존 명령 구현](../../src/git/runner.rs)은 기본 조회 구현을 유지하므로 기록·경합 테스트용 runner도 사용할 수 있습니다.

| 단계 | 현재 상태 |
|---|---|
| 저장소별 동시 실행 | 완료. 기본 2~4개, 개별·일괄 Scan 상한 공유, 중복 방지·취소·결과 순서 유지 |
| 타입 기반 읽기 인터페이스 | 완료. Git 명령과 gix가 같은 앱 타입 반환 |
| 저장소·기준 브랜치·ref·커밋 관계 | 완료. 일반·symbolic ref와 tag object 구분, 같은 OID도 병합됨으로 처리 |
| 워크트리·파일 검사 | 완료. 메인·등록·detached·잠금·경로 누락, 스테이징·추적/미추적 파일·서브모듈 확인 |
| 실행 직전 조회 | 완료. 정리·최신화 재검사 때 새 조회 상태 사용, 상태 변경 시 건너뜀 |
| 앱 기본 연결 | Auto Git에서 gix 사용. Custom Git과 미지원 설정은 Git 명령 조회 |
| 같은 동시 실행 수 비교 | macOS와 Windows x64에서 1·2·4개씩 측정. Windows ARM64·추가 호환성·실제 tray 메모리는 후속 검증 |
| 파일 상태 검사 메모리 | 비용 수용. Windows 파일 구성은 작업 후 Private Bytes 약 +5.09MiB, Scan 속도 개선을 우선해 gix 채택. macOS 할당 영역 잔류·실제 tray 메모리는 후속 개선 |
| git2 비교·라이브러리 삭제 | 미구현. Windows 결과나 남은 삭제 비용이 필요성을 보일 때 검토 |

## 조회 수명과 안전 조건

- Scan 하나에서 저장소 객체를 재사용하고 기준 커밋을 고정합니다. 정리 확인 창이나 UI 상태에는 객체·인덱스·그래프를 보관하지 않습니다.
- gix 객체는 각 작업 스레드 안에서 생성합니다. 공유 저장소 객체·상주 스레드 풀·async runtime은 추가하지 않았습니다. gix 내부 `parallel` 기능도 껐고 파일 검사 스레드 수는 1로 지정했습니다.
- object cache는 저장소당 1MiB로 제한합니다. 이 값은 그래프·인덱스·메모리 매핑을 포함한 전체 메모리 상한은 아닙니다.
- 파일 상태는 전체 iterator를 끝까지 읽고 오류를 확인합니다. `status.showUntrackedFiles=no`여도 미추적 파일 순회를 복원합니다. 자동 인덱스 저장은 호출하지 않습니다.
- 경로가 사라진 연결 워크트리도 등록된 HEAD·브랜치 연결을 읽어 일반 브랜치 후보로 오인하지 않습니다. 확인할 수 없는 상태는 clean으로 처리하지 않습니다.
- 정리·최신화 직전에는 새 조회 상태를 열어 기준·ref·symbolic 여부·워크트리·잠금·파일 변경을 다시 읽습니다. 라이브러리 오류는 `CleanupError::RepositoryReadFailed`에 작업 종류와 원인을 담습니다.
- 삭제는 `git branch -d`, `git worktree remove`, 이전 OID를 지정한 `git update-ref --no-deref -d`를 유지합니다. 외부 Git 잠금 파일을 제거하지 않습니다.

Git 명령도 모든 경합을 원자적으로 막지는 않습니다. `git branch -d`는 검사 당시 OID를 삭제 조건으로 받지 않습니다. 라이브러리 조회가 이 제한을 해소했다고 보지 않습니다. 조건부 삭제 등은 C안에서 별도로 설계합니다.

## 미지원 설정과 Git 명령 전환

저장소를 열 때 다음 조건이면 해당 조회 상태 전체를 Git 명령 구현으로 선택합니다. 권한·손상 오류가 발생한 뒤 임의로 일부 결과를 합치지 않습니다.

- ref 저장 방식이 files가 아님(reftable 등)
- sparse checkout/index, fsmonitor, partial clone, legacy grafts
- 외부 필터 설정, 축약되거나 여러 개인 `branch.*.merge` 값
- 사용자가 Custom Git을 지정했거나 `GIT_DIR` 환경 변수를 사용함

SHA-1·SHA-256, packed refs, 일반 서브모듈은 실제 저장소 테스트에 포함했습니다. shallow·alternates·복잡한 attributes·Windows 경로/대소문자·Git for Windows 설치별 설정은 후속 호환성 검증이 필요합니다. 기능 미지원 이외의 오류를 자동으로 성공 처리하지 않습니다. 저장소 열기나 로컬 조회가 fetch·누락 object 다운로드를 시작하지 않습니다.

## 검증과 재현

기존 실제 Git 테스트를 두 방식으로 실행합니다. 기본 `cargo test`는 기존 명령 판정을 유지하고, 다음 명령은 같은 정리·기준 브랜치·안전·경합·최신화 테스트를 gix 조회로 실행합니다.

```sh
GIT_CLEANUP_TEST_READS=gix cargo test --test cleanup --test default_branch --test safety --test race_conditions --test sync
```

[추가 통합 테스트](../../tests/in_process.rs)는 Git 프로세스를 금지한 runner로 로컬 Scan을 검사합니다. 후보 OID·기준 판별 출처·제외 사유·오류 유무, 인덱스 내용·수정 시각, 원격 HEAD 질의, fallback, 서브모듈, 실행 전 상태 변경을 확인합니다.

[측정 예제](../../examples/scan_benchmark.rs)는 두 방식에 같은 `scan_all_with_parallelism`과 입력을 사용하고, 매번 전체 계획 일치를 확인합니다. 준비 실행 후 시간을 재며 결과 비교·저장소 생성은 시간에서 제외합니다. 원본 CSV·재현 명령·검사 결과는 측정 보고서에 남겼습니다.
