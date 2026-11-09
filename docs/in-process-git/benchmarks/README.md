<!-- SPDX-License-Identifier: GPL-3.0-only -->

# 플랫폼별 원본 측정값

- `windows-x64-2026-11-09.csv`: Windows x64에서 같은 네 구성·동시 실행 수 1·2·4·두 방식으로 각각 30회 측정한 720개 표본입니다. 단위와 제외 범위는 macOS 시간 CSV와 같습니다. Windows 경로는 Debug 문자열 대신 타입으로 비교합니다.
- `windows-x64-memory-2026-11-09.csv`: worktrees·files의 두 방식을 새 프로세스로 각각 30회, files의 두 방식을 각각 50회 실행한 뒤 읽은 6개 메모리·대기 CPU 표본입니다. `private_bytes`, `working_set_bytes`, `peak_working_set_bytes`의 단위는 bytes입니다. 주 비교 표본은 작업 종료 신호 후 약 5초 뒤 메모리를 읽고 다음 약 5초간 CPU 시간을 읽었습니다. 실제 간격도 함께 기록합니다. peak는 프로세스 안의 준비·검증을 포함하며 Git 하위 프로세스는 제외합니다. [재현 스크립트](measure-windows-memory.ps1)를 사용했습니다.

Windows 메모리 원본의 `fixture_cleanup`은 `rust`, `powershell-assisted`(files/commands 30회의 수동 정리 지원), `manual-after-ready`(files/gix 30회의 지연된 별도 관측), `shared-fixture`(외부에서 한 번 준비한 동일 입력 재사용)를 구분합니다. `cleanup_ms`는 확인한 PowerShell 정리 구간만 의미하며 Rust 정리나 재사용 입력의 최종 삭제는 포함하지 않습니다. `after_scan_ms`의 빈칸은 해당 간격을 기록하지 않았다는 뜻입니다. files 30회는 절차가 달라 참고값으로 보관하며 파일 구성의 두 방식 비교에는 같은 재사용 입력의 50회 결과를 사용합니다.

`cleanup_before_sample`은 관측 전에 PowerShell 전용 폴더 정리가 끝났는지 나타내며 미기록은 빈칸입니다. files/gix 30회는 완료 신호가 담긴 출력 파일의 마지막 수정 시각부터 약 777,895ms 후 관측했고 이 간격을 `ready_signal_age_ms`에 남겼습니다. 이 표본의 `observed_delay_ms`는 별도 관측을 시작하고 기다린 약 5초입니다. `comparison_scope`은 worktrees의 `paired-30`, files의 `paired-50`을 주 비교로 지정하고 files/commands 30회는 `reference-only`, files/gix 30회는 `additional-30`으로 구분합니다.

- `macos-2026-11-09.csv`: 구성 4종 × 동시 실행 수 1·2·4 × 방식 2종 × 30회, 총 720개 표본. `elapsed_ms`는 전체 저장소 Scan 완료 시간입니다. `git_processes`는 측정 중 `GitRunner::run`을 호출한 횟수입니다. 저장소 생성·준비 실행·결과 비교는 제외합니다.
- `macos-memory-50-2026-11-09.csv`: 파일이 많은 구성의 gix 모드를 새 프로세스에서 50회 반복한 추가 확인입니다.
- `macos-memory-2026-11-09.csv`: 같은 release 예제를 방식별 새 프로세스로 실행하고 준비 5회·측정 30회 뒤 임시 저장소와 작업 객체를 해제한 시점의 `vmmap --summary` 값입니다. `physical_footprint_peak`는 저장소 생성·검증을 포함한 프로세스 전체의 최고값이므로 Scan만의 peak로 해석하지 않습니다.

`commands`는 Git 명령 실행, `gix`는 라이브러리 내부 처리입니다. 단위·장비·재현 명령·제한 사항은 [macOS 보고서](../macos-results-2026-11-09.md)와 [Windows x64 보고서](../windows-results-2026-11-09.md)에 있습니다. Windows의 bytes·Working Set과 macOS의 physical footprint는 서로 다른 지표이므로 수치를 직접 비교하지 않습니다.
