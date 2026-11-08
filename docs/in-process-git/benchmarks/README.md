<!-- SPDX-License-Identifier: GPL-3.0-only -->

# macOS 원본 측정값

- `macos-2026-11-09.csv`: 구성 4종 × 동시 실행 수 1·2·4 × 방식 2종 × 30회, 총 720개 표본. `elapsed_ms`는 전체 저장소 Scan 완료 시간입니다. `git_processes`는 측정 중 `GitRunner::run`을 호출한 횟수입니다. 저장소 생성·준비 실행·결과 비교는 제외합니다.
- `macos-memory-50-2026-11-09.csv`: 파일이 많은 구성의 gix 모드를 새 프로세스에서 50회 반복한 추가 확인입니다.
- `macos-memory-2026-11-09.csv`: 같은 release 예제를 방식별 새 프로세스로 실행하고 준비 5회·측정 30회 뒤 임시 저장소와 작업 객체를 해제한 시점의 `vmmap --summary` 값입니다. `physical_footprint_peak`는 저장소 생성·검증을 포함한 프로세스 전체의 최고값이므로 Scan만의 peak로 해석하지 않습니다.

`commands`는 Git 명령 실행, `gix`는 라이브러리 내부 처리입니다. 단위·장비·재현 명령·제한 사항은 [측정 보고서](../macos-results-2026-11-09.md)에 있습니다.
