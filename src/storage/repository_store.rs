// SPDX-License-Identifier: GPL-3.0-only

//! 등록 저장소 목록 (`repositories.json`) 저장과 로드.
//!
//! 저장소 등록 UI는 Phase 4에서 만든다. 그 전까지는 이 파일을 직접 편집해 등록할 수 있도록
//! `id`, `name`, `path` 외의 필드는 생략하면 기본값을 쓴다.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::git::repository::RepositoryId;

/// 저장 파일 이름.
pub const FILE_NAME: &str = "repositories.json";

/// 기본 remote 이름.
pub const DEFAULT_REMOTE: &str = "origin";

/// 기본 custom ref namespace (PLAN 8장).
pub const DEFAULT_CUSTOM_REF_NAMESPACE: &str = "refs/remote/";

/// 등록 저장소 하나의 설정 (PLAN 16장 `RepositoryConfig`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryConfig {
    /// 앱이 부여한 식별자.
    pub id: RepositoryId,
    /// 화면에 표시할 이름.
    pub name: String,
    /// 저장소 경로 (worktree 중 하나일 수 있다).
    pub path: PathBuf,
    /// 중복 등록 판별용 common Git directory. Phase 4 등록 화면에서 채우며 Scan에는 쓰지 않는다.
    #[serde(default)]
    pub common_dir: PathBuf,
    /// Scan All / Clean All 대상 여부.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// default branch 판별에 쓸 remote 이름.
    #[serde(default = "default_remote_name")]
    pub remote_name: String,
    /// 사용자가 지정한 base branch. `None`이면 자동 판별한다.
    #[serde(default)]
    pub base_branch_override: Option<String>,
    /// 정리할 custom ref namespace 목록. 기본값은 `refs/remote/`.
    #[serde(default = "default_custom_ref_namespaces")]
    pub custom_ref_namespaces: Vec<String>,
}

/// `enabled` 기본값. 새로 등록한 저장소는 Scan All 대상이다.
fn default_enabled() -> bool {
    true
}

/// `remote_name` 기본값 ([`DEFAULT_REMOTE`]).
fn default_remote_name() -> String {
    String::from(DEFAULT_REMOTE)
}

/// `custom_ref_namespaces` 기본값 ([`DEFAULT_CUSTOM_REF_NAMESPACE`] 하나).
fn default_custom_ref_namespaces() -> Vec<String> {
    vec![String::from(DEFAULT_CUSTOM_REF_NAMESPACE)]
}

/// `dir`의 `repositories.json`을 읽는다. 파일이 없으면 빈 목록이다.
///
/// # Errors
///
/// 파일을 읽지 못하거나 JSON이 깨졌으면 [`super::load_json`]의 오류.
pub fn load(dir: &Path) -> Result<Vec<RepositoryConfig>> {
    super::load_json(&dir.join(FILE_NAME))
}

/// `dir`의 `repositories.json`에 `repositories`를 저장한다.
///
/// # Errors
///
/// 저장에 실패하면 [`super::save_json`]의 오류.
pub fn save(dir: &Path, repositories: &[RepositoryConfig]) -> Result<()> {
    super::save_json(&dir.join(FILE_NAME), repositories)
}

/// 새 저장소에 줄 식별자. 기존 최댓값 + 1이며, 지운 저장소의 id를 다시 쓰지 않도록 재사용하지 않는다.
#[must_use]
pub fn next_id(repositories: &[RepositoryConfig]) -> RepositoryId {
    let max = repositories.iter().map(|repo| repo.id.0).max().unwrap_or(0);
    RepositoryId(max.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 필수 필드만 적은 항목에 기본값이 채워지는지 확인한다.
    #[test]
    fn omitted_fields_use_defaults() {
        let json = r#"[{"id": 7, "name": "프로젝트", "path": "/tmp/작업 폴더"}]"#;
        let repos: Vec<RepositoryConfig> = serde_json::from_str(json).expect("parse");
        assert_eq!(
            repos,
            [RepositoryConfig {
                id: RepositoryId(7),
                name: String::from("프로젝트"),
                path: PathBuf::from("/tmp/작업 폴더"),
                common_dir: PathBuf::new(),
                enabled: true,
                remote_name: String::from("origin"),
                base_branch_override: None,
                custom_ref_namespaces: vec![String::from("refs/remote/")],
            }]
        );
    }
}
