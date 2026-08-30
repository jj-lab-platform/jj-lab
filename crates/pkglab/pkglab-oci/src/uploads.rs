//! Streaming blob upload sessions (OCI distribution spec).
//!
//! Sessions buffer into temp files, persist a record in the artifact store
//! (surviving restart), and commit into the blob store with digest
//! verification on the final PUT.

use crate::manifest::parse_digest;
use pkglab_common::blob::{BlobStore, UploadRecord};
use pkglab_common::registry::{RegistryError, Result};
use pkglab_common::store::ArtifactStore;
use rand::RngCore;
use sha2::Digest as _;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

pub struct UploadSession {
    pub id: String,
    pub name: String,
    tmp_path: PathBuf,
    file: tokio::sync::Mutex<tokio::fs::File>,
    size: std::sync::atomic::AtomicU64,
}

impl UploadSession {
    pub fn size(&self) -> u64 {
        self.size.load(std::sync::atomic::Ordering::SeqCst)
    }
}

type SessionMap = std::sync::Mutex<std::collections::HashMap<String, Arc<UploadSession>>>;

pub struct Uploads {
    sessions: SessionMap,
    store: Arc<dyn BlobStore>,
    meta: Arc<dyn ArtifactStore>,
    tmp_dir: PathBuf,
}

impl Uploads {
    pub fn new(store: Arc<dyn BlobStore>, meta: Arc<dyn ArtifactStore>, tmp_dir: PathBuf) -> Self {
        Self { sessions: SessionMap::new(std::collections::HashMap::new()), store, meta, tmp_dir }
    }

    fn new_id() -> String {
        let mut b = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut b);
        hex::encode(b)
    }

    /// Start a session for repository `name`.
    pub async fn start(self: &Arc<Self>, name: &str) -> Result<Arc<UploadSession>> {
        tokio::fs::create_dir_all(&self.tmp_dir).await.map_err(RegistryError::Io)?;
        let id = Self::new_id();
        let tmp_path = self.tmp_dir.join(format!("oci-upload-{id}"));
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&tmp_path)
            .await
            .map_err(RegistryError::Io)?;
        let sess = Arc::new(UploadSession {
            id: id.clone(),
            name: name.to_string(),
            tmp_path,
            file: tokio::sync::Mutex::new(file),
            size: std::sync::atomic::AtomicU64::new(0),
        });
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), sess.clone());
        let _ = self
            .meta
            .save_upload(UploadRecord {
                session_id: id.clone(),
                repo: name.to_string(),
                tmp_path: sess.tmp_path.display().to_string(),
                size: 0,
            })
            .await;
        Ok(sess)
    }

    /// Restore persisted sessions after a restart. Broken records are dropped.
    pub async fn restore(self: &Arc<Self>) {
        let Ok(ids) = self.meta.list_uploads().await else {
            return;
        };
        for id in ids {
            let Ok(rec) = self.meta.get_upload(&id).await else {
                continue;
            };
            let Ok(f) = tokio::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .append(false)
                .open(&rec.tmp_path)
                .await
            else {
                let _ = self.meta.delete_upload(&id).await;
                continue;
            };
            let sess = Arc::new(UploadSession {
                id: id.clone(),
                name: rec.repo.clone(),
                tmp_path: PathBuf::from(&rec.tmp_path),
                file: tokio::sync::Mutex::new(f),
                size: std::sync::atomic::AtomicU64::new(rec.size.max(0) as u64),
            });
            self.sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(id, sess);
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<UploadSession>> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
    }

    /// Append bytes; when `expect_start` is set the offset must match the
    /// current size (out-of-order chunk => 416 semantics via error).
    pub async fn patch(
        &self,
        id: &str,
        expect_start: Option<u64>,
        body: impl tokio::io::AsyncRead + Unpin,
    ) -> Result<u64> {
        let Some(sess) = self.get(id) else {
            return Err(RegistryError::UploadUnknown);
        };
        {
            let cur = sess.size();
            if expect_start.is_some() && expect_start != Some(cur) {
                return Err(RegistryError::Other("out of order chunk".into()));
            }
            let mut f = sess.file.lock().await;
            let mut r = body;
            let n = tokio::io::copy(&mut r, &mut *f).await.map_err(RegistryError::Io)?;
            f.flush().await.map_err(RegistryError::Io)?;
            sess.size.fetch_add(n, std::sync::atomic::Ordering::SeqCst);
            let _ = self
                .meta
                .save_upload(UploadRecord {
                    session_id: sess.id.clone(),
                    repo: sess.name.clone(),
                    tmp_path: sess.tmp_path.display().to_string(),
                    size: sess.size() as i64,
                })
                .await;
        }
        Ok(sess.size())
    }

    /// Finalize: hash the buffered bytes, verify against `digest`, store into
    /// the CAS, and drop the session.
    pub async fn commit(&self, id: &str, digest: &str) -> Result<()> {
        let Some(sess) = self.get(id) else {
            return Err(RegistryError::UploadUnknown);
        };
        let expected_hex = parse_digest(digest)?;
        let (hash_hex, bytes) = {
            let mut f = sess.file.lock().await;
            f.seek(std::io::SeekFrom::Start(0)).await.map_err(RegistryError::Io)?;
            let cur_size = sess.size();
            let mut hasher = match digest.split_once(':').map(|x| x.0) {
                Some("sha512") => Hasher::Sha512(sha2::Sha512::new()),
                _ => Hasher::Sha256(sha2::Sha256::new()),
            };
            let mut data = Vec::with_capacity(cur_size as usize);
            {
                use tokio::io::AsyncReadExt;
                let reader = &mut *f;
                let mut buf = [0u8; 64 * 1024];
                loop {
                    let n = reader.read(&mut buf).await.map_err(RegistryError::Io)?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                    data.extend_from_slice(&buf[..n]);
                }
            }
            (hasher.finalize(), data)
        };
        if hash_hex != expected_hex {
            self.cancel(id).await;
            return Err(RegistryError::DigestMismatch {
                expected: digest.to_string(),
                got: format!("sha256:{hash_hex}"),
            });
        }
        let mut cursor = std::io::Cursor::new(bytes);
        self.store.put_if_absent(digest, &mut cursor).await?;
        // Drop the session record.
        {
            self.sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
        }
        let _ = self.meta.delete_upload(id).await;
        let _ = tokio::fs::remove_file(&sess.tmp_path).await;
        Ok(())
    }

    /// Abort and remove a session. Idempotent.
    pub async fn cancel(&self, id: &str) {
        let sess = {
            let mut map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            map.remove(id)
        };
        if let Some(sess) = sess {
            let _ = tokio::fs::remove_file(&sess.tmp_path).await;
        }
        let _ = self.meta.delete_upload(id).await;
    }
}

enum Hasher {
    Sha256(sha2::Sha256),
    Sha512(sha2::Sha512),
}

impl Hasher {
    fn update(&mut self, chunk: &[u8]) {
        match self {
            Hasher::Sha256(h) => h.update(chunk),
            Hasher::Sha512(h) => h.update(chunk),
        }
    }
    fn finalize(self) -> String {
        match self {
            Hasher::Sha256(h) => hex::encode(h.finalize()),
            Hasher::Sha512(h) => hex::encode(h.finalize()),
        }
    }
}

impl UploadSession {
    pub async fn read_all(&self) -> Result<Vec<u8>> {
        let mut f = self.file.lock().await;
        f.seek(std::io::SeekFrom::Start(0)).await.map_err(RegistryError::Io)?;
        let mut data = Vec::with_capacity(self.size() as usize);
        {
            use tokio::io::AsyncReadExt;
            let reader = &mut *f;
            let mut buf = [0u8; 64 * 1024];
            loop {
                let n = reader.read(&mut buf).await.map_err(RegistryError::Io)?;
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n]);
            }
        }
        Ok(data)
    }
}
