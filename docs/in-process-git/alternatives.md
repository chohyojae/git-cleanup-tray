<!-- SPDX-License-Identifier: GPL-3.0-only -->

# 라이브러리와 적용 범위 비교

2026-11-08에 crates.io API와 배포 소스를 확인했습니다. 아래 버전은 조사 당시 최신 안정 버전이며, 의존성을 추가할 때 다시 확인합니다. 현재 `Cargo.toml`·`Cargo.lock`은 변경하지 않았습니다.

세 안 모두 저장소마다 작업 스레드를 배정합니다. 동시 실행 수, 대상 저장소, 안전 검사, 취소 방식은 현재의 동시 Git 명령 실행과 동일하게 맞춥니다.

## 후보 현황

| 항목 | `gix` | `git2` |
|---|---|---|
| 최신 안정 버전 | `0.90.0` | `0.21.0` |
| 해당 버전 배포일, UTC | 2026-10-22 | 2026-05-18 |
| crates.io 최근 다운로드 수(`recent_downloads`) | 13,637,347 | 18,777,583 |
| Rust 패키지 라이선스 | MIT OR Apache-2.0 | MIT OR Apache-2.0 |
| 구현 | Gitoxide의 Rust API | C 라이브러리 libgit2의 Rust 바인딩 |
| Rust 버전 | 최소 1.88로 선언. 앱의 1.92에서 전체 의존성 빌드 확인 필요 | 최소 버전 선언 없음. 앱의 1.92에서 직접 확인 필요 |
| 빌드 기능 선택 | 기본 기능을 끄고 필요한 조회 기능만 선택 | 0.21.0의 기본 기능은 비어 있음. 로컬 조회에 HTTPS·SSH 불필요 |

근거는 [gix API](https://crates.io/api/v1/crates/gix), [git2 API](https://crates.io/api/v1/crates/git2)입니다. `git2` 패키지의 최종 갱신일은 2026-10-06이지만, 위 배포일은 해당 버전의 `created_at` 값입니다. 두 버전 모두 조사일로부터 6개월 안에 배포됐습니다. 다운로드 수는 사용자 수나 성능을 뜻하지 않으며, 이슈·PR 대응과 RUSTSEC 권고는 도입할 때 다시 확인합니다.

## A안: 조회는 `gix`, 삭제와 최신화는 Git 명령 — 우선 추천

`gix::Repository`로 저장소 경로, ref가 가리키는 대상과 종류, 브랜치·커밋 관계를 읽습니다. 워크트리 목록과 파일 변경 여부도 기존 결과와 비교한 뒤 옮깁니다. 삭제·fetch·fast-forward·원격 HEAD 질의는 `ProcessGitRunner`로 Git 명령을 실행합니다.

우선 실험할 의존성 설정입니다. `revision`은 커밋 관계 조회용이고 `sha1`은 SHA-1 저장소용입니다.

```toml
# SPDX-License-Identifier: GPL-3.0-only
gix = { version = "0.90.0", default-features = false, features = ["revision", "sha1"] }
```

파일 변경 검사 실험에는 `status`를 추가합니다. SHA-256 저장소는 `sha256`도 검증한 뒤 지원하며, 그전에는 Git 명령으로 처리합니다. `max-performance`, `parallel`, 네트워크, `worktree-mutation`은 처음부터 켜지 않습니다. 저장소별 스레드는 앱이 관리하고, 라이브러리 내부 병렬화는 별도로 측정해 스레드가 과도하게 늘지 않게 합니다. [0.90.0 기능 설정](https://docs.rs/crate/gix/0.90.0/source/Cargo.toml.orig)

`merge_base_with_graph()`는 반복 조회에서 커밋 그래프를 재사용할 수 있습니다. 제한된 캐시를 저장소 하나를 처리하는 동안만 유지하고, ref가 많으면 기준 커밋에서 한 번 이력을 걷는 방식과 비교합니다. 앱의 짧은 작업 수명에 맞는 구성이라는 판단이며, 더 빠르다는 결론은 실측 후 내립니다. [병합 관계 API](https://docs.rs/gix/0.90.0/gix/struct.Repository.html#method.merge_base_with_graph), [캐시 설명](https://docs.rs/gix/0.90.0/gix/#object-access-performance)

그대로 치환하면 안 되는 부분도 있습니다.

- `Repository::is_dirty()`는 **Git이 추적하지 않는 파일을 검사하지 않습니다.** 현재의 `worktree::is_clean()`을 대체하려면 `status(...).untracked_files(UntrackedFiles::Collapsed).into_iter(...)`로 스테이징한 변경, 작업 파일 변경, 추적하지 않는 파일을 함께 검사해야 합니다. 조회 오류도 처리하고 `status.showUntrackedFiles=no` 설정 때문에 검사가 생략되지 않게 합니다. [파일 변경 검사 소스](https://docs.rs/crate/gix/0.90.0/source/src/status/mod.rs)
- `worktrees()`는 연결된 워크트리만 반환합니다. 메인 워크트리를 포함하는 API와 대조하고, 잠금·경로 누락·권한 오류를 구분해야 합니다. 라이브러리가 충분한 정보를 주지 못하면 해당 조회는 Git 명령을 유지합니다. [워크트리 API](https://docs.rs/gix/0.90.0/gix/struct.Repository.html#method.worktrees)
- 저장소 객체는 설정·인덱스·ref를 캐시할 수 있습니다. 정리 확인을 기다리는 동안 보관하거나 삭제 직전 재검사에 이전 값을 그대로 쓰지 않습니다.
- `status` 기능에는 Git 속성·필터 처리가 포함됩니다. 외부 필터 프로그램을 실행할 수 있는 경로도 확인합니다. 라이브러리를 쓴다고 보조 프로세스까지 모두 없어지는 것은 아닙니다.

장점은 Rust 중심으로 의존성과 캐시를 관리할 수 있다는 점입니다. 고려할 비용은 0.x 버전의 API 변경, Git 설정·저장 형식별 검증, 파일 검사 기능의 추가 의존성입니다. 최종 실행 파일 크기와 전체 의존성 수는 아직 측정하지 않았습니다.

## B안: 같은 조회 작업에 `git2` 사용 — 비교 후보

`Repository::commondir()`, `references()`, `find_reference()`, `worktrees()`, `statuses()`와 커밋 그래프 API로 A안과 같은 정보를 얻습니다. 앱은 안전한 Rust 바인딩을 사용합니다. [Repository API](https://docs.rs/git2/0.21.0/git2/struct.Repository.html)

```toml
# SPDX-License-Identifier: GPL-3.0-only
git2 = { version = "0.21.0", default-features = false }
```

다음 차이를 확인해야 합니다.

- `graph_descendant_of(base, candidate)`는 두 커밋이 같으면 `false`를 반환합니다. Git과 같은 병합 판정을 하려면 `candidate == base`도 병합된 상태로 처리해야 합니다. [동일 커밋 주의사항](https://docs.rs/git2/0.21.0/git2/struct.Repository.html#method.graph_descendant_of)
- 파일 검사는 추적하지 않는 파일을 포함하고, Git이 무시하도록 설정한 파일은 제외합니다. HEAD와 인덱스, 인덱스와 작업 파일을 모두 비교하고 읽기 실패도 처리합니다. 인덱스를 저장하는 옵션은 켜지 않습니다. [StatusOptions](https://docs.rs/git2/0.21.0/git2/struct.StatusOptions.html)
- libgit2를 함께 빌드하거나 시스템 라이브러리에 링크하므로 세 플랫폼의 C 빌드 도구·링크·배포 결과를 확인해야 합니다. `vendored-libgit2` 사용 여부도 실험 후 정합니다. [빌드 설명](https://docs.rs/crate/git2/0.21.0/source/README.md)
- Rust 패키지와 별개로 libgit2는 GPLv2와 링크 예외 조항을 적용합니다. 포함된 C 코드의 고지·예외도 확인하며, `deny.toml`에 GPLv2를 포괄 허용하는 것으로 처리하지 않습니다. [libgit2 COPYING](https://github.com/libgit2/libgit2/blob/main/COPYING)

앱이 직접 작성하는 `unsafe`는 기존대로 `platform/`의 OS 연동에만 둡니다. 동일한 테스트 저장소와 동시 실행 수에서 A안보다 속도·메모리·호환성이 유리하면 B안을 선택합니다. 두 라이브러리를 최종 제품에 모두 넣지는 않습니다.

## C안: 조회와 삭제를 `gix`로 처리 — 후속 검토

A/B안으로 반복 조회를 옮긴 뒤에도 삭제 명령 실행이 주된 비용이면 검토합니다. 로컬 검사·정리를 라이브러리로 처리하고, 네트워크와 체크아웃된 워크트리의 최신화는 각각 따로 전환합니다.

`gix 0.90.0`에는 조건부 브랜치 삭제와 워크트리 제거 API가 있습니다. 다음 조건까지 만족해야 앱에서 사용할 수 있습니다.

| 작업 | 라이브러리 사용 조건 |
|---|---|
| 로컬 브랜치 삭제 | `delete_local_branches_if_unchanged()`로 확인한 대상과 일치할 때만 삭제. API 자체에는 병합 검사가 없으므로 기준 브랜치와 `git branch -d`의 upstream/HEAD 조건을 별도로 확인 |
| custom ref 삭제 | 삭제할 ref의 이전 OID와 일반 ref 여부를 함께 확인. symbolic ref가 가리키는 대상을 따라가지 않고 기존 namespace 제한 유지 |
| 연결된 워크트리 제거 | `prepare_remove_worktree()`와 `Target::remove(Force::Never, ...)` 검토. 메인·등록 워크트리 보호, 정확한 절대 경로, 일부 파일만 지워진 실패, 취소·Windows 파일 잠금 검증 |
| 오래된 워크트리 정보 정리 | Git의 만료 설정과 잠금·경로 누락 판정을 보존. 메타데이터 파일 직접 삭제로 대체하지 않음 |
| 체크아웃되지 않은 브랜치 최신화 | 이전 OID만 확인해서는 다른 워크트리의 체크아웃 경합을 막을 수 없음. 체크아웃된 브랜치와 fast-forward가 아닌 갱신을 모두 거부할 수 있을 때 전환 |
| 체크아웃된 워크트리 최신화 | ref·인덱스·파일·진행 중인 Git 작업·필터를 보호. 일반 머지나 강제 체크아웃을 `merge --ff-only --no-autostash` 대신 쓰지 않음 |

근거: [조건부 브랜치 삭제](https://docs.rs/gix/0.90.0/gix/struct.Repository.html#method.delete_local_branches_if_unchanged), [워크트리 제거](https://docs.rs/crate/gix/0.90.0/source/src/worktree/remove.rs), [Git 브랜치 삭제 조건](https://git-scm.com/docs/git-branch), [Git 워크트리 정책](https://git-scm.com/docs/git-worktree), [Git ref 조건부 변경](https://git-scm.com/docs/git-update-ref).

`git2::Branch::delete()`도 `git branch -d`와 같은 병합 검사를 약속하지 않습니다. `Worktree::prune()`도 `git worktree remove`와 같은 동작으로 취급할 수 없습니다. B안을 선택하더라도 단순한 함수 교체로 삭제까지 전환하지 않습니다. [브랜치 삭제 API](https://libgit2.org/docs/reference/main/branch/git_branch_delete.html), [Worktree API](https://docs.rs/git2/0.21.0/git2/struct.Worktree.html)

Git Credential Manager, SSH agent·설정, 사내 인증서·프록시·원격 연결 도구까지 맞춰야 하므로 네트워크 전환은 별도 검토합니다. 라이브러리 내부에서도 인증·전송용 보조 프로그램이 필요할 수 있습니다.

## 추천을 바꾸는 조건

Windows에서 `gix`보다 `git2`의 실측과 호환성이 좋으면 B안을 선택합니다. 두 라이브러리 모두 파일 검사에서 느리다면 ref와 병합 관계만 옮기고 파일 검사는 Git 명령으로 유지할 수 있습니다. C안은 동시 Git 실행과 A/B안을 비교한 뒤에도 남은 삭제 비용이 클 때 진행합니다.
