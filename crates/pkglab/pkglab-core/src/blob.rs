//! Filesystem content-addressed blob store.
//!
//! Layout: `<root>/<algo>:<hex>` files plus a sibling `<digest>.hashes`
//! sidecar JSON caching md5/sha1/sha256/sha512 so protocol adapters can serve
//! checksum fields without recomputing.

use async_trait::async_trait;
use md5::Md5;
use pkglab_common::artifact::{compute_hashes, Hashes};
use pkglab_common::blob::{BlobReader, BlobStore};
use pkglab_common::registry::RegistryError;
use sha1::Sha1;
use sha2::{Digest as _, Sha256, Sha512};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct FsBlobStore {
    root: PathBuf,
}

impl FsBlobStore {
    pub fn new(root: &Path) -> pkglab_common::registry::Result<Self> {
        std::fs::create_dir_all(root).map_err(RegistryError::Io)?;
        Ok(Self { root: root.to_path_buf() })
    }

    fn path(&self, digest: &str) -> PathBuf {
        // Filesystem-safe name: ':' kept (valid on unix), '/' absent in digests.
        self.root.join(digest)
    }

    /// Open a blob's backing file directly (for streaming to an async sink).
    pub fn open_file(&self, digest: &str) -> std::io::Result<Option<std::fs::File>> {
        match std::fs::File::open(self.path(digest)) {
            Ok(f) => Ok(Some(f)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

struct FileReader(std::fs::File);

impl Read for FileReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl BlobReader for FileReader {
    fn seek_to(&mut self, pos: u64) -> std::io::Result<()> {
        use std::io::Seek;
        self.0.seek(std::io::SeekFrom::Start(pos))?;
        Ok(())
    }
}

#[async_trait]
impl BlobStore for FsBlobStore {
    async fn stat(&self, digest: &str) -> pkglab_common::blob::Result<Option<u64>> {
        let p = self.path(digest);
        tokio::task::block_in_place(|| match std::fs::metadata(&p) {
            Ok(m) => Ok(Some(m.len())),
            Err(_) => Ok(None),
        })
    }

    async fn open(&self, digest: &str) -> pkglab_common::blob::Result<Option<Box<dyn BlobReader>>> {
        let p = self.path(digest);
        match std::fs::File::open(&p) {
            Ok(f) => Ok(Some(Box::new(FileReader(f)))),
            Err(_) => Ok(None),
        }
    }

    async fn put_if_absent(
        &self,
        digest: &str,
        r: &mut (dyn Read + Send + Unpin),
    ) -> pkglab_common::blob::Result<bool> {
        let root = self.root.clone();
        let dst = self.path(digest);
        let digest = digest.to_string();

        // Parse the digest algorithm.
        let (algo, expected_hex) = parse_digest(&digest)?;

        // Fast path: already present.
        if dst.exists() {
            return Ok(false);
        }

        // Stream to a temp file while hashing.
        let tmp_path = root.join(format!(".upload-{}-{}", std::process::id(), rand_suffix()));
        let mut hasher = Sha256::new();
        let mut md5h = Md5::new();
        let mut sha1h = Sha1::new();
        let mut sha512h = Sha512::new();
        let mut file = tokio::fs::File::create(&tmp_path).await.map_err(RegistryError::Io)?;
        use tokio::io::AsyncWriteExt;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = r.read(&mut buf).map_err(RegistryError::Io)?;
            if n == 0 {
                break;
            }
            let chunk = &buf[..n];
            match algo.as_str() {
                "sha256" => hasher.update(chunk),
                _ => hasher.update(chunk),
            };
            md5h.update(chunk);
            sha1h.update(chunk);
            sha512h.update(chunk);
            file.write_all(chunk).await.map_err(RegistryError::Io)?;
        }
        file.flush().await.map_err(RegistryError::Io)?;
        drop(file);

        let got_hex = hex::encode(hasher.finalize());
        if !expected_hex.is_empty() && got_hex != expected_hex {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(RegistryError::DigestMismatch {
                expected: digest.clone(),
                got: format!("sha256:{got_hex}"),
            });
        }

        // Second existence check before rename (cheap dedup race guard).
        if dst.exists() {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Ok(false);
        }
        tokio::fs::rename(&tmp_path, &dst).await.map_err(RegistryError::Io)?;

        // Persist sidecar hashes.
        let hashes = Hashes {
            md5: hex::encode(md5h.finalize()),
            sha1: hex::encode(sha1h.finalize()),
            sha256: got_hex,
            sha512: hex::encode(sha512h.finalize()),
        };
        if let Ok(json) = serde_json::to_vec(&hashes) {
            let _ = tokio::fs::write(self.path(&format!("{digest}.hashes")), json).await;
        }
        Ok(true)
    }

    async fn hashes_for(&self, digest: &str) -> pkglab_common::blob::Result<Hashes> {
        let sidecar = self.path(&format!("{digest}.hashes"));
        if let Ok(json) = tokio::fs::read(&sidecar).await {
            if let Ok(h) = serde_json::from_slice::<Hashes>(&json) {
                return Ok(h);
            }
        }
        // Recompute from content.
        let p = self.path(digest);
        let data = tokio::fs::read(&p).await.map_err(|_| RegistryError::BlobUnknown)?;
        let (h, _) = compute_hashes(data.as_slice()).map_err(RegistryError::Io)?;
        Ok(h)
    }

    async fn delete(&self, digest: &str) -> pkglab_common::blob::Result<()> {
        let _ = tokio::fs::remove_file(self.path(&format!("{digest}.hashes"))).await;
        match tokio::fs::remove_file(self.path(digest)).await {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(RegistryError::Io(e)),
        }
    }

    async fn list(&self) -> pkglab_common::blob::Result<Vec<String>> {
        let root = self.root.clone();
        tokio::task::block_in_place(|| {
            let mut out = Vec::new();
            let entries = std::fs::read_dir(&root).map_err(RegistryError::Io)?;
            for e in entries {
                let e = e.map_err(RegistryError::Io)?;
                let name = e.file_name().to_string_lossy().to_string();
                if e.path().is_dir() || name.ends_with(".hashes") || name.starts_with('.') {
                    continue;
                }
                out.push(name);
            }
            Ok(out)
        })
    }
}

/// Split `algo:hex` (defaulting to sha256 when no prefix). Returns the
/// algorithm and the expected hex ("" when the caller did not pre-state it).
fn parse_digest(digest: &str) -> pkglab_common::blob::Result<(String, String)> {
    match digest.split_once(':') {
        Some((algo, hexpart)) => {
            if algo.is_empty() || hexpart.is_empty() {
                return Err(RegistryError::InvalidDigest(digest.to_string()));
            }
            Ok((algo.to_string(), hexpart.to_lowercase()))
        }
        None => Ok(("sha256".to_string(), digest.to_string())),
    }
}

fn rand_suffix() -> String {
    use rand::RngExt;
    let n: u64 = rand::rng().random();
    format!("{n:x}")
}

/// Helper used by tests and GC: compute sha256 of bytes.
pub fn blob_digest(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("sha256:{}", hex::encode(h.finalize()))
}

/// Shared handle constructor.
pub fn shared(root: &Path) -> pkglab_common::registry::Result<Arc<FsBlobStore>> {
    Ok(Arc::new(FsBlobStore::new(root)?))
}
