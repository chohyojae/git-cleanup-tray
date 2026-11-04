# Git Cleanup Tray 개발 계획서

> 상태: Draft 1.0  
> 대상: Windows / macOS  
> 기술 스택: Rust + Slint 1.18  
> 라이선스: GPL-3.0-only

## 1. 목적

Git 저장소를 여러 개 등록해 두고, 각 저장소에서 기본 브랜치에 이미 병합된 로컬 브랜치, 연결된 Git worktree, AI coding agent가 남긴 `refs/remote/*` custom ref를 안전하게 정리하는 경량 데스크톱 유틸리티를 개발한다.

프로그램은 시스템 트레이 또는 macOS 메뉴 막대에 상주하는 **tray-first application**으로 설계한다. 평상시에는 최소한의 리소스만 사용하고, 사용자가 관리 화면을 요청했을 때만 메인 GUI를 표시한다.

핵심 목표는 다음과 같다.

- Electron, WebView, JavaScript runtime을 사용하지 않는다.
- Windows와 macOS를 하나의 Rust 코드베이스로 지원한다.
- 시스템 트레이 중심으로 동작한다.
- Git의 파괴적 작업에는 보수적인 안전 정책을 적용한다.
- 저장소별 정리와 등록 저장소 전체 정리를 지원한다.
- `main`, `master` 같은 이름을 추측하지 않고 remote의 default branch를 판별한다.
- 애플리케이션과 소스 코드를 GPLv3에 맞춰 배포한다.

---

## 2. 기술 결정

### 2.1 언어

**Rust**를 사용한다.

Git 실행, filesystem 처리, 설정 저장, cleanup planning과 실행, tray event 처리 등 모든 application logic은 Rust로 구현한다.

### 2.2 GUI

**Slint 최신 버전**을 사용한다.

구성:

```text
Rust Process
├── Application Core
├── Git Core
├── Job Manager
├── Persistence
└── Slint UI
    ├── SystemTrayIcon
    ├── Tray Menu
    └── GUI Windows
```

UI는 `.slint` 파일에 declarative하게 정의하고, business logic은 Rust에 둔다.

### 2.3 Tray-first

앱 실행 시 메인 창을 반드시 표시하지 않는다.

기본 상태:

```text
Rust process
  +
Slint event loop
  +
System tray icon
```

사용자가 `Open`을 선택했을 경우에만 Main Window를 표시한다.

창을 닫더라도 애플리케이션은 종료하지 않고 tray에 남는다. 사용자가 명시적으로 `Quit`을 선택해야 종료한다.

### 2.4 제외 기술

다음 기술은 사용하지 않는다.

- Electron
- Tauri
- WebView2 기반 UI
- WKWebView 기반 UI
- Node.js
- React/Vue/Svelte 등 Web frontend runtime

---

## 3. 라이선스 정책

프로젝트는 **GNU General Public License v3.0 only**로 배포한다.

저장소 루트에 다음 파일을 둔다.

```text
LICENSE
```

Rust source file 등 필요한 위치에서는 SPDX identifier를 사용한다.

```text
SPDX-License-Identifier: GPL-3.0-only
```

`Cargo.toml`:

```toml
[package]
license = "GPL-3.0-only"
```

Slint 역시 GPLv3 라이선스 선택지를 사용한다.

### 3.1 Dependency license 관리

CI에서 dependency 라이선스를 자동 확인한다.

권장 도구:

- `cargo-deny`
- 필요 시 SBOM 또는 third-party license report 생성

새 dependency 추가 시 GPLv3 배포와 호환되는지 확인한다.

---

## 4. 지원 플랫폼

### MVP

- Windows 11 x64
- Windows 11 ARM64
- macOS Apple Silicon

Windows ARM64는 2026-11-02에 MVP 대상으로 옮겼다.

### 후속 검토

- macOS Intel
- Linux

MVP에서 Linux는 지원 범위에 포함하지 않는다. 단, core logic은 OS-specific code와 분리하여 향후 Linux 지원을 막지 않도록 한다.

---

## 5. 핵심 개념

### Repository

사용자가 앱에 등록한 로컬 Git repository.

### Base Branch

merge 여부 판단 기준이 되는 remote repository의 default branch.

### Cleanup Candidate

base branch에 이미 포함되어 안전 조건을 만족하는 branch, worktree 또는 custom ref.

### Custom Ref

AI coding agent 등이 임시 작업에 사용하는 Git ref.

기본 namespace:

```text
refs/remote/*
```

다음과 혼동하지 않는다.

```text
refs/remotes/*
```

`refs/remotes/*`는 cleanup 대상이 아니다.

---

## 6. 저장소 관리

사용자는 앱에 여러 Git repository를 등록할 수 있다.

Repository 항목 정보:

```text
- ID
- Display Name
- Repository Path
- Common Git Directory
- Enabled
- Remote Name
- Base Branch Override
- Custom Ref Namespaces
- Last Scan Time
- Last Cleanup Time
- Last Result
```

기본 remote name:

```text
origin
```

### 기능

- Add Repository
- Remove Repository
- Edit Repository
- Enable / Disable
- Open in Explorer / Finder
- Open Terminal
- Scan
- Cleanup

Repository 추가 시 다음을 검증한다.

```text
git rev-parse --git-dir
git rev-parse --git-common-dir
git rev-parse --show-toplevel
```

서로 다른 worktree 경로가 동일 repository를 가리키는 경우 중복 등록하지 않도록 common Git directory 기준으로 식별한다.

---

## 7. Default Branch 판별

base branch 이름을 하드코딩하거나 추측하지 않는다.

사용자가 override하지 않은 경우 다음 순서를 따른다.

### 1단계

로컬 symbolic ref 확인:

```text
git symbolic-ref --quiet --short refs/remotes/<remote>/HEAD
```

예:

```text
origin/main
origin/master
origin/trunk
```

### 2단계

symbolic ref가 없으면 remote에 질의한다.

```text
git ls-remote --symref <remote> HEAD
```

### 3단계

판별한 branch가 로컬에 존재하면:

```text
refs/heads/<branch>
```

를 base ref로 사용한다.

로컬 branch가 없고 remote-tracking ref만 있으면:

```text
refs/remotes/<remote>/<branch>
```

를 사용한다.

둘 다 없으면 검사하지 않고 `git fetch <remote>`가 필요하다는 상태를 보여준다.

### Offline 동작

`refs/remotes/<remote>/HEAD`가 이미 유효하면 remote network query 없이 검사할 수 있다.

---

## 8. Cleanup 대상

### 8.1 Local Branch

대상 namespace:

```text
refs/heads/*
```

다음 조건을 모두 만족해야 한다.

1. base branch가 아니다.
2. ref가 유효한 commit을 가리킨다.
3. candidate commit이 base commit의 ancestor이다.
4. 보호 대상이 아니다.

Merge 판정:

```text
git merge-base --is-ancestor <candidate-ref> <base-ref>
```

삭제:

```text
git branch -d -- <branch>
```

다음 명령은 사용하지 않는다.

```text
git branch -D
```

### 8.2 Worktree

조회:

```text
git worktree list --porcelain
```

연결된 branch가 merge되었더라도 worktree가 다음 조건을 모두 만족해야 삭제할 수 있다.

- linked worktree이다.
- 현재 main worktree가 아니다.
- path가 존재한다.
- locked 상태가 아니다.
- staged change가 없다.
- modified file이 없다.
- untracked file이 없다.

상태 확인:

```text
git -C <worktree> status --porcelain --untracked-files=normal
```

출력이 하나라도 있으면 자동 cleanup 대상에서 제외한다.

삭제 순서:

```text
1. 최신 상태 재검사
2. git worktree remove -- <path>
3. 성공하면 git branch -d -- <branch>
```

다음은 금지한다.

```text
git worktree remove --force
git reset --hard
git clean -fd
자동 stash
OS API를 이용한 worktree directory 강제 삭제
```

처리 마지막에는:

```text
git worktree prune
```

을 수행한다.

### 8.3 AI Agent Custom Ref

기본 대상:

```text
refs/remote/*
```

예:

```text
refs/remote/refactor/task001
refs/remote/review/pr-123
refs/remote/worktree/task042
```

candidate ref가 base commit의 ancestor인 경우에만 삭제할 수 있다.

삭제:

```text
git update-ref -d <full-ref>
```

### 8.4 명시적 제외 대상

절대로 자동 삭제하지 않는다.

```text
refs/remotes/*
refs/tags/*
refs/stash
refs/notes/*
remote repository의 실제 branch
base branch
사용자 보호 branch/ref
```

---

## 9. 검사와 실행 분리

Core API를 크게 두 단계로 나눈다.

```text
RepositoryInspector
        ↓
CleanupPlanner
        ↓
CleanupPlan
        ↓
CleanupExecutor
```

### Scan

mutation 없이 CleanupPlan만 만든다.

### Execute

CleanupPlan을 그대로 신뢰하지 않는다.

실제 삭제 직전에 대상마다 상태를 다시 확인한다.

재검증 항목:

- ref가 존재하는가
- base branch가 변경되지 않았는가
- 아직 merge된 상태인가
- worktree branch mapping이 동일한가
- worktree가 clean한가
- worktree가 lock되지 않았는가

조건이 바뀌었다면 삭제하지 않고 `Skipped`로 기록한다.

---

## 10. 메인 UI

메인 화면은 저장소 목록 중심으로 구성한다.

```text
Git Cleanup Tray

Repositories                         [Scan All] [Clean All]

Project A
C:\workspace\project-a
main                     3 removable / 0 warning
                              [Scan] [Clean]

Project B
~/workspace/project-b
trunk                    Clean
                              [Scan] [Clean]

Project C
~/workspace/project-c
master                   2 removable / 1 warning
                              [Scan] [Clean]
```

실제 UI는 Windows/macOS style과 window size를 고려해 재구성한다.

### Repository Row

표시 항목:

- name
- abbreviated path
- detected base branch
- status
- removable count
- warning count
- last scan

Action:

- Scan
- Clean
- Details
- More

---

## 11. Tray UI

Tray menu 기본 구성:

```text
Git Cleanup Tray
────────────────
Open
Scan All
Clean All
────────────────
Last result: ...
────────────────
Settings
About
Quit
```

### Tray 상태

가능하면 icon/tooltip에 상태를 반영한다.

```text
Idle
Scanning
Cleanup in progress
Attention required
Error
```

Tray interaction과 GUI가 동일 application state를 사용하도록 Rust application service에서 상태를 관리한다.

---

## 12. Scan All / Clean All

### Scan All

Enabled repository 전체를 순차 검사한다.

결과:

```text
Repository A    3 removable
Repository B    Clean
Repository C    2 removable, 1 skipped
```

### Clean All

바로 삭제하지 않는다.

1. 모든 저장소 재검사
2. 전체 CleanupPlan 생성
3. 요약 표시
4. 사용자 확인
5. 저장소 단위 순차 실행
6. 결과 표시

한 저장소에서 오류가 발생해도 다음 저장소를 계속 처리한다.

MVP에서는 병렬 cleanup을 사용하지 않는다.

---

## 13. Cleanup 확인 Dialog

삭제 전 최소 다음을 보여준다.

```text
Repositories       4
Worktrees          3
Local branches     8
Custom refs         5
Skipped             1
```

그리고 다음 문구를 명확히 표시한다.

```text
Remote branches will not be deleted.
Worktrees with uncommitted or untracked files will be skipped.
```

Primary action은 의미가 명확하도록:

```text
Clean
```

또는

```text
Clean 16 items
```

처럼 표현한다.

---

## 14. Detail 화면

CleanupPlan의 항목을 종류별로 표시한다.

### Worktrees

```text
worktree/task001
C:\...	ask001
Clean
```

### Local Branches

```text
refactor/task001
Already merged into main
```

### Custom Refs

```text
refs/remote/refactor/task001
Already contained in main
```

### Skipped

```text
feature/test
Worktree contains uncommitted changes
```

---

## 15. Settings

### General

- Start at login
- Start minimized to tray
- Show notification when cleanup completes
- Open main window on tray left-click

### Git

- Git executable path: Auto / Custom
- Default remote name
- Allow remote query when default branch is unknown

### Cleanup

- custom ref namespaces
- protected branch patterns
- confirmation before Clean All

안전 옵션 중 일부는 사용자가 해제할 수 없게 한다.

예:

```text
Skip dirty worktrees
Disable force branch deletion
Disable remote branch deletion
```

이 세 정책은 configurable option이 아니라 application invariant로 둔다.

---

### 주기 최신화·정리 (2026-11-04 추가)

- 수동 `Update All`/저장소 `Update`: 등록 remote fetch → 해당 remote의 upstream이 있는 로컬 branch fast-forward.
- fetch는 명시한 `refs/remotes/<remote>/*`만 갱신하며 prune/tag 갱신을 하지 않는다.
- checkout되지 않은 branch는 강제 갱신 없는 로컬 fetch를 사용한다. clean main/linked worktree는 `merge --ff-only --no-autostash --no-overwrite-ignore`로 갱신한다.
- dirty/locked worktree, 진행 중인 rebase/merge, 로컬 커밋이 앞서거나 갈라진 branch는 보존한다. branch·HEAD를 실행 직전에 다시 확인한다.
- Settings에서 자동 실행과 간격을 설정한다. `config.json`의 `maintenance_interval_minutes`는 `null`(기본 OFF) 또는 1~10080 사이 정수(분)다. 이전 설정 파일은 OFF로 읽는다.
- 켜면 앱이 실행 중인 동안 enabled 저장소에 최신화 → 새 Scan → 기존 executor로 Clean을 수행한다. 이 설정은 확인 창 없이 반복 정리하는 데 대한 사전 동의이며, 수동 Clean 확인 설정은 그대로 적용한다.
- 시작 직후에는 실행하지 않고 설정 간격을 기다린다. 다른 worker/폴더 선택/Clean 확인 대기 중에는 timer를 정지하고 남은 시간을 보관했다가 재개한다. 주기 작업이 끝나면 다음 간격을 처음부터 기다린다. 앱을 다시 시작하면 전체 간격으로 시작하며 누락된 회차를 몰아서 실행하지 않는다.
- fetch/브랜치 갱신/Scan에 오류가 있는 저장소는 자동 정리하지 않고 다음 저장소를 처리한다. 결과는 tray/메인 창과 기존 정리 이력에 남긴다. 자동 실행은 창을 만들지 않는다.
- dependency나 상주 thread를 추가하지 않고 Slint 일회성 timer와 기존 worker를 사용한다.

## 16. 데이터 모델

### RepositoryConfig

```rust
struct RepositoryConfig {
    id: RepositoryId,
    name: String,
    path: PathBuf,
    enabled: bool,
    remote_name: String,
    base_branch_override: Option<String>,
    custom_ref_namespaces: Vec<String>,
}
```

### CleanupPlan

```rust
struct CleanupPlan {
    repository_id: RepositoryId,
    base_branch: String,
    base_ref: String,
    base_commit: ObjectId,
    worktrees: Vec<WorktreeCandidate>,
    branches: Vec<BranchCandidate>,
    custom_refs: Vec<CustomRefCandidate>,
    skipped: Vec<SkippedItem>,
    errors: Vec<CleanupError>,
}
```

### CleanupResult

```rust
struct CleanupResult {
    repository_id: RepositoryId,
    removed_worktrees: Vec<PathBuf>,
    removed_branches: Vec<String>,
    removed_custom_refs: Vec<String>,
    skipped: Vec<SkippedItem>,
    errors: Vec<CleanupError>,
}
```

실제 구현에서는 domain type을 더 세분화해 잘못된 string 조합을 가능한 한 compile time에 줄인다.

---

## 17. 프로젝트 구조

```text
git-cleanup-tray/
├── Cargo.toml
├── Cargo.lock
├── LICENSE
├── README.md
├── deny.toml
├── build.rs
│
├── src/
│   ├── main.rs
│   ├── app/
│   │   ├── mod.rs
│   │   ├── state.rs
│   │   └── commands.rs
│   │
│   ├── git/
│   │   ├── mod.rs
│   │   ├── runner.rs
│   │   ├── repository.rs
│   │   ├── default_branch.rs
│   │   ├── refs.rs
│   │   └── worktree.rs
│   │
│   ├── cleanup/
│   │   ├── mod.rs
│   │   ├── planner.rs
│   │   ├── executor.rs
│   │   ├── candidate.rs
│   │   └── result.rs
│   │
│   ├── storage/
│   │   ├── mod.rs
│   │   ├── repository_store.rs
│   │   ├── settings_store.rs
│   │   └── history_store.rs
│   │
│   ├── jobs/
│   │   ├── mod.rs
│   │   ├── queue.rs
│   │   └── cancellation.rs
│   │
│   ├── platform/
│   │   ├── mod.rs
│   │   ├── autostart.rs
│   │   ├── notification.rs
│   │   └── shell_open.rs
│   │
│   └── ui/
│       ├── mod.rs
│       ├── bridge.rs
│       └── models.rs
│
├── ui/
│   ├── main-window.slint
│   ├── tray.slint
│   ├── repository-row.slint
│   ├── cleanup-preview.slint
│   ├── cleanup-result.slint
│   ├── repository-editor.slint
│   ├── settings.slint
│   └── components/
│
└── tests/
    ├── fixtures/
    ├── repository_scan.rs
    ├── cleanup_plan.rs
    ├── cleanup_execute.rs
    └── destructive_guards.rs
```

---

## 18. Git Process 실행 정책

shell command string을 만들지 않는다.

나쁜 예:

```text
"git -C " + path + " branch -d " + branch
```

대신 Rust의 process API로 executable과 argument를 분리한다.

개념적으로:

```text
Command("git")
args(["-C", repository, ...])
```

이 방식으로 다음 문제를 방지한다.

- 공백 포함 path
- 한글 path
- shell metacharacter
- command injection
- Windows/macOS quoting 차이

stdout/stderr/exit code를 구조화하여 반환하는 `GitRunner`를 만든다.

---

## 19. Concurrency

Slint UI event loop를 Git 작업으로 block하지 않는다.

Git scan/cleanup은 worker thread 또는 background task에서 수행한다.

완료된 결과만 UI thread에 전달한다.

규칙:

- 동일 repository에 두 job을 동시에 실행하지 않는다.
- Clean All은 기본적으로 repository를 순차 처리한다.
- UI button double-click에 의한 중복 실행을 막는다.
- 외부 Git process가 사용 중인 lock을 임의로 삭제하지 않는다.

---

## 20. Persistence

앱 설정은 OS별 app data directory에 저장한다.

Repository 자체에는 이 앱의 설정 file을 만들지 않는다.

초기 구현은 JSON으로 충분하다.

예:

```text
config.json
repositories.json
history.json
```

History는 최근 100회 정도로 제한한다.

향후 요구가 커지면 SQLite로 전환할 수 있도록 storage interface를 분리한다.

---

## 21. Logging

기록 대상:

- scan start/end
- cleanup start/end
- base branch 결정 방식
- Git command 종류
- exit code
- candidate 수
- removed item
- skipped item과 이유
- error

기록하지 않을 것:

- credential
- access token
- password
- credential이 포함된 remote URL

remote URL을 로그에 남겨야 한다면 credential을 마스킹한다.

---

## 22. OS Integration

### Windows

- notification area tray icon
- File Explorer에서 repository 열기
- 기본 terminal 또는 설정된 terminal 열기
- login startup 옵션
- Windows notification

### macOS

- menu bar status item
- Finder에서 repository 열기
- Terminal 열기
- login startup 옵션
- macOS notification

플랫폼별 구현은 `platform` module 내부로 격리한다.

---

## 23. UI/UX 원칙

- 작은 desktop utility답게 화면을 단순하게 유지한다.
- 기본 화면에서 Git 내부 용어를 지나치게 노출하지 않는다.
- 상세 화면에서는 실제 ref와 Git 오류를 확인할 수 있게 한다.
- 삭제 가능, skipped, error 상태를 색상만으로 구분하지 않는다.
- Keyboard navigation을 지원한다.
- screen reader가 action과 상태를 인식할 수 있게 accessibility 정보를 제공한다.
- 시스템 light/dark theme을 따른다.
- Windows/macOS에서 해당 플랫폼에 어색하지 않은 간격과 interaction을 사용한다.

---

## 24. 안전 불변조건

다음 조건은 제품의 핵심 invariant이므로 regression test를 둔다.

### 절대 수행하지 않음

```text
git branch -D
git worktree remove --force
git reset --hard
git clean -fd
git push --delete
```

### 절대 삭제하지 않음

```text
base branch
refs/remotes/*
remote repository branch
dirty worktree
locked worktree
현재 main worktree
```

### 반드시 수행

```text
실제 mutation 직전 상태 재검증
```

---

## 25. 오류 처리

오류를 단순 문자열 하나로 처리하지 않고 category를 둔다.

예:

```text
GitNotFound
NotARepository
RepositoryNotAccessible
RemoteNotFound
DefaultBranchUnknown
AuthenticationFailed
NetworkUnavailable
DirtyWorktree
LockedWorktree
GitCommandFailed
PermissionDenied
StateChanged
```

사용자 UI에는 읽기 쉬운 메시지를 표시하고, 상세 정보에서 Git stderr와 exit code를 확인할 수 있게 한다.

---

## 26. 테스트 전략

### Unit Tests

- remote HEAD parser
- default branch resolution
- porcelain worktree parser
- ref namespace filtering
- protected branch rules
- cleanup ordering
- state transition
- configuration validation

### Integration Tests

테스트에서 임시 Git repository를 실제 생성한다.

반드시 포함할 케이스:

```text
main default branch
master default branch
trunk default branch
custom-named default branch
merged local branch
unmerged local branch
clean linked worktree
dirty linked worktree
staged change
untracked file
locked worktree
refs/remote/* merged ref
refs/remote/* unmerged ref
refs/remotes/* 존재
한글 repository path
공백 포함 path
```

### Race-condition Tests

Scan 이후 Execute 이전에:

- branch에 새 commit 추가
- worktree file 변경
- worktree lock
- base branch 이동

등을 발생시키고 안전하게 skip되는지 확인한다.

### Destructive Guard Tests

mock/recording GitRunner를 이용해 다음 command가 절대 호출되지 않는지 검사한다.

```text
branch -D
worktree remove --force
push --delete
reset --hard
clean -fd
```

---

## 27. 라이선스 및 공급망 검사

CI pipeline:

```text
cargo fmt --check
cargo clippy
cargo test
cargo deny check
cargo build --release
```

Dependency 추가 PR에서는:

- license
- source
- 유지보수 상태
- 불필요한 native dependency
- binary size 영향

을 확인한다.

가능하면 dependency 수 자체를 작게 유지한다.

---

## 28. Performance 목표

정확한 수치는 prototype 단계에서 측정하되 방향은 다음과 같다.

### Idle

- tray-only 상태 CPU 사용량은 사실상 0에 가깝게 유지
- polling loop 금지
- 불필요한 주기적 Git scan 금지

### Startup

- 앱 시작 후 tray icon이 빠르게 표시될 것
- startup 시 모든 repository를 자동 scan하지 않을 것

### Main Window

- 사용자가 요청할 때 즉시 표시
- repository scan 때문에 window creation을 기다리지 않을 것

### Git Work

- Scan은 사용자 요청 또는 명시적으로 켠 주기 최신화·정리에서만 실행
- Git command output은 필요한 만큼만 읽음
- 전체 cleanup은 UI를 block하지 않음

---

## 29. 패키징 및 배포

### Windows

MVP artifact:

```text
GitCleanupTray.exe (x64, ARM64 각각)
```

ARM64 PC에서 x64 build를 emulation으로 돌리지 않도록 `aarch64-pc-windows-msvc` native build를 따로 만든다.

필요 시 installer 추가.

장기적으로 code signing을 적용한다.

### macOS

```text
Git Cleanup Tray.app
```

외부 배포 시 code signing과 notarization을 준비한다.

### Release

GitHub Release 등을 사용할 경우:

```text
Windows x64
Windows ARM64
macOS Apple Silicon
Source archive
SHA-256 checksums
Third-party license information
```

을 제공한다.

GPLv3 source availability 조건을 release process에 포함한다.

---

## 30. 구현 단계

### Phase 1: Rust Git Core

구현:

- GitRunner
- Repository identification
- DefaultBranchResolver
- branch/ref scanning
- worktree parsing
- CleanupPlanner
- CleanupExecutor

완료 조건:

GUI 없이 integration test만으로 실제 임시 repository cleanup을 검증할 수 있다.

### Phase 2: Safety Tests

구현:

- dirty worktree tests
- locked worktree tests
- protected ref tests
- race-condition tests
- destructive command guard tests

이 단계가 끝나기 전에 GUI의 `Clean` 버튼을 실제 executor에 연결하지 않는다.

### Phase 3: Slint Tray

구현:

- SystemTrayIcon
- tray menu
- Open
- Scan All
- Clean All
- About
- Quit

완료 조건:

메인 창 없이 tray만으로 앱이 정상 상주한다.

### Phase 4: Main GUI

구현:

- repository list
- repository 등록/삭제
- scan
- detail
- individual cleanup
- progress
- settings

### Phase 5: Clean All

구현:

- aggregate plan
- final confirmation
- sequential job execution
- cancellation
- result summary

### Phase 6: OS Integration

구현:

- startup at login
- native notifications
- Explorer/Finder open
- terminal open
- app data paths

### Phase 7: Release

- GPL files 확인
- `cargo-deny`
- Windows release build (x64, ARM64)
- macOS release build
- smoke test
- signing/notarization 준비

---

## 31. MVP 완료 조건

다음을 모두 만족해야 한다.

- Windows와 macOS에서 실행된다.
- WebView/Electron/Node.js 없이 동작한다.
- tray-only 상태로 정상 상주한다.
- 여러 repository를 관리한다.
- remote의 실제 default branch를 자동 판별한다.
- branch 이름을 `main`/`master`로 추측하지 않는다.
- repository별 Scan/Clean이 동작한다.
- Scan All/Clean All이 동작한다.
- merged local branch를 안전하게 삭제한다.
- clean linked worktree를 먼저 제거하고 branch를 삭제한다.
- dirty/locked worktree는 skip한다.
- merged `refs/remote/*` custom ref를 삭제한다.
- `refs/remotes/*`는 삭제하지 않는다.
- remote branch는 삭제하지 않는다.
- mutation 직전에 상태를 재검증한다.
- cleanup 결과와 skip 이유를 보여준다.
- destructive guard test를 통과한다.
- GPLv3 및 dependency license 검사를 통과한다.

---

## 32. Claude Code 구현 지침

Claude Code가 이 문서를 기반으로 구현할 때 다음 규칙을 따른다.

1. `docs`보다 먼저 working Git core와 automated tests를 만든다.
2. Git 실행 layer를 하나의 `GitRunner` abstraction으로 제한한다.
3. shell command string을 생성하지 않는다.
4. UI에서 직접 Git process를 실행하지 않는다.
5. CleanupPlan과 CleanupExecutor를 분리한다.
6. 모든 destructive operation 직전에 최신 Git 상태를 재검증한다.
7. dirty 또는 locked worktree를 자동 변경하지 않는다.
8. force deletion 기능을 만들지 않는다.
9. remote branch deletion 기능을 만들지 않는다.
10. `refs/remotes/*`를 cleanup namespace로 취급하지 않는다.
11. `refs/remote/*`와 `refs/remotes/*`를 코드와 test에서 명확하게 구분한다.
12. base branch 이름을 추측하지 않는다.
13. GUI를 Slint에 한정하고 application logic은 Rust에 둔다.
14. tray가 application lifetime의 기준이 되도록 한다.
15. main UI가 없어도 tray만으로 event loop가 정상 동작하게 한다.
16. worker 작업으로 UI event loop를 block하지 않는다.
17. Windows/macOS-specific 코드는 `platform` module에 격리한다.
18. 공백/한글/Unicode path를 항상 고려한다.
19. `unsafe` 사용은 특별한 플랫폼 연동 외에는 피하고, 사용할 경우 목적과 safety invariant를 문서화한다.
20. 모든 새 dependency는 GPLv3 compatibility를 확인한다.

---

## 33. 초기 결정 사항

현재 확정:

```text
Language        Rust
GUI             Slint 1.18
Architecture    Tray-first native desktop app
Platforms       Windows + macOS
WebView         사용하지 않음
Electron        사용하지 않음
Base Branch     remote default branch 자동 판별
Custom Ref      refs/remote/*
Remote Branch   삭제하지 않음
Dirty Worktree  삭제하지 않음
License         GPL-3.0-only
```

향후 구현 과정에서 변경 가능한 사항:

```text
제품명
아이콘
설정 저장 포맷
notification 세부 구현
installer 형식
macOS Intel 지원 여부
```

---

## 부록 A. 기존 도구 조사 및 신규 개발 타당성

### A.1 조사 목적

본 프로젝트를 신규 개발하기 전에, 동일하거나 유사한 문제를 이미 해결하는 도구가 있는지 확인하였다. 특히 다음 요구사항의 조합을 기준으로 대안을 검토했다.

- Windows와 macOS 지원
- 여러 Git repository 등록 및 관리
- tray/menu bar 상주
- merge 완료된 local branch 자동 탐지 및 정리
- 연결된 clean worktree 자동 정리
- dirty/locked worktree 보호
- AI coding agent가 남긴 `refs/remote/*` custom ref 정리
- repository별 Scan/Clean
- 전체 repository 대상 Scan All/Clean All
- remote default branch 자동 판별
- remote repository branch 및 `refs/remotes/*`는 삭제하지 않는 보수적 정책

조사 결과, 개별 기능을 제공하는 CLI와 GUI는 존재하지만 위 요구사항을 모두 만족하는 애플리케이션은 확인하지 못했다. 따라서 본 프로젝트는 기존 Git client 또는 worktree manager와 경쟁하는 범용 Git GUI가 아니라, **여러 repository에서 AI-assisted development 이후 남은 local Git 작업 흔적을 안전하게 정리하는 tray-first utility**로 범위를 한정한다.

### A.2 `milehighideas/git-cleanup`

Git worktree와 merge 완료 branch 정리를 자동화하는 Bash 기반 CLI이다. AI agent tooling으로 worktree가 누적되는 상황을 직접적인 문제로 설명하며, merge 완료 worktree/branch 정리, dry-run, dirty worktree 보호 등을 제공한다.

본 프로젝트와 겹치는 부분:

- merge된 branch 정리
- worktree 정리
- 기본적으로 dirty worktree 보호
- dry-run 성격의 사전 확인
- AI agent로 인한 worktree 누적 문제 해결

차이점:

- GUI와 tray가 없다.
- 여러 repository를 등록해 일괄 관리하는 desktop UX가 없다.
- Bash 4 이상을 요구하므로 Windows/macOS 공통 native utility로 쓰기 어렵다.
- remote branch 정리 기능도 제공하므로 본 프로젝트의 보수적 정책과 범위가 다르다.
- `refs/remote/*` custom ref를 핵심 대상으로 다루지 않는다.

결론: cleanup 정책과 safety test의 참고 구현으로 활용하되 제품 자체의 대체재로 보지 않는다.

### A.3 Worktree Tool (`gregnazario/worktree-ui`)

Rust와 GPUI로 구현된 native worktree GUI이다. Windows, macOS, Linux, FreeBSD용 패키지를 제공하며 worktree 목록, dirty 상태, upstream ahead/behind, 생성/제거/prune, terminal/file manager 연동 등을 지원한다.

본 프로젝트와 겹치는 부분:

- Rust native desktop application
- Windows/macOS 지원
- worktree 상태 시각화
- worktree remove/prune
- terminal/file manager 연동

차이점:

- tray-first cleanup utility가 아니다.
- merge 완료 branch/worktree 전체 정리가 핵심 workflow가 아니다.
- 등록된 여러 repository에 대한 Scan All/Clean All이 핵심 기능이 아니다.
- `refs/remote/*` custom ref cleanup을 제공하지 않는다.
- 현재 프로젝트가 GPUI에 기반하므로 Slint 기반의 최소 tray utility라는 아키텍처 목표와 다르다.

결론: worktree parsing, 상태 표시, cross-platform packaging 및 UX를 참고할 가치가 높다. 그러나 원하는 제품을 만들기 위해 fork하면 기존 범용 worktree manager의 구조와 기능을 상당 부분 변경해야 하므로 신규 전용 앱이 더 단순한 구조를 유지할 가능성이 높다.

### A.4 GitHub Desktop

GitHub Desktop은 현재 worktree 생성, 전환, 이름 변경, 삭제를 지원한다. 따라서 수동 worktree 관리는 이미 성숙한 범용 Git GUI에서도 해결되고 있다.

차이점:

- tray-first cleanup utility가 아니다.
- 여러 repository의 merge 완료 local artifact를 한 번에 정리하는 workflow가 없다.
- AI agent custom ref cleanup을 제공하지 않는다.
- remote branch를 건드리지 않는 전용 cleanup policy와 Scan/Clean preview를 중심으로 설계된 제품이 아니다.

결론: 본 프로젝트가 worktree 자체를 관리하는 범용 UI를 재구현해서는 안 된다는 근거로 삼는다. 신규 앱은 **cleanup automation**에 집중한다.

### A.5 Worktree Manager (`guoyongchang/worktree-manager`)

Tauri 2 + React + Rust 기반의 worktree management application으로 multi-repository/workspace, terminal integration 및 AI/MCP 관련 기능을 제공하는 활발한 프로젝트다.

본 프로젝트와 겹치는 부분:

- 여러 repository/workspace를 다루는 GUI
- Rust backend
- AI-assisted development workflow 고려
- worktree 관리

차이점:

- Tauri/WebView 기반이며 본 프로젝트의 no-WebView 요구와 맞지 않는다.
- tray에서 가볍게 상주하는 cleanup 전용 utility가 핵심 목적이 아니다.
- merge 완료된 local branch/worktree와 `refs/remote/*`를 보수적으로 일괄 정리하는 workflow가 핵심 기능이 아니다.

결론: multi-repository UX와 AI workflow 관점의 참고 자료로 사용한다. 기술 기반과 제품 범위 때문에 직접 채택하지 않는다.

### A.6 Axis

Windows/macOS/Linux를 지원하는 Git client로 worktree 추가, 목록, 열기, 제거 및 stale worktree prune 기능을 제공한다.

차이점:

- 범용 Git client이다.
- merge 완료 artifact cleanup을 여러 repository에 대해 수행하는 tray workflow가 아니다.
- custom ref cleanup을 목표로 하지 않는다.

결론: worktree의 기본 CRUD 자체는 차별화 요소가 아니며, 본 프로젝트에서는 필요 이상의 Git client 기능을 추가하지 않는다.

### A.7 Worktree Helper

Windows 11용 WPF/.NET tray application으로, 한 repository의 worktree를 tray에서 확인하고 VS Code, terminal, File Explorer로 열 수 있다. uncommitted path와 upstream ahead/behind도 표시한다.

본 프로젝트와 겹치는 부분:

- tray-first UX
- main window를 닫아도 tray 상주
- worktree 상태를 빠르게 확인
- 개발자용 경량 utility라는 제품 성격

차이점:

- Windows 전용이다.
- cleanup automation이 핵심 기능이 아니다.
- merge 완료 local branch/worktree 자동 정리가 없다.
- custom ref cleanup이 없다.
- multi-repository Clean All workflow가 없다.

결론: 제품 UX 측면에서 가장 가까운 참고 사례 중 하나다. 특히 tray/menu와 작은 popup/window interaction을 설계할 때 참고한다.

### A.8 Shell script 계열

공개된 여러 Bash script와 개인 자동화 사례에도 다음 패턴이 존재한다.

- merged branch 검색
- branch에 연결된 worktree 검색
- dirty/untracked/unpushed 상태 검사
- clean worktree 삭제
- local branch 삭제
- `git worktree prune`

따라서 Git cleanup algorithm 자체는 새로운 아이디어가 아니며, 본 프로젝트는 알고리즘을 새로 발명하는 것을 목표로 하지 않는다.

차별점은 이 검증된 CLI pattern을 다음과 같이 제품화하는 데 있다.

```text
기존 script
    ↓
현재 repository 한 곳에서 수동 실행

본 프로젝트
    ↓
등록 repository 목록
    ↓
안전한 Scan/Plan
    ↓
개별 Clean 또는 Clean All
    ↓
tray에서 지속적으로 접근
```

### A.9 Git 자체 기능

최신 Git도 merge 완료 branch 정리를 위한 기능을 계속 개선하고 있다. 따라서 자체 Git history algorithm을 구현하기보다 가능한 범위에서는 Git이 제공하는 ref/merge/worktree 명령을 조합하고, 애플리케이션은 orchestration, safety policy, multi-repository management와 UX를 담당한다.

원칙:

- Git이 제공하는 merge/ancestor 판정을 신뢰한다.
- `.git` 내부 파일을 직접 수정하지 않는다.
- ref 삭제는 Git plumbing command를 사용한다.
- worktree는 filesystem 삭제 대신 `git worktree remove`를 사용한다.
- 새 Git 버전에 더 안전하고 직접적인 명령이 추가되면 core implementation 단순화를 검토한다.

### A.10 대안 비교 요약

| 대안 | GUI | Tray | Windows + macOS | Multi-repo | Merged branch/worktree cleanup | `refs/remote/*` cleanup | 본 프로젝트 대체 가능성 |
|---|---|---|---|---|---|---|---|
| `git-cleanup` | 아니오 | 아니오 | 제한적 | 아니오 | 예 | 확인되지 않음 | 낮음 |
| Worktree Tool | 예 | 아니오 | 예 | 제한적 | 수동 중심 | 아니오 | 중간 |
| GitHub Desktop | 예 | 아니오 | 예 | 예 | 수동 중심 | 아니오 | 낮음 |
| Worktree Manager | 예 | 주 목적 아님 | 예 | 예 | 관리 중심 | 확인되지 않음 | 중간 |
| Axis | 예 | 아니오 | 예 | 예 | 수동 중심 | 아니오 | 낮음 |
| Worktree Helper | 예 | 예 | Windows만 | 아니오 | 아니오 | 아니오 | 낮음 |
| 본 프로젝트 | 예 | 예 | 예 | 예 | 예 | 예 | 요구사항 충족 |

### A.11 Build vs. Adopt 결정

기존 제품을 그대로 도입하는 대신 신규 개발을 선택한다.

결정 사유:

1. **요구사항 조합을 만족하는 기존 앱이 없다.**
   개별 기능은 존재하지만 tray + Windows/macOS + multi-repository + safe cleanup + AI custom ref cleanup의 조합은 확인되지 않았다.

2. **범용 Git client가 필요하지 않다.**
   commit graph, staging, merge UI, diff editor 등의 기능은 범위 밖이다. 기존 Git client를 확장하면 불필요한 복잡성을 함께 가져오게 된다.

3. **tray-first와 작은 idle footprint가 제품 요구사항이다.**
   본 도구는 하루 종일 실행되는 작은 utility이므로 일반 Git desktop client와 lifecycle이 다르다.

4. **cleanup safety policy가 제품의 핵심이다.**
   remote branch 삭제, force removal 등의 기능을 지원하지 않는 좁은 정책이 의도적인 차별점이다.

5. **AI agent custom ref가 별도 요구사항이다.**
   `refs/remote/*` 같은 임시 namespace를 일반 Git GUI가 UI에 노출하거나 정리하는 경우는 확인하지 못했다.

6. **새 구현의 범위가 충분히 작다.**
   Git 자체를 재구현하는 것이 아니라 Git CLI를 안전하게 orchestration하는 작은 Rust service와 Slint UI를 작성한다.

### A.12 재발명하지 않을 부분

신규 앱을 개발하더라도 다음 영역은 새로 발명하지 않는다.

- Git merge 판정
- Git ref storage
- worktree metadata 관리
- branch deletion semantics
- native Git credential/authentication
- Git network protocol

모두 설치된 `git` executable을 사용한다.

앱이 새로 제공하는 것은 다음 계층이다.

```text
Safety policy
+ Cleanup planning
+ State revalidation
+ Multi-repository orchestration
+ Tray-first UX
+ AI custom-ref cleanup
```

### A.13 향후 재평가 조건

다음 중 하나가 발생하면 신규 개발 필요성을 다시 평가한다.

- 기존 cross-platform worktree manager가 tray mode와 multi-repository safe cleanup을 공식 지원
- `refs/remote/*` 또는 configurable custom ref cleanup 지원 등장
- Git 자체에 worktree + merged branch + custom ref를 안전하게 일괄 정리하는 상위 명령 추가
- 유지보수 비용이 기존 프로젝트 fork 비용보다 커짐

기존 대안은 경쟁 제품이 아니라 지속적인 참고 대상으로 취급하고, release 전후에 기능 중복 여부를 재확인한다.


