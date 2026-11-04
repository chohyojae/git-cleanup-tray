# Git Cleanup Tray

여러 Git 저장소에서 base branch에 이미 병합된 local branch, linked worktree,
`refs/remote/*` custom ref를 안전하게 정리하는 tray-first 데스크톱 유틸리티입니다.
Rust + Slint로 만들며 WebView/Electron을 쓰지 않습니다.

- 설계: [PLAN.md](PLAN.md)
- 개발 지침: [AGENTS.md](AGENTS.md)

## 빌드

```bash
cargo build --release
```

## 사용법

```bash
cargo run --release
```

메뉴 막대(Windows는 알림 영역)에 아이콘이 뜹니다. Open → Add…로 저장소 폴더를 등록한 뒤
Scan으로 정리 대상을 확인하고, Clean은 확인 창에서 승인한 뒤 삭제합니다(Clean All 확인은 설정 가능).
Update All 또는 저장소의 Update는 등록 remote를 fetch한 뒤 upstream이 있는 로컬 브랜치를 fast-forward로 최신화합니다.
미커밋 변경이나 lock이 있는 worktree, 로컬 커밋이 앞서거나 갈라진 브랜치는 보존합니다.

Settings → Scheduled update and cleanup에서 자동 실행을 켜고 간격(1~10080분)을 설정할 수 있습니다.
기본은 꺼져 있으며, 켜면 앱이 실행되는 동안 enabled 저장소를 **최신화 → Scan → Clean** 순서로 확인 창 없이 처리합니다.
다른 작업이나 Clean 확인 대기 중에는 타이머가 정지하고, 끝나면 남은 시간부터 이어갑니다.
주기 작업이 끝나면 설정한 간격을 새로 기다립니다. fetch나 최신화에 실패한 저장소는 해당 실행에서 정리하지 않습니다.

- 삭제 대상: base branch에 병합된 local branch, 그 branch의 clean linked worktree, `refs/remote/*` ref
- 삭제하지 않는 것: remote branch(`refs/remotes/*`), tag, 변경 사항이 있거나 lock된 worktree, main worktree

설정과 저장소 목록은 아래 폴더의 JSON 파일에 저장됩니다. 등록한 저장소 안에는 아무 파일도 만들지 않습니다.

- macOS: `~/Library/Application Support/GitCleanupTray`
- Windows: `%APPDATA%\GitCleanupTray`

## License

GPL-3.0-only. [LICENSE](LICENSE) 참고.
