//! pkglab-core — reference implementation of the pkglab substrate.
//!
//! Provides:
//! - [`store::SqliteArtifactStore`]: the artifact metadata store (rusqlite),
//! - [`blob::FsBlobStore`]: the content-addressed blob store,
//! - [`upstreams`]: re-export of the shared upstream table (re-exported from
//!   common for convenience),
//! - [`auth::DefaultAuth`]: bcrypt Basic + HMAC-signed bearer tokens,
//! - [`system`]: the cross-protocol admin API routes,
//! - [`gc`]: reachability-driven blob reclamation.

pub mod auth;
pub mod blob;
pub mod gc;
pub mod store;
pub mod system;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::Arc;

/// A fully-assembled substrate instance bound to a data root:
/// `<root>/meta.sqlite`, `<root>/blobs/sha256`, `<root>/upstreams.json`.
///
/// Note: this is the *concrete* core registry (SQLite + filesystem CAS). The
/// trait-only composed substrate lives in `pkglab_common::Registry`.
pub struct Registry {
    pub blobs: Arc<dyn pkglab_common::BlobStore>,
    pub meta: Arc<dyn pkglab_common::ArtifactStore>,
    pub upstreams: pkglab_common::upstreams::Upstreams,
}

impl Registry {
    /// Open (or create) a registry rooted at `root`.
    pub fn open(root: &Path) -> pkglab_common::registry::Result<Arc<Registry>> {
        let blobs = Arc::new(blob::FsBlobStore::new(&root.join("blobs").join("sha256"))?);
        let meta = Arc::new(store::SqliteArtifactStore::open(&root.join("meta.sqlite"))?);
        let upstreams = pkglab_common::upstreams::Upstreams::new(Some(root.join("upstreams.json")));
        Ok(Arc::new(Registry { blobs, meta, upstreams }))
    }

    /// Reclaim unreferenced blobs; returns the number removed.
    pub async fn gc(&self) -> pkglab_common::registry::Result<u64> {
        gc::run(&self.meta, &self.blobs).await
    }
}
