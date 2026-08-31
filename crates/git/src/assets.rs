//! Release assets (M5-A): content-addressed blob storage. Sha256-addressed,
//! deduplicating, four-hash sidecars (mirrors pkglab's artifact model so a
//! future swap to pkglab's BlobStore trait is drop-in).

use std::io::Read;
use std::path::PathBuf;

use sha2::Digest as _;

/// Content-addressed FS blob store: `<root>/sha256/<hex>`.
pub struct AssetStore {
    root: PathBuf,
}

impl AssetStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path_of(&self, digest: &str) -> PathBuf {
        self.root.join("sha256").join(digest)
    }

    /// Store bytes; returns the sha256 hex digest. Dedup: existing blob kept.
    pub fn put(&self, data: &[u8]) -> std::io::Result<String> {
        let digest = sha256_hex(data);
        let dir = self.root.join("sha256");
        std::fs::create_dir_all(&dir)?;
        let path = self.path_of(&digest);
        if !path.exists() {
            // Write temp then rename for atomicity.
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, data)?;
            std::fs::rename(&tmp, &path)?;
        }
        Ok(digest)
    }

    /// Read a blob by digest, streaming from disk.
    pub fn open(&self, digest: &str) -> std::io::Result<Option<std::fs::File>> {
        let path = self.path_of(digest);
        if !path.exists() {
            return Ok(None);
        }
        std::fs::File::open(path).map(Some)
    }

    pub fn exists(&self, digest: &str) -> bool {
        self.path_of(digest).exists()
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = sha2::Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

/// Compute sha256 of a stream (multipart upload path uses buffered bytes).
pub fn sha256_of_reader<R: Read>(mut r: R) -> std::io::Result<(String, u64)> {
    let mut h = sha2::Sha256::new();
    let mut n: u64 = 0;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = r.read(&mut buf)?;
        if read == 0 {
            break;
        }
        h.update(&buf[..read]);
        n += read as u64;
    }
    Ok((hex::encode(h.finalize()), n))
}

/// Validate a digest string is a plain sha256 hex (no `algo:` prefix yet).
pub fn valid_digest(d: &str) -> bool {
    d.len() == 64 && d.chars().all(|c| c.is_ascii_hexdigit())
}

/// Location helper for where the asset store lives per deployment.
pub fn asset_root_from_env() -> PathBuf {
    PathBuf::from(std::env::var("JJLAB_ASSETS").unwrap_or_else(|_| "/data/assets".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_is_content_addressed_and_dedups() {
        let dir = tempfile::tempdir().unwrap();
        let store = AssetStore::new(dir.path());
        let d1 = store.put(b"hello").unwrap();
        let d2 = store.put(b"hello").unwrap();
        assert_eq!(d1, d2);
        assert_eq!(d1.len(), 64);
        // Only one blob file exists on disk.
        let blobs: Vec<_> = std::fs::read_dir(dir.path().join("sha256")).unwrap().collect();
        assert_eq!(blobs.len(), 1);
    }

    #[test]
    fn different_content_differs() {
        let dir = tempfile::tempdir().unwrap();
        let store = AssetStore::new(dir.path());
        let d1 = store.put(b"a").unwrap();
        let d2 = store.put(b"b").unwrap();
        assert_ne!(d1, d2);
    }

    #[test]
    fn open_returns_none_for_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = AssetStore::new(dir.path());
        assert!(store.open(&"0".repeat(64)).unwrap().is_none());
        assert!(!store.exists(&"0".repeat(64)));
    }

    #[test]
    fn roundtrip_preserves_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let store = AssetStore::new(dir.path());
        let data = vec![7u8; 100_000];
        let d = store.put(&data).unwrap();
        let mut f = store.open(&d).unwrap().unwrap();
        let mut back = Vec::new();
        f.read_to_end(&mut back).unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn valid_digest_rejects_short_or_prefixed() {
        assert!(valid_digest(&"a".repeat(64)));
        assert!(!valid_digest("sha256:0123"));
        assert!(!valid_digest("abc"));
    }
}
