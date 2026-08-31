//! jj-lib 真相源封装：管理 org/repo 的 jj workspace 生命周期。
//!
//! 存储用 `init_internal_git`（纯 jj 存储 + 内部 git 对象 store，便于将来
//! export 阶段直接 `export_refs`）。这是纯 jj 语义的真相源；git 只是 backend。

use std::path::PathBuf;
use std::sync::Arc;

use jj_lib::default_backend_factories::{
    default_backend_factories, default_working_copy_factories,
};
use jj_lib::repo::ReadonlyRepo;
use tokio::sync::Mutex;

use crate::settings;

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("repository {org}/{repo} not found")]
    NotFound { org: String, repo: String },
    #[error("repository operation failed: {0}")]
    Other(String),
}

pub type RepoResult<T> = std::result::Result<T, RepoError>;

/// A handle to an opened repository. `ReadonlyRepo` is held inside an `Arc`.
pub struct RepoHandle {
    pub repo: Arc<ReadonlyRepo>,
}

/// Owns the filesystem directory layout `<root>/<org>/<repo>` and serializes
/// concurrent init/clone/delete of the same repo.
pub struct RepoStore {
    pubs_root: PathBuf,
    init_lock: Mutex<()>,
}

impl RepoStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            pubs_root: root,
            init_lock: Mutex::new(()),
        }
    }

    /// Validate org/repo as safe path segments before any fs use. This is the
    /// single traversal boundary: every directory access funnels here.
    fn checked_repo_dir(&self, org: &str, repo: &str) -> RepoResult<PathBuf> {
        jjlab_core::validate_segment(org, "org").map_err(RepoError::Other)?;
        jjlab_core::validate_segment(repo, "repo").map_err(RepoError::Other)?;
        Ok(self.pubs_root.join(org).join(repo))
    }

    /// Raw (unvalidated) path — only for internal callers that have already
    /// validated; do not expose this to request-derived names.
    pub fn repo_dir(&self, org: &str, repo: &str) -> PathBuf {
        self.pubs_root.join(org).join(repo)
    }

    /// Validated repo directory for callers outside this module.
    pub fn repo_dir_checked(&self, org: &str, repo: &str) -> RepoResult<PathBuf> {
        self.checked_repo_dir(org, repo)
    }

    pub fn exists(&self, org: &str, repo: &str) -> bool {
        self.checked_repo_dir(org, repo)
            .map(|d| d.join(".jj").exists())
            .unwrap_or(false)
    }

    pub async fn open_or_init(&self, org: &str, repo: &str) -> RepoResult<RepoHandle> {
        if !self.exists(org, repo) {
            // Scope the init lock so it is dropped before the awaited init.
            {
                let _guard = self.init_lock.lock().await;
                if !self.exists(org, repo) {
                    let dir = self.checked_repo_dir(org, repo)?;
                    self.init(&dir).await?;
                }
            }
        }
        self.open(org, repo).await
    }

    pub async fn open(&self, org: &str, repo: &str) -> RepoResult<RepoHandle> {
        // Serialize only the fallback init; the load path below is read-only.
        let settings = settings::user_settings()
            .map_err(RepoError::Other)?;
        let path = self.checked_repo_dir(org, repo)?;
        let store_factories = default_backend_factories();
        let working_copy_factories = default_working_copy_factories();
        match jj_lib::workspace::Workspace::load(
            &settings,
            &path,
            &store_factories,
            &working_copy_factories,
        ) {
            Ok(ws) => Ok(RepoHandle {
                repo: ws
                    .repo_loader()
                    .load_at_head()
                    .await
                    .map_err(|e| RepoError::Other(e.to_string()))?,
            }),
            Err(_) => {
                // A bare internal-git repo has `.jj/repo` but no workspace.
                let repo_dir = path.join(".jj/repo");
                let repo_loader = jj_lib::repo::RepoLoader::init_from_file_system(
                    &settings,
                    &repo_dir,
                    &store_factories,
                )
                .map_err(|e| RepoError::Other(e.to_string()))?;
                let repo = repo_loader
                    .load_at_head()
                    .await
                    .map_err(|e| RepoError::Other(e.to_string()))?;
                Ok(RepoHandle { repo })
            }
        }
    }

    async fn init(&self, dir: &std::path::Path) -> RepoResult<()> {
        std::fs::create_dir_all(dir).map_err(|e| RepoError::Other(e.to_string()))?;
        let settings =
            settings::user_settings().map_err(RepoError::Other)?;
        jj_lib::workspace::Workspace::init_internal_git(&settings, dir, gix::hash::Kind::Sha1)
            .await
            .map(|_| ())
            .map_err(|e| RepoError::Other(e.to_string()))
    }
}