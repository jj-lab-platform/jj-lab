//! SQLite-backed [`ArtifactStore`] (rusqlite bundled, WAL, busy timeout).
//!
//! Schema mirrors the shape proven by the original implementation:
//! artifacts keyed `(format, repository, version)` storing the serialized
//! artifact JSON, plus repo index, upload sessions and a raw meta KV table.

use async_trait::async_trait;
use pkglab_common::registry::RegistryError;
use pkglab_common::store::PackageSummary;
use pkglab_common::{Artifact, UploadRecord};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::params;
use std::path::Path;

type Pool = r2d2::Pool<SqliteConnectionManager>;

pub struct SqliteArtifactStore {
    pool: Pool,
}

const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS artifacts (
        format     TEXT NOT NULL,
        repository TEXT NOT NULL,
        version    TEXT NOT NULL,
        data       BLOB NOT NULL,
        PRIMARY KEY (format, repository, version)
    )",
    "CREATE INDEX IF NOT EXISTS idx_artifacts_repo ON artifacts (repository)",
    "CREATE TABLE IF NOT EXISTS repos (
        repository TEXT PRIMARY KEY
    )",
    "CREATE TABLE IF NOT EXISTS uploads (
        session_id TEXT PRIMARY KEY,
        data       BLOB NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS meta (
        key   TEXT PRIMARY KEY,
        value BLOB
    )",
];

impl SqliteArtifactStore {
    pub fn open(path: &Path) -> pkglab_common::registry::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(RegistryError::Io)?;
        }
        let manager = SqliteConnectionManager::file(path).with_init(|c| {
            c.execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=NORMAL;
                 PRAGMA busy_timeout=5000;",
            )
        });
        let pool = r2d2::Pool::builder()
            .max_size(8)
            .build(manager)
            .map_err(|e| RegistryError::Db(format!("sqlite pool: {e}")))?;
        {
            let conn = pool
                .get()
                .map_err(|e| RegistryError::Db(format!("sqlite get: {e}")))?;
            for stmt in SCHEMA {
                conn.execute(stmt, [])
                    .map_err(|e| RegistryError::Db(format!("schema: {e}")))?;
            }
        }
        Ok(Self { pool })
    }

    fn conn(
        &self,
    ) -> std::result::Result<r2d2::PooledConnection<SqliteConnectionManager>, RegistryError> {
        self.pool.get().map_err(|e| RegistryError::Db(e.to_string()))
    }
}

fn encode(a: &Artifact) -> std::result::Result<Vec<u8>, RegistryError> {
    serde_json::to_vec(a).map_err(|e| RegistryError::Db(e.to_string()))
}

fn decode(data: &[u8]) -> std::result::Result<Artifact, RegistryError> {
    serde_json::from_slice(data).map_err(|e| RegistryError::Db(e.to_string()))
}

#[async_trait]
impl pkglab_common::ArtifactStore for SqliteArtifactStore {
    async fn put(&self, a: Artifact) -> pkglab_common::registry::Result<()> {
        let data = encode(&a)?;
        // Normalize an empty format to "oci" (parity with the reference
        // implementation: OCI predates the format field).
        let mut a = a;
        let format = if a.format.is_empty() {
            a.format = "oci".into();
            "oci".to_string()
        } else {
            a.format.clone()
        };
        let (repo, version) = (a.repository.clone(), a.version.clone());
        self.conn()?
            .execute("INSERT INTO repos (repository) VALUES (?1) ON CONFLICT DO NOTHING", [&repo])
            .map_err(db)?;
        self.conn()?
            .execute(
                "INSERT INTO artifacts (format, repository, version, data) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (format, repository, version) DO UPDATE SET data = excluded.data",
                params![format, repo, version, data],
            )
            .map_err(db)?;
        Ok(())
    }

    async fn get(
        &self,
        format: &str,
        repo: &str,
        version: &str,
    ) -> pkglab_common::registry::Result<Artifact> {
        let conn = self.conn()?;
        let res: std::result::Result<Vec<u8>, _> = conn.query_row(
            "SELECT data FROM artifacts WHERE format = ?1 AND repository = ?2 AND version = ?3",
            params![format, repo, version],
            |r| r.get(0),
        );
        match res {
            Ok(data) => decode(&data),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(RegistryError::ArtifactUnknown),
            Err(e) => Err(db(e)),
        }
    }

    async fn delete(
        &self,
        format: &str,
        repo: &str,
        version: &str,
    ) -> pkglab_common::registry::Result<()> {
        self.conn()?
            .execute(
                "DELETE FROM artifacts WHERE format = ?1 AND repository = ?2 AND version = ?3",
                params![format, repo, version],
            )
            .map_err(db)?;
        Ok(())
    }

    async fn delete_repo(&self, repo: &str) -> pkglab_common::registry::Result<u64> {
        let conn = self.conn()?;
        let n = conn.execute("DELETE FROM artifacts WHERE repository = ?1", [repo]).map_err(db)?;
        conn.execute("DELETE FROM repos WHERE repository = ?1", [repo]).map_err(db)?;
        Ok(n as u64)
    }

    async fn list_versions(
        &self,
        format: &str,
        repo: &str,
    ) -> pkglab_common::registry::Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT version FROM artifacts WHERE format = ?1 AND repository = ?2 ORDER BY version",
            )
            .map_err(db)?;
        let rows = stmt.query_map(params![format, repo], |r| r.get::<_, String>(0)).map_err(db)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(db)?);
        }
        Ok(out)
    }

    async fn list_repositories_by_format(
        &self,
        format: &str,
    ) -> pkglab_common::registry::Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT repository FROM artifacts
                 WHERE format = ?1 OR (format = 'oci' AND ?1 = '')
                 ORDER BY repository",
            )
            .map_err(db)?;
        let rows = stmt.query_map([format], |r| r.get::<_, String>(0)).map_err(db)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(db)?);
        }
        Ok(out)
    }

    async fn list_repositories(&self) -> pkglab_common::registry::Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT repository FROM repos ORDER BY repository").map_err(db)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(db)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(db)?);
        }
        Ok(out)
    }

    async fn list_packages(&self) -> pkglab_common::registry::Result<Vec<PackageSummary>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT format, repository, version, data FROM artifacts").map_err(db)?;        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Vec<u8>>(3)?,
                ))
            })
            .map_err(db)?;

        use std::collections::BTreeMap;
        struct Acc {
            versions: std::collections::BTreeSet<String>,
            source: String,
            repo: String,
            bookmark: String,
            sha: String,
        }
        let mut by_repo: BTreeMap<(String, String), Acc> = BTreeMap::new();
        for row in rows {
            let (f, repo, ver, data) = row.map_err(db)?;
            if repo.is_empty() || ver.is_empty() {
                continue;
            }
            let a: Artifact = match decode(&data) {
                Ok(a) => a,
                Err(_) => continue,
            };
            let format = if a.format.is_empty() { f } else { a.format };
            let format = if format.is_empty() { "oci".to_string() } else { format };
            let entry = by_repo
                .entry((format, repo))
                .or_insert_with(|| Acc {
                    versions: Default::default(),
                    source: a.source.clone(),
                    repo: a.repo.clone(),
                    bookmark: a.bookmark.clone(),
                    sha: a.sha.clone(),
                });
            entry.versions.insert(ver);
            if a.source == "pull" {
                entry.source = "pull".into();
            } else if a.source == "push" && entry.source.is_empty() {
                entry.source = "push".into();
            }
        }

        let mut out: Vec<PackageSummary> = by_repo
            .into_iter()
            .map(|((format, repo), acc)| {
                let source = if acc.source.is_empty() { "push".to_string() } else { acc.source };
                PackageSummary {
                    format,
                    repository: repo,
                    versions: acc.versions.into_iter().collect(),
                    source,
                    repo: acc.repo.clone(),
                    bookmark: acc.bookmark.clone(),
                    sha: acc.sha.clone(),
                }
            })
            .collect();
        out.sort_by(|a, b| (&a.format, &a.repository).cmp(&(&b.format, &b.repository)));
        Ok(out)
    }

    async fn list_oci_images(
        &self,
        repo: &str,
        source: &str,
    ) -> pkglab_common::registry::Result<Vec<PackageSummary>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT format, repository, version, data FROM artifacts WHERE format = 'oci'")
            .map_err(db)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Vec<u8>>(3)?,
                ))
            })
            .map_err(db)?;

        use std::collections::BTreeMap;
        struct Acc {
            versions: std::collections::BTreeSet<String>,
            source: String,
            repo: String,
            bookmark: String,
            sha: String,
        }
        let mut by_repo: BTreeMap<(String, String), Acc> = BTreeMap::new();
        for row in rows {
            let (f, repo, ver, data) = row.map_err(db)?;
            if repo.is_empty() || ver.is_empty() {
                continue;
            }
            let a: Artifact = match decode(&data) {
                Ok(a) => a,
                Err(_) => continue,
            };
            let format = if a.format.is_empty() { f } else { a.format };
            let format = if format.is_empty() { "oci".to_string() } else { format };
            let entry = by_repo
                .entry((format, repo))
                .or_insert_with(|| Acc {
                    versions: Default::default(),
                    source: a.source.clone(),
                    repo: a.repo.clone(),
                    bookmark: a.bookmark.clone(),
                    sha: a.sha.clone(),
                });
            entry.versions.insert(ver);
            if a.source == "pull" {
                entry.source = "pull".into();
            } else if a.source == "push" && entry.source.is_empty() {
                entry.source = "push".into();
            }
        }

        let mut out: Vec<PackageSummary> = by_repo
            .into_iter()
            .filter(|((_format, _repo), acc)| {
                // repo filter: empty = all; else match the source repo.
                let repo_match = repo.is_empty()
                    || acc.repo == repo
                    || (repo.starts_with("library/") && acc.repo.is_empty());
                let source_match = source.is_empty() || acc.source == source;
                repo_match && source_match
            })
            .map(|((format, repo), acc)| {
                let source = if acc.source.is_empty() { "push".to_string() } else { acc.source };
                PackageSummary {
                    format,
                    repository: repo,
                    versions: acc.versions.into_iter().collect(),
                    source,
                    repo: acc.repo.clone(),
                    bookmark: acc.bookmark.clone(),
                    sha: acc.sha.clone(),
                }
            })
            .collect();
        out.sort_by(|a, b| a.repository.cmp(&b.repository));
        Ok(out)
    }

    async fn list_artifacts(
        &self,
        format: &str,
        repo: &str,
    ) -> pkglab_common::registry::Result<Vec<Artifact>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT data FROM artifacts WHERE format = ?1 AND repository = ?2 ORDER BY version")
            .map_err(db)?;
        let rows = stmt.query_map(params![format, repo], |r| r.get::<_, Vec<u8>>(0)).map_err(db)?;
        let mut out = Vec::new();
        for r in rows {
            let data = r.map_err(db)?;
            match decode(&data) {
                Ok(a) => out.push(a),
                Err(_) => {}
            }
        }
        Ok(out)
    }

    async fn save_upload(&self, u: UploadRecord) -> pkglab_common::registry::Result<()> {
        let data = serde_json::to_vec(&u).map_err(|e| RegistryError::Db(e.to_string()))?;
        self.conn()?
            .execute(
                "INSERT INTO uploads (session_id, data) VALUES (?1, ?2)
                 ON CONFLICT (session_id) DO UPDATE SET data = excluded.data",
                params![u.session_id, data],
            )
            .map_err(db)?;
        Ok(())
    }

    async fn get_upload(&self, id: &str) -> pkglab_common::registry::Result<UploadRecord> {
        let conn = self.conn()?;
        let res: std::result::Result<Vec<u8>, _> =
            conn.query_row("SELECT data FROM uploads WHERE session_id = ?1", [id], |r| r.get(0));
        match res {
            Ok(data) => serde_json::from_slice(&data).map_err(|e| RegistryError::Db(e.to_string())),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(RegistryError::UploadUnknown),
            Err(e) => Err(db(e)),
        }
    }

    async fn delete_upload(&self, id: &str) -> pkglab_common::registry::Result<()> {
        self.conn()?.execute("DELETE FROM uploads WHERE session_id = ?1", [id]).map_err(db)?;
        Ok(())
    }

    async fn list_uploads(&self) -> pkglab_common::registry::Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT session_id FROM uploads").map_err(db)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(db)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(db)?);
        }
        Ok(out)
    }

    async fn get_meta(&self, key: &str) -> pkglab_common::registry::Result<Option<Vec<u8>>> {
        let conn = self.conn()?;
        let res: std::result::Result<Vec<u8>, _> =
            conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0));
        match res {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(db(e)),
        }
    }

    async fn set_meta(&self, key: &str, value: &[u8]) -> pkglab_common::registry::Result<()> {
        self.conn()?
            .execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(db)?;
        Ok(())
    }
}

fn db(e: rusqlite::Error) -> RegistryError {
    RegistryError::Db(e.to_string())
}
