//! The composed substrate handed to every protocol adapter: an artifact
//! store, a blob store and the upstream table, plus the generic pull-through
//! flows built on top.

use crate::artifact::{compute_hashes, Hashes};
use crate::blob::BlobStore;
use crate::registry::{RegistryError, Result};
use crate::remote::Remote;
use crate::store::ArtifactStore;
use crate::upstreams::Upstreams;
use async_trait::async_trait;
use std::io::Read;
use std::sync::Arc;

/// Composed substrate. Every adapter holds an `Arc<Registry>` and only sees
/// the traits, so an embedder can wire any implementation.
pub struct Registry {
    pub blobs: Arc<dyn BlobStore>,
    pub meta: Arc<dyn ArtifactStore>,
    pub upstreams: Upstreams,
}

impl Registry {
    pub fn new(
        blobs: Arc<dyn BlobStore>,
        meta: Arc<dyn ArtifactStore>,
        upstreams: Upstreams,
    ) -> Self {
        Self { blobs, meta, upstreams }
    }

    /// Effective upstream base for a format (override, then built-in default).
    pub fn upstream(&self, format: &str) -> Option<String> {
        self.upstreams.get(format)
    }

    /// Effective sub-endpoint URL (`cargo.static`, `nuget.search`, ...).
    pub fn upstream_sub(&self, format: &str, sub: &str) -> Option<String> {
        self.upstreams.sub(format, sub)
    }

    /// A [`Remote`] for a format's primary upstream honoring its proxy policy.
    pub fn remote(&self, format: &str, base: Option<&str>) -> Option<Remote> {
        let base = base.map(|s| s.to_string()).or_else(|| self.upstream(format))?;
        let proxy = self.upstreams.proxy_url(format);
        Some(Remote::new(&self.upstreams.factory(), &base, proxy.as_deref()))
    }

    /// A [`Remote`] for a sub-endpoint honoring its proxy policy.
    pub fn remote_sub(&self, format: &str, sub: &str) -> Option<Remote> {
        let base = self.upstream_sub(format, sub)?;
        let proxy = self.upstreams.proxy_url(&format!("{format}.{sub}"));
        Some(Remote::new(&self.upstreams.factory(), &base, proxy.as_deref()))
    }

    /// A [`Remote`] against an arbitrary absolute base with a format's proxy
    /// policy.
    pub fn remote_at(&self, format: &str, base: &str) -> Remote {
        let proxy = self.upstreams.proxy_url(format);
        Remote::new(&self.upstreams.factory(), base, proxy.as_deref())
    }

    /// Fetch content from an absolute URL, store it (dedup by sha256), and
    /// return bytes + hashes. The URL is used verbatim.
    pub async fn fetch_absolute(&self, url: &str) -> Result<Fetched> {
        // Proxy policy: use a bare "generic" key when the format is unknown.
        let proxy = self.upstreams.proxy_url("generic");
        let remote = Remote::new(&self.upstreams.factory(), "", proxy.as_deref());
        let resp = remote.get(url).await.map_err(|e| RegistryError::Http(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(RegistryError::UpstreamStatus {
                path: url.to_string(),
                status: status.as_u16(),
            });
        }
        let data = resp.bytes().await.map_err(|e| RegistryError::Http(e.to_string()))?.to_vec();
        self.finish_fetch(data).await
    }

    /// Fetch `path` from a format's upstream, store it (dedup by sha256), and
    /// return bytes + hashes.
    pub async fn fetch(&self, format: &str, upstream_base: &str, path: &str) -> Result<Fetched> {
        let remote = self
            .remote(format, None)
            .ok_or_else(|| RegistryError::NoUpstream(format.to_string()))?;
        // An empty upstream_base means "use the format's configured/default
        // upstream" (Remote already resolves it); non-empty bases are used
        // verbatim below via remote_at.
        let remote =
            if upstream_base.is_empty() { remote } else { self.remote_at(format, upstream_base) };
        let resp = remote.get(path).await.map_err(|e| RegistryError::Http(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(RegistryError::UpstreamStatus {
                path: path.to_string(),
                status: status.as_u16(),
            });
        }
        let data = resp.bytes().await.map_err(|e| RegistryError::Http(e.to_string()))?.to_vec();
        self.finish_fetch(data).await
    }

    /// Store freshly fetched bytes and build the [`Fetched`] summary.
    async fn finish_fetch(&self, data: Vec<u8>) -> Result<Fetched> {
        let (stored, data) = self.store_and_keep(data).await?;
        Ok(Fetched { hashes: stored.hashes, size: stored.size as i64, data })
    }

    /// Store bytes and return both the stored summary and the original bytes.
    async fn store_and_keep(&self, data: Vec<u8>) -> Result<(Stored, Vec<u8>)> {
        let (hashes, size) = compute_hashes(data.as_slice()).map_err(RegistryError::Io)?;
        let digest = format!("sha256:{}", hashes.sha256);
        let mut r: Box<dyn Read + Send + Unpin> = Box::new(std::io::Cursor::new(data.clone()));
        self.blobs.put_if_absent(&digest, r.as_mut()).await?;
        Ok((Stored { hashes, size, digest }, data))
    }

    /// Store bytes in the CAS (dedup by sha256) and return their hashes.
    pub async fn store_and_hash(&self, data: Vec<u8>) -> Result<Stored> {
        let (hashes, size) = compute_hashes(data.as_slice()).map_err(RegistryError::Io)?;
        let digest = format!("sha256:{}", hashes.sha256);
        let mut r: Box<dyn Read + Send + Unpin> = Box::new(std::io::Cursor::new(data));
        self.blobs.put_if_absent(&digest, r.as_mut()).await?;
        Ok(Stored { hashes, size, digest })
    }
}

/// Result of a pull-through fetch: full upstream content (cached or fresh)
/// plus hashes.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub data: Vec<u8>,
    pub hashes: Hashes,
    pub size: i64,
}

/// Stored-content summary.
#[derive(Debug, Clone)]
pub struct Stored {
    pub hashes: Hashes,
    pub size: u64,
    pub digest: String,
}

/// Trait object convenience so adapters can hold `Arc<dyn RegistryApi>` if
/// they prefer dynamic dispatch over the concrete [`Registry`].
#[async_trait]
pub trait RegistryApi: Send + Sync + 'static {
    async fn fetch(&self, format: &str, upstream_base: &str, path: &str) -> Result<Fetched>;
    async fn fetch_absolute(&self, url: &str) -> Result<Fetched>;
    async fn store_and_hash(&self, data: Vec<u8>) -> Result<Stored>;
}

#[async_trait]
impl RegistryApi for Registry {
    async fn fetch(&self, format: &str, upstream_base: &str, path: &str) -> Result<Fetched> {
        Registry::fetch(self, format, upstream_base, path).await
    }
    async fn fetch_absolute(&self, url: &str) -> Result<Fetched> {
        Registry::fetch_absolute(self, url).await
    }
    async fn store_and_hash(&self, data: Vec<u8>) -> Result<Stored> {
        Registry::store_and_hash(self, data).await
    }
}
