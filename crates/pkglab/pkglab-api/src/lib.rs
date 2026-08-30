//! pkglab-api — unified programmatic (non-HTTP) facade.
//!
//! Every pkglab protocol adapter exposes a thin HTTP `router()` today. This
//! crate adds the layer *under* that HTTP shell so an embedder (jjlab, a
//! worker, an internal service) can publish / fetch / list / delete artifacts
//! through one neutral surface without round-tripping its own HTTP port.
//!
//! Two levels:
//! - [`RegistryApi`] — neutral, protocol-agnostic facade derived from a
//!   [`pkglab_common::Registry`]. It speaks in `format + repository + version
//!   + bytes` and is the primary surface internal systems call.
//! - [`pkgs`] — the uniform [`pkgs::Client`] (one `publish`/`fetch`/`delete`
//!   shape for every format) plus per-format advanced helpers (`pkgs::oci`,
//!   `pkgs::conan`). Protocol differences live behind the [`pkgs::ProtocolHandler`]
//!   trait, mostly reusing each adapter's own functions.

pub mod types;

mod impls;
pub mod pkgs;

use pkglab_common::registry::Result;
use std::sync::Arc;

/// A neutral facade over the composed substrate. No protocol knowledge is
/// required of the caller beyond the `format` tag.
#[derive(Clone)]
pub struct RegistryApi {
    registry: Arc<pkglab_common::Registry>,
}

impl RegistryApi {
    pub fn new(registry: Arc<pkglab_common::Registry>) -> Self {
        Self { registry }
    }

    /// The underlying substrate, for advanced/protocol-specific calls.
    pub fn registry(&self) -> &Arc<pkglab_common::Registry> {
        &self.registry
    }

    /// Store (or replace) one artifact under a format.
    pub async fn put(&self, a: pkglab_common::Artifact) -> Result<()> {
        self.registry.meta.put(a).await
    }

    /// Fetch an artifact by format + repository + version.
    pub async fn get(
        &self,
        format: &str,
        repo: &str,
        version: &str,
    ) -> Result<pkglab_common::Artifact> {
        self.registry.meta.get(format, repo, version).await
    }

    /// Remove one artifact (idempotent). Caller-owned GC (reclaiming
    /// unreferenced blobs) is a substrate concern (`pkglab_core::Registry::gc`).
    pub async fn delete(&self, format: &str, repo: &str, version: &str) -> Result<()> {
        self.registry.meta.delete(format, repo, version).await
    }

    /// Remove every version of a repository across all formats.
    pub async fn delete_repo(&self, repo: &str) -> Result<u64> {
        self.registry.meta.delete_repo(repo).await
    }

    /// Versions of a repository under a format, sorted ascending.
    pub async fn versions(&self, format: &str, repo: &str) -> Result<Vec<String>> {
        self.registry.meta.list_versions(format, repo).await
    }

    /// Repository names under a format, sorted ascending.
    pub async fn repositories(&self, format: &str) -> Result<Vec<String>> {
        self.registry.meta.list_repositories_by_format(format).await
    }

    /// Every stored package grouped by format + name, with versions and origin.
    pub async fn packages(&self) -> Result<Vec<pkglab_common::PackageSummary>> {
        self.registry.meta.list_packages().await
    }

    /// Bytes of a stored blob by digest, if present.
    pub async fn blob_bytes(&self, digest: &str) -> Result<Vec<u8>> {
        let mut reader = self
            .registry
            .blobs
            .open(digest)
            .await?
            .ok_or(pkglab_common::RegistryError::BlobUnknown)?;
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut reader, &mut data)?;
        Ok(data)
    }
}

/// Build the uniform client for a format, sharing `api`.
pub fn client(api: RegistryApi, format: &str) -> pkgs::Client {
    pkgs::client(api, format)
}
