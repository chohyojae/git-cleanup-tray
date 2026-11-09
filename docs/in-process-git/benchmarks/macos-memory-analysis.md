<!-- SPDX-License-Identifier: GPL-3.0-only -->

# macOS 파일 상태 검사 후 메모리

release 측정 예제, files 구성(저장소 4개, 각 연결 워크트리의 추적 파일 10,000개), 동시 실행 4개입니다. 각 행은 별도 프로세스로 실행했습니다. 준비 5회 이후 기록한 반복 수이며 작업 객체·계획·임시 저장소를 해제한 뒤 `vmmap --summary`를 읽었습니다.

| 조회·반복 | Physical footprint | malloc zone dirty | malloc zone bytes allocated | malloc zone fragmentation | Malloc Small (empty) dirty |
|---|---:|---:|---:|---:|---:|
| Git 명령 30회 | 3712K | 2224K | 105K | 2119K | 528K |
| gix 30회 | 67.8M | 64.5M | 106K | 64.4M | 62.0M |
| gix 50회 | 57.3M | 54.4M | 106K | 54.3M | 50.6M |

단위는 `vmmap`의 K/M 표기를 그대로 옮겼습니다. malloc zone 수치는 모든 zone의 TOTAL 행입니다. physical footprint는 malloc 이외의 비용도 포함하므로 두 열이 정확히 같지는 않습니다.

30회와 50회 모두 live allocation은 약 106KiB인 반면, 해제된 small 영역의 dirty 페이지가 대부분 남았습니다. 저장소 객체를 drop해도 OS 관점의 메모리가 즉시 돌아오지 않는 현상입니다. object cache 1MiB 상한으로 이 비용까지 제한할 수는 없습니다.

측정 장비의 메모리는 64GiB입니다. 여유 메모리가 OS·할당기의 해제 영역 유지에 영향을 줬을 가능성은 있지만, 메모리 압력을 바꿔 비교하지 않아 원인을 확정하지는 않습니다. 이 두 점만으로 장기 증가·누수 없음·특정 상한을 보장하지 않습니다.

[Windows 결과](../windows-results-2026-11-09.md)의 메모리 증가와 큰 속도 개선을 함께 고려해 gix를 기본 로컬 조회로 채택했습니다. 실제 tray 메모리와 할당 패턴 개선은 후속 개선으로 관리합니다. 임의의 malloc 강제 정리나 상주 스레드 풀은 이번 변경에 넣지 않았습니다.
