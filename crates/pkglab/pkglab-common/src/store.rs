//! Artifact metadata storage trait.
//!
//! Artifacts are keyed by `(format, repository, version)`; the same
//! `repository` may exist under several formats without collision.

use crate::artifact::Artifact;
use crate::blob::UploadRecord;
use crate::registry::{RegistryError, Result};
use async_trait::async_trait;

/// A repository + its versions, tagged with the owning protocol format. Used
/// by admin surfaces to enumerate everything stored.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PackageSummary {
    pub format: String,
    pub repository: String,
    pub versions: Vec<String>,
    pub source: String,
    /// Owning source repo (org/repo), empty for pull-through caches.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo: String,
    /// The workspace bookmark (branch) the artifact was published from.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bookmark: String,
    /// Commit sha provenance.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha: String,
}

/// Protocol-neutral artifact metadata storage.
#[async_trait]
pub trait ArtifactStore: Send + Sync + 'static {
    /// Store (or replace) an artifact and register its repository.
    async fn put(&self, a: Artifact) -> Result<()>;

    /// Fetch an artifact by format + repository + version.
    async fn get(&self, format: &str, repo: &str, version: &str) -> Result<Artifact>;

    /// Remove an artifact. Idempotent.
    async fn delete(&self, format: &str, repo: &str, version: &str) -> Result<()>;

    /// Remove every version of a repository across all formats, plus the
    /// repository index entry. Returns the number of versions removed.
    async fn delete_repo(&self, repo: &str) -> Result<u64>;

    /// All versions for a repository under a format, sorted ascending.
    async fn list_versions(&self, format: &str, repo: &str) -> Result<Vec<String>>;

    /// Repository names whose stored artifacts have the given format, sorted.
    async fn list_repositories_by_format(&self, format: &str) -> Result<Vec<String>>;

    /// All repository names, sorted ascending.
    async fn list_repositories(&self) -> Result<Vec<String>>;

    /// Every stored repository grouped by format+name with versions and
    /// origin (push vs pull), sorted by format then repository.
    async fn list_packages(&self) -> Result<Vec<PackageSummary>>;

    // -- upload session persistence (OCI chunked uploads) -------------------

    async fn save_upload(&self, u: UploadRecord) -> Result<()>;
    async fn get_upload(&self, id: &str) -> Result<UploadRecord>;
    async fn delete_upload(&self, id: &str) -> Result<()>;
    async fn list_uploads(&self) -> Result<Vec<String>>;

    // -- raw key/value metadata (per-adapter extras, e.g. swift git urls) ---

    async fn get_meta(&self, key: &str) -> Result<Option<Vec<u8>>>;
    async fn set_meta(&self, key: &str, value: &[u8]) -> Result<()>;
}

/// Map the conventional "unknown" sentinel into `RegistryError::ArtifactUnknown`.
pub fn unknown_if_absent<T>(opt: Option<T>) -> Result<T> {
    opt.ok_or(RegistryError::ArtifactUnknown)
}
