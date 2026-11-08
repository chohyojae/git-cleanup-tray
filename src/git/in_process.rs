// SPDX-License-Identifier: GPL-3.0-only

//! 조회는 `gix`, 변경·네트워크는 기존 Git runner로 처리한다.

use std::ffi::OsStr;
use std::path::Path;

use gix::refs::file::ReferenceExt;

use crate::error::{CleanupError, Result};

use super::object::ObjectId;
use super::read::{CommandRead, GitReadRepository, TrackedBranch};
use super::refs::RefEntry;
use super::repository::RepositoryIdentity;
use super::runner::{GitOutput, GitRunner, ProcessGitRunner};
use super::worktree::WorktreeEntry;

/// 라이브러리 조회를 선택하는 runner. 저장소 객체는 보관하지 않아 스레드 사이에 공유되지 않는다.
#[derive(Debug, Clone)]
pub struct InProcessGitRunner<R = ProcessGitRunner> {
    /// 변경·네트워크·명시적 미지원 조회에 사용하는 기존 runner.
    process: R,
    /// Custom Git의 설치별 설정을 보존하거나 비교할 때 명령 조회를 그대로 사용한다.
    command_reads: bool,
}

impl Default for InProcessGitRunner {
    /// PATH의 Git을 변경 작업에 사용하는 runner를 만든다.
    fn default() -> Self {
        Self::new(ProcessGitRunner::new())
    }
}

impl<R> InProcessGitRunner<R> {
    /// Git 명령 실행 정책과 기록 기능을 그대로 유지하며 라이브러리 조회를 연결한다.
    #[must_use]
    pub fn new(process: R) -> Self {
        Self {
            process,
            command_reads: false,
        }
    }

    /// 같은 변경 runner를 유지하면서 조회도 Git 명령으로 처리한다.
    #[must_use]
    pub fn commands_only(process: R) -> Self {
        Self {
            process,
            command_reads: true,
        }
    }
}

impl<R: GitRunner> GitRunner for InProcessGitRunner<R> {
    /// 저장소를 현재 스레드에서 연다. 지원하지 않는 설정은 조회 전체를 Git 명령으로 처리한다.
    fn open_read<'a>(&'a self, path: &'a Path) -> Result<Box<dyn GitReadRepository + 'a>> {
        if self.command_reads || std::env::var_os("GIT_DIR").is_some() {
            return Ok(Box::new(CommandRead::new(&self.process, path)));
        }
        if !path.is_dir() {
            return Err(CleanupError::RepositoryNotAccessible(path.to_path_buf()));
        }
        let reader = GixRead::open(path)?;
        if let Some(reason) = reader.command_reason()? {
            log::debug!("using git commands for repository reads: {reason}");
            return Ok(Box::new(CommandRead::new(&self.process, path)));
        }
        Ok(Box::new(reader))
    }

    /// 모든 명령 실행은 기존 runner와 guard를 그대로 거친다.
    fn run(&self, path: &Path, args: &[&OsStr]) -> Result<GitOutput> {
        self.process.run(path, args)
    }
}

/// 한 번의 읽기 작업에서만 사용하는 저장소 객체. plan이나 UI 상태에 저장하지 않는다.
struct GixRead {
    /// 읽기 전용 인터페이스 밖으로 노출하지 않는 라이브러리 저장소.
    repo: gix::Repository,
}

impl GixRead {
    /// 신뢰·설정을 엄격히 확인하며 연다. Git 설치 경로 탐색용 보조 process도 실행하지 않는다.
    fn open(path: &Path) -> Result<Self> {
        let options = gix::open::Options::default()
            .strict_config(true)
            .bail_if_untrusted(true)
            .lossy_config(true);
        let mut repo =
            gix::discover_opts(path, gix::discover::upwards::Options::default(), options).map_err(
                |error| {
                    if error.is_not_found() {
                        CleanupError::NotARepository(path.to_path_buf())
                    } else {
                        read_error("open repository", error)
                    }
                },
            )?;
        // 사용자 설정의 큰 캐시 값을 따르지 않는다. 동시 저장소 수만큼 peak가 늘기 때문이다.
        repo.object_cache_size(Some(1024 * 1024));
        Ok(Self { repo })
    }

    /// Git과의 일치를 확인하지 못했거나 외부 helper가 필요한 설정만 명시적으로 되돌린다.
    fn command_reason(&self) -> Result<Option<&'static str>> {
        let config = self.repo.config_snapshot();
        if config
            .string("extensions.refStorage")
            .is_some_and(|value| value.as_slice() != b"files")
        {
            return Ok(Some("ref storage"));
        }
        for branch in self.repo.branch_names() {
            if config
                .plumbing()
                .strings_by("branch", Some(branch.as_bytes().into()), "merge")
                .is_some_and(|values| values.len() != 1 || !values[0].starts_with(b"refs/heads/"))
            {
                // Git과 라이브러리는 축약 이름·여러 merge 값의 해석이 달라질 수 있다.
                return Ok(Some("nonstandard branch merge configuration"));
            }
        }
        if config.plumbing().sections_by_name("filter").is_some() {
            return Ok(Some("external filter configuration"));
        }
        if config.string("core.fsmonitor").is_some() {
            return Ok(Some("filesystem monitor configuration"));
        }
        for key in ["core.sparseCheckout", "index.sparse"] {
            if config
                .try_boolean(key)
                .map_err(|error| read_error("read repository configuration", error))?
                .unwrap_or(false)
            {
                return Ok(Some("sparse checkout or index"));
            }
        }
        if config.string("extensions.partialClone").is_some() {
            return Ok(Some("partial clone"));
        }
        if self.repo.common_dir().join("info/grafts").try_exists()? {
            return Ok(Some("legacy grafts"));
        }
        Ok(None)
    }

    /// ref를 symbolic 여부와 실제 object 종류를 보존한 앱 타입으로 바꾼다.
    fn ref_entry(reference: &mut gix::Reference<'_>) -> Result<RefEntry> {
        let name = text(reference.name().as_bstr())?;
        let is_symbolic = matches!(reference.target(), gix::refs::TargetRef::Symbolic(_));
        // tag를 커밋까지 peel하면 tag ref를 삭제 후보로 오인할 수 있어 symbolic ref만 따라간다.
        let id = reference
            .follow_to_object()
            .map_err(|error| read_error("resolve reference", error))?;
        let is_commit = id
            .header()
            .map_err(|error| read_error("read object type", error))?
            .kind()
            == gix::object::Kind::Commit;
        Ok(RefEntry {
            name,
            target: domain_id(id.detach())?,
            is_commit,
            is_symbolic,
        })
    }

    /// 같은 커밋·tag 입력도 Git과 같은 조상 판정에 사용할 수 있도록 커밋 ID로 정규화한다.
    fn commit_id(&self, id: &ObjectId) -> Result<gix::hash::ObjectId> {
        let object = self
            .repo
            .find_object(native_id(id)?)
            .map_err(|error| read_error("read commit", error))?
            .peel_tags_to_end()
            .map_err(|error| read_error("peel commit", error))?;
        if object.kind != gix::object::Kind::Commit {
            return Err(read_error(
                "read commit",
                std::io::Error::new(std::io::ErrorKind::InvalidData, "object is not a commit"),
            ));
        }
        Ok(object.id)
    }
}

impl GitReadRepository for GixRead {
    /// 라이브러리가 확인한 경로를 절대 경로로 반환하며 bare 저장소는 거부한다.
    fn identity(&mut self) -> Result<RepositoryIdentity> {
        let Some(workdir) = self.repo.workdir() else {
            return Err(CleanupError::NotARepository(self.repo.path().to_path_buf()));
        };
        Ok(RepositoryIdentity {
            toplevel: self.repo.current_dir().join(workdir),
            git_dir: self.repo.current_dir().join(self.repo.git_dir()),
            common_dir: self.repo.current_dir().join(self.repo.common_dir()),
        })
    }

    /// symbolic ref 체인을 읽되 순환이나 과도한 깊이는 오류로 처리한다.
    fn symbolic_ref(&mut self, name: &str) -> Result<Option<String>> {
        let mut current = name.to_owned();
        let mut symbolic = false;
        for _ in 0..40 {
            let reference = self
                .repo
                .try_find_reference(current.as_str())
                .map_err(|error| read_error("read symbolic reference", error))?;
            let Some(reference) = reference else {
                return Ok(symbolic.then_some(current));
            };
            match reference.target() {
                gix::refs::TargetRef::Symbolic(target) => {
                    current = text(target.as_bstr())?;
                    symbolic = true;
                }
                gix::refs::TargetRef::Object(_) => return Ok(symbolic.then_some(current)),
            }
        }
        Err(read_error(
            "read symbolic reference",
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "symbolic reference cycle or excessive depth",
            ),
        ))
    }

    /// 필요한 ref만 읽고 같은 커밋 그래프를 재사용해 병합 여부를 판정한다.
    fn list_refs(
        &mut self,
        patterns: &[&str],
        merged_into: Option<&ObjectId>,
    ) -> Result<Vec<RefEntry>> {
        let platform = self
            .repo
            .references()
            .map_err(|error| read_error("list references", error))?;
        let iter = platform
            .all()
            .map_err(|error| read_error("list references", error))?;
        let cache = self
            .repo
            .commit_graph_if_enabled()
            .map_err(|error| read_error("read commit graph", error))?;
        let mut graph = self.repo.revision_graph(cache.as_ref());
        let base = merged_into.map(|id| self.commit_id(id)).transpose()?;
        let mut entries = Vec::new();
        for reference in iter {
            let mut reference = reference.map_err(|error| read_error("read reference", error))?;
            let name = reference.name().as_bstr();
            if !patterns
                .iter()
                .any(|pattern| matches_pattern(name, pattern))
            {
                continue;
            }
            let entry = Self::ref_entry(&mut reference)?;
            if let Some(base) = base {
                if !entry.is_commit {
                    continue;
                }
                let candidate = native_id(&entry.target)?;
                let merged = candidate == base
                    || self
                        .repo
                        .merge_base_with_graph(candidate, base, &mut graph)
                        .map_err(|error| read_error("check merged reference", error))?
                        .is_some_and(|id| id.detach() == candidate);
                if !merged {
                    continue;
                }
            }
            entries.push(entry);
        }
        entries.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    /// 존재하는 ref를 tag 끝까지 따라간 뒤 커밋인지 확인한다.
    fn resolve_commit(&mut self, name: &str) -> Result<Option<ObjectId>> {
        let Some(mut reference) = self
            .repo
            .try_find_reference(name)
            .map_err(|error| read_error("find commit reference", error))?
        else {
            return Ok(None);
        };
        let object = reference
            .follow_to_object()
            .map_err(|error| read_error("resolve commit reference", error))?
            .object()
            .map_err(|error| read_error("read commit object", error))?
            .peel_tags_to_end()
            .map_err(|error| read_error("peel commit reference", error))?;
        if object.kind == gix::object::Kind::Commit {
            domain_id(object.id).map(Some)
        } else {
            Ok(None)
        }
    }

    /// 같은 OID도 실제 커밋 존재를 확인한 뒤 조상으로 인정한다.
    fn is_ancestor(&mut self, ancestor: &ObjectId, descendant: &ObjectId) -> Result<bool> {
        let ancestor = self.commit_id(ancestor)?;
        let descendant = self.commit_id(descendant)?;
        Ok(ancestor == descendant
            || self
                .repo
                .merge_base(ancestor, descendant)
                .map_err(|error| read_error("check commit ancestry", error))?
                .is_some_and(|id| id.detach() == ancestor))
    }

    /// 메인·연결 워크트리를 조회하고, 잠금 파일을 읽지 못한 경우 잠금 없음으로 처리하지 않는다.
    fn worktrees(&mut self) -> Result<Vec<WorktreeEntry>> {
        let main = self
            .repo
            .main_repo()
            .map_err(|error| read_error("read main worktree", error))?;
        let mut entries = vec![worktree_entry(&main, true, None, None)?];
        for proxy in self
            .repo
            .worktrees()
            .map_err(|error| read_error("list worktrees", error))?
        {
            let locked = match std::fs::read(proxy.git_dir().join("locked")) {
                Ok(reason) => Some(String::from_utf8_lossy(&reason).trim_end().to_owned()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            let path = proxy
                .base()
                .map_err(|error| read_error("read worktree registration", error))?;
            let prunable =
                if locked.is_none() && (!path.try_exists()? || !path.join(".git").try_exists()?) {
                    Some(String::from("worktree path does not exist"))
                } else {
                    None
                };
            // 경로가 사라진 worktree도 branch 연결은 보존해야 일반 branch 후보로 오인하지 않는다.
            // checkout을 열어 신뢰 검사를 완화하지 않고, 검증된 공통 저장소의 등록 정보만 읽는다.
            let mut store = gix::refs::file::Store::for_linked_worktree(
                proxy.git_dir().to_path_buf(),
                self.repo.current_dir().join(self.repo.common_dir()),
                self.repo.object_hash(),
            );
            store.namespace = self.repo.namespace().cloned();
            let head = store
                .find("HEAD")
                .map_err(|error| read_error("read linked worktree HEAD", error))?;
            let (branch, head) = match head.target {
                gix::refs::Target::Object(id) => (None, Some(domain_id(id)?)),
                gix::refs::Target::Symbolic(name) => {
                    let branch = text(name.as_bstr())?;
                    let head = store
                        .try_find(&name)
                        .map_err(|error| read_error("find linked branch", error))?
                        .map(|mut reference| {
                            reference
                                .follow_to_object_packed(&store, None)
                                .map_err(|error| read_error("resolve linked branch", error))
                                .and_then(domain_id)
                        })
                        .transpose()?;
                    (Some(branch), head)
                }
            };
            entries.push(WorktreeEntry {
                path,
                head,
                branch,
                is_main: false,
                is_bare: false,
                locked,
                prunable,
            });
        }
        Ok(entries)
    }

    /// 전체 상태 iterator가 오류 없이 끝나야만 변경 없음으로 판단한다. 인덱스를 저장하지 않는다.
    fn is_clean(&mut self) -> Result<bool> {
        let repo = &self.repo;
        let dirwalk = repo
            .dirwalk_options()
            .map_err(|error| read_error("prepare untracked file walk", error))?;
        let status = repo
            .status(gix::progress::Discard)
            .map_err(|error| read_error("prepare worktree status", error))?
            .index_worktree_options_mut(|options| {
                options.thread_limit = Some(1);
                // `status.showUntrackedFiles=no`가 꺼 둔 순회도 앱의 명시적 검사에서는 복원한다.
                options.dirwalk_options = Some(dirwalk);
            })
            .untracked_files(gix::status::UntrackedFiles::Collapsed)
            .into_iter(std::iter::empty())
            .map_err(|error| read_error("read worktree status", error))?;
        let mut clean = true;
        for item in status {
            item.map_err(|error| read_error("read worktree status item", error))?;
            clean = false;
        }
        Ok(clean)
    }

    /// 라이브러리의 branch 설정 API로 upstream의 원격 이름과 ref를 읽는다.
    fn tracked_branches(&mut self) -> Result<Vec<TrackedBranch>> {
        let entries = self.list_refs(&[super::refs::LOCAL_BRANCH_PREFIX], None)?;
        let mut branches = Vec::with_capacity(entries.len());
        for entry in entries {
            if entry.is_symbolic || !entry.is_commit {
                continue;
            }
            let reference = self
                .repo
                .find_reference(entry.name.as_str())
                .map_err(|error| read_error("read branch upstream", error))?;
            let remote = reference
                .remote_name(gix::remote::Direction::Fetch)
                .map(|name| text(name.as_bstr()))
                .transpose()?
                .unwrap_or_default();
            let tracking = reference
                .remote_tracking_ref_name(gix::remote::Direction::Fetch)
                .transpose()
                .map_err(|error| read_error("read upstream tracking configuration", error))?;
            let source = reference
                .remote_ref_name(gix::remote::Direction::Fetch)
                .transpose()
                .map_err(|error| read_error("read branch upstream reference", error))?;
            // 사용자 fetch refspec이 로컬 브랜치를 가리켜도 앱이 방금 가져온 tracking ref만 사용한다.
            let upstream = source
                .as_ref()
                .filter(|_| tracking.is_some())
                .and_then(|source| source.as_bstr().strip_prefix(b"refs/heads/"))
                .map(|short| text(short).map(|short| format!("refs/remotes/{remote}/{short}")))
                .transpose()?
                .unwrap_or_default();
            branches.push(TrackedBranch {
                name: entry.name,
                commit: entry.target,
                remote: if upstream.is_empty() {
                    String::new()
                } else {
                    remote
                },
                upstream,
            });
        }
        Ok(branches)
    }
}

/// 워크트리의 HEAD와 경로를 읽기 전용 앱 타입으로 변환한다.
fn worktree_entry(
    repo: &gix::Repository,
    is_main: bool,
    locked: Option<String>,
    prunable: Option<String>,
) -> Result<WorktreeEntry> {
    let head = repo
        .head()
        .map_err(|error| read_error("read worktree HEAD", error))?;
    let path = repo
        .workdir()
        .map_or_else(|| repo.path().to_path_buf(), Path::to_path_buf);
    Ok(WorktreeEntry {
        path,
        head: head.id().map(|id| domain_id(id.detach())).transpose()?,
        branch: head
            .referent_name()
            .map(|name| text(name.as_bstr()))
            .transpose()?,
        is_main,
        is_bare: repo.is_bare(),
        locked,
        prunable,
    })
}

/// Git의 ref 이름 경계에 맞춰 비교해 비슷한 이름의 namespace를 섞지 않는다.
fn matches_pattern(name: &[u8], pattern: &str) -> bool {
    let pattern = pattern.as_bytes();
    name == pattern
        || name
            .strip_prefix(pattern)
            .is_some_and(|rest| pattern.ends_with(b"/") || rest.starts_with(b"/"))
}

/// 손실 변환으로 다른 ref를 가리키지 않도록 이름의 UTF-8 유효성을 확인한다.
fn text(bytes: &[u8]) -> Result<String> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|error| read_error("decode reference name", error))
}

/// 라이브러리가 검증한 object ID를 앱 타입으로 보관한다.
fn domain_id(id: gix::hash::ObjectId) -> Result<ObjectId> {
    ObjectId::parse(&id.to_string()).ok_or_else(|| {
        read_error(
            "convert object id",
            std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid object id"),
        )
    })
}

/// 앱의 검증된 object ID를 라이브러리 타입으로 변환한다.
fn native_id(id: &ObjectId) -> Result<gix::hash::ObjectId> {
    gix::hash::ObjectId::from_hex(id.as_str().as_bytes())
        .map_err(|error| read_error("parse object id", error))
}

/// 설정 값을 일반 로그에 노출하지 않고 원인을 타입 기반 오류에 보관한다.
fn read_error(
    operation: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> CleanupError {
    CleanupError::RepositoryReadFailed {
        operation,
        source: Box::new(source),
    }
}
