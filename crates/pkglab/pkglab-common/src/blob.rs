//! Blob storage trait (content-addressed, deduplicating) and the persisted
//! OCI upload-session record type.

use crate::artifact::Hashes;
use async_trait::async_trait;
use std::io::Read;

/// A reader over stored blob bytes. Positional reads (Range requests) are
/// supported via [`BlobReader::seek_to`].
pub trait BlobReader: Read + Send {
    fn seek_to(&mut self, pos: u64) -> std::io::Result<()>;
}

/// Content-addressed blob storage. Blobs are deduplicated across all protocols
/// by digest — the shared substrate of a multi-format registry.
#[async_trait]
pub trait BlobStore: Send + Sync + 'static {
    /// Size of the blob, or `None` when absent.
    async fn stat(&self, digest: &str) -> Result<Option<u64>>;

    /// A reader positioned at the start of the blob, or `None` when absent.
    async fn open(&self, digest: &str) -> Result<Option<Box<dyn BlobReader>>>;

    /// Stream `r` into the store, verifying the content matches `digest`.
    /// Returns `true` when newly stored, `false` when the blob already
    /// existed (dedup). Errors on digest mismatch.
    async fn put_if_absent(&self, digest: &str, r: &mut (dyn Read + Send + Unpin)) -> Result<bool>;

    /// The persisted MD5/SHA1/SHA256/SHA512 hashes for a blob. Implementations
    /// may recompute from content when no cached sidecar exists.
    async fn hashes_for(&self, digest: &str) -> Result<Hashes>;

    /// Remove a blob if present. Idempotent.
    async fn delete(&self, digest: &str) -> Result<()>;

    /// Digest (hex or `algo:hex`) of every blob in the store. The caller owns
    /// reachability/GC policy.
    async fn list(&self) -> Result<Vec<String>>;
}

/// Result alias used across the substrate traits.
pub type Result<T> = std::result::Result<T, crate::registry::RegistryError>;

/// A persisted in-progress blob upload session (OCI chunked uploads survive a
/// process restart through this record).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct UploadRecord {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub tmp_path: String,
    #[serde(default)]
    pub size: i64,
}
