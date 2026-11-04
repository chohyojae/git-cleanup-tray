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
Scan으로 정리 대상을 확인하고, Clean은 확인 창에서 한 번 더 승인해야 실제로 삭제합니다.

- 삭제 대상: base branch에 병합된 local branch, 그 branch의 clean linked worktree, `refs/remote/*` ref
- 삭제하지 않는 것: remote branch(`refs/remotes/*`), tag, 변경 사항이 있거나 lock된 worktree, main worktree

설정과 저장소 목록은 아래 폴더의 JSON 파일에 저장됩니다. 등록한 저장소 안에는 아무 파일도 만들지 않습니다.

- macOS: `~/Library/Application Support/GitCleanupTray`
- Windows: `%APPDATA%\GitCleanupTray`

## License

GPL-3.0-only. [LICENSE](LICENSE) 참고.
