//! SQLite metadata store: orgs, repos, changes, bookmarks, change-id anchor
//! map, conflicts, and op-log. jj objects (tree/blob) live on local FS via
//! jj-lib; this store holds only the relational metadata and the git-sha →
//! change-id mapping that keeps change-ids stable across force-push/amend.

use std::sync::Arc;

use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;

use crate::{Error, Result};

#[derive(Clone)]
pub struct Db {
    pool: Arc<r2d2::Pool<SqliteConnectionManager>>,
}

#[derive(Debug, Clone)]
pub struct RepoRow {
    pub id: String,
    pub org_id: String,
    pub name: String,
    pub default_bookmark: String,
    pub git_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ChangeRow {
    pub change_id: String,
    pub repo_id: String,
    pub description: String,
    pub author: String,
    pub committer: String,
    pub git_commit_sha: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AnchorRow {
    pub change_id: String,
    pub commit_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ConflictRow {
    pub id: String,
    pub repo_id: String,
    pub change_id: String,
    pub path: String,
    pub adds: String,
    pub removes: String,
}

#[derive(Debug, Clone)]
pub struct ReleaseRow {
    pub id: i64,
    pub repo_id: String,
    pub tag_name: String,
    pub name: String,
    pub body: String,
    pub draft: bool,
    pub prerelease: bool,
}

#[derive(Debug, Clone)]
pub struct ReleaseAssetRow {
    pub id: i64,
    pub release_id: i64,
    pub name: String,
    pub size: i64,
    pub digest: String,
    pub content_type: String,
}

#[derive(Debug, Clone)]
pub struct WorkflowRow {
    pub id: i64,
    pub repo_id: String,
    pub name: String,
    pub path: String,
    pub trigger: String,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct RunRow {
    pub id: i64,
    pub repo_id: String,
    pub workflow_id: i64,
    pub trigger_ref: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct JobRow {
    pub id: i64,
    pub run_id: i64,
    pub name: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub log_path: Option<String>,
    pub image: Option<String>,
    pub run: Option<String>,
    pub timeout_seconds: Option<i64>,
    pub build_spec: Option<String>,
}

impl JobRow {
    /// The shell command this job executes in its pod (empty when absent).
    pub fn command(&self) -> &str {
        self.run.as_deref().unwrap_or("")
    }
}

#[derive(Debug, Clone)]
pub struct MrRow {
    pub id: i64,
    pub repo_id: String,
    pub number: i64,
    pub title: String,
    pub description: String,
    pub author: String,
    pub state: String,
    pub head_change_id: String,
    pub head_sha: Option<String>,
    pub base_rev: String,
}

#[derive(Debug, Clone)]
pub struct MrReviewRow {
    pub id: i64,
    pub mr_id: i64,
    pub reviewer: String,
    pub state: String,
    pub body: String,
    pub commit_sha: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MrCommentRow {
    pub id: i64,
    pub mr_id: i64,
    pub author: String,
    pub body: String,
    pub path: Option<String>,
    pub commit_sha: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BookmarkRow {
    pub name: String,
    pub change_id: String,
    pub is_remote: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameOutcome {
    Ok,
    NotFound,
    Conflict,
}

impl Db {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| Error::Db(format!("db mkdir: {e}")))?;
        }
        let manager = SqliteConnectionManager::file(path).with_init(|c| {
            c.pragma_update(None, "journal_mode", "WAL")?;
            c.pragma_update(None, "foreign_keys", "ON")?;
            c.pragma_update(None, "busy_timeout", 5000)?;
            Ok(())
        });
        let pool = r2d2::Pool::builder()
            .max_size(8)
            .build(manager)
            .map_err(|e| Error::Db(format!("db pool: {e}")))?;
        {
            let conn = pool
                .get()
                .map_err(|e| Error::Db(format!("db get: {e}")))?;
            conn.execute_batch(SCHEMA)
                .map_err(|e| Error::Db(format!("db schema: {e}")))?;
        }
        Ok(Self { pool: Arc::new(pool) })
    }

    /// Run a synchronous DB closure on the blocking thread pool so a SQLite
    /// write/lock wait never stalls the tokio reactor. Handlers call this to
    /// get `Result<T>` back on their own async path.
    pub async fn run<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Self) -> Result<T> + Send + 'static,
    {
        let this = self.clone();
        tokio::task::spawn_blocking(move || f(&this))
            .await
            .map_err(|e| Error::Join(e.to_string()))?
    }

    pub fn upsert_org(&self, org_id: &str, name: &str) -> Result<()> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        conn.execute(
            "INSERT INTO orgs (id, name) VALUES (?1, ?2)
             ON CONFLICT (id) DO UPDATE SET name = excluded.name",
            rusqlite::params![org_id, name],
        )
        .map_err(|e| Error::Db(format!("upsert org: {e}")))?;
        Ok(())
    }

    pub fn upsert_repo(
        &self,
        id: &str,
        org_id: &str,
        name: &str,
        default_bookmark: &str,
        git_url: Option<&str>,
    ) -> Result<()> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        conn.execute(
            "INSERT INTO repos (id, org_id, name, default_bookmark, git_url)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET name = excluded.name,
               default_bookmark = excluded.default_bookmark,
               git_url = excluded.git_url",
            rusqlite::params![id, org_id, name, default_bookmark, git_url],
        )
        .map_err(|e| Error::Db(format!("upsert repo: {e}")))?;
        Ok(())
    }

    pub fn get_repo(&self, id: &str) -> Result<Option<RepoRow>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, org_id, name, default_bookmark, git_url
                 FROM repos WHERE id = ?1",
            )
            .map_err(|e| Error::Db(format!("get repo prepare: {e}")))?;
        let mut rows = stmt
            .query_map([id], |r| {
                Ok(RepoRow {
                    id: r.get(0)?,
                    org_id: r.get(1)?,
                    name: r.get(2)?,
                    default_bookmark: r.get(3)?,
                    git_url: r.get(4)?,
                })
            })
            .map_err(|e| Error::Db(format!("get repo query: {e}")))?;
        match rows.next() {
            Some(Ok(v)) => Ok(Some(v)),
            Some(Err(e)) => Err(Error::Db(format!("get repo row: {e}"))),
            None => Ok(None),
        }
    }

    /// Delete a repo and all its dependent metadata rows (cascade by hand for
    /// tables whose FK doesn't define ON DELETE CASCADE). Also removes the org
    /// row when it has no more repos.
    pub fn delete_repo(&self, id: &str) -> Result<()> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let org_id: String = match conn
            .query_row(
                "SELECT org_id FROM repos WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| Error::Db(format!("get org for repo: {e}")))?
        {
            Some(org) => org,
            None => return Ok(()),
        };

        // Tables keyed by repo_id (FK REFERENCES repos(id) ON UPDATE CASCADE without cascade), and
        // `changes` last because bookmarks/conflicts/change_parents reference it.
        // `change_parents` references `changes` without cascade, so clear it
        // before `changes` via the change ids of this repo.
        {
            let change_ids: Vec<String> = {
                let mut stmt = conn
                    .prepare("SELECT change_id FROM changes WHERE repo_id = ?1")
                    .map_err(|e| Error::Db(format!("list changes prepare: {e}")))?;
                let rows = stmt
                    .query_map(rusqlite::params![id], |r| r.get::<_, String>(0))
                    .map_err(|e| Error::Db(format!("list changes query: {e}")))?;
                rows.collect::<rusqlite::Result<Vec<String>>>()
                    .map_err(|e| Error::Db(format!("list changes rows: {e}")))?
            };
            for cid in &change_ids {
                conn.execute(
                    "DELETE FROM change_parents WHERE change_id = ?1 OR parent_change_id = ?1",
                    rusqlite::params![cid],
                )
                .map_err(|e| Error::Db(format!("delete change_parents: {e}")))?;
            }
        }

        // Order matters (FKs with foreign_keys=ON): dependents before parents.
        for (table, label) in [
            ("bookmarks", "bookmarks"),
            ("conflicts", "conflicts"),
            ("change_id_map", "change_id_map"),
            ("runs", "runs"),
            ("workflows", "workflows"),
            ("changes", "changes"),
        ] {
            conn.execute(
                &format!("DELETE FROM {table} WHERE repo_id = ?1"),
                rusqlite::params![id],
            )
            .map_err(|e| Error::Db(format!("delete {label}: {e}")))?;
        }

        // MRs reference merge_requests; reviews/comments cascade via FK,
        // releases/release_assets cascade, but merge_requests has no cascade.
        let mr_ids: Vec<i64> = {
            let mut stmt = conn
                .prepare("SELECT id FROM merge_requests WHERE repo_id = ?1")
                .map_err(|e| Error::Db(format!("list mrs prepare: {e}")))?;
            let rows = stmt
                .query_map(rusqlite::params![id], |r| r.get::<_, i64>(0))
                .map_err(|e| Error::Db(format!("list mrs query: {e}")))?;
            rows.collect::<rusqlite::Result<Vec<i64>>>()
                .map_err(|e| Error::Db(format!("list mrs rows: {e}")))?
        };
        for mr_id in mr_ids {
            conn.execute(
                "DELETE FROM merge_requests WHERE id = ?1",
                rusqlite::params![mr_id],
            )
            .map_err(|e| Error::Db(format!("delete mr: {e}")))?;
        }
        conn.execute(
            "DELETE FROM releases WHERE repo_id = ?1",
            rusqlite::params![id],
        )
        .map_err(|e| Error::Db(format!("delete releases: {e}")))?;

        conn.execute("DELETE FROM repos WHERE id = ?1", rusqlite::params![id])
            .map_err(|e| Error::Db(format!("delete repo: {e}")))?;

        // Drop the org if it has no remaining repos.
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM repos WHERE org_id = ?1",
                rusqlite::params![org_id],
                |r| r.get(0),
            )
            .map_err(|e| Error::Db(format!("count org repos: {e}")))?;
        if remaining == 0 {
            conn.execute("DELETE FROM orgs WHERE id = ?1", rusqlite::params![org_id])
                .map_err(|e| Error::Db(format!("delete org: {e}")))?;
        }
        Ok(())
    }

    /// Rename a repo, cascading `repo_id` updates to all dependent tables via
    /// the `ON UPDATE CASCADE` foreign keys. Returns the collision/not-found
    /// status so the caller can map to 409/404.
    pub fn rename_repo(&self, old_id: &str, new_id: &str) -> Result<RenameOutcome> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM repos WHERE id = ?1)",
                rusqlite::params![old_id],
                |r| r.get(0),
            )
            .map_err(|e| Error::Db(format!("rename repo exists: {e}")))?;
        if !exists {
            return Ok(RenameOutcome::NotFound);
        }
        let clobber: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM repos WHERE id = ?1)",
                rusqlite::params![new_id],
                |r| r.get(0),
            )
            .map_err(|e| Error::Db(format!("rename repo clobber: {e}")))?;
        if clobber {
            return Ok(RenameOutcome::Conflict);
        }
        let new_name = new_id.rsplit('/').next().unwrap_or(new_id);
        conn.execute(
            "UPDATE repos SET id = ?1, name = ?2 WHERE id = ?3",
            rusqlite::params![new_id, new_name, old_id],
        )
        .map_err(|e| Error::Db(format!("rename repo: {e}")))?;
        Ok(RenameOutcome::Ok)
    }

    /// All orgs as `(id, name)` pairs, ordered by name.
    pub fn list_orgs(&self) -> Result<Vec<(String, String)>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT id, name FROM orgs ORDER BY name")
            .map_err(|e| Error::Db(format!("list orgs prepare: {e}")))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| Error::Db(format!("list orgs query: {e}")))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| Error::Db(format!("list orgs row: {e}")))
    }

    /// All repos (across orgs) ordered by id.
    pub fn list_repos(&self) -> Result<Vec<RepoRow>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, org_id, name, default_bookmark, git_url
                 FROM repos ORDER BY id",
            )
            .map_err(|e| Error::Db(format!("list repos prepare: {e}")))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(RepoRow {
                    id: r.get(0)?,
                    org_id: r.get(1)?,
                    name: r.get(2)?,
                    default_bookmark: r.get(3)?,
                    git_url: r.get(4)?,
                })
            })
            .map_err(|e| Error::Db(format!("list repos query: {e}")))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| Error::Db(format!("list repos row: {e}")))
    }

    pub fn upsert_change(&self, r: &ChangeRow) -> Result<()> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        conn.execute(
            "INSERT INTO changes (change_id, repo_id, description, author, committer,
               git_commit_sha)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (change_id) DO UPDATE SET description = excluded.description,
               git_commit_sha = excluded.git_commit_sha",
            rusqlite::params![
                r.change_id,
                r.repo_id,
                r.description,
                r.author,
                r.committer,
                r.git_commit_sha,
            ],
        )
        .map_err(|e| Error::Db(format!("upsert change: {e}")))?;
        Ok(())
    }

    pub fn get_change(&self, change_id: &str) -> Result<Option<ChangeRow>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let mut stmt = conn
            .prepare(
                "SELECT change_id, repo_id, description, author, committer, git_commit_sha
                 FROM changes WHERE change_id = ?1",
            )
            .map_err(|e| Error::Db(format!("get change prepare: {e}")))?;
        let mut rows = stmt
            .query_map([change_id], |r| {
                Ok(ChangeRow {
                    change_id: r.get(0)?,
                    repo_id: r.get(1)?,
                    description: r.get(2)?,
                    author: r.get(3)?,
                    committer: r.get(4)?,
                    git_commit_sha: r.get(5)?,
                })
            })
            .map_err(|e| Error::Db(format!("get change query: {e}")))?;
        match rows.next() {
            Some(Ok(v)) => Ok(Some(v)),
            Some(Err(e)) => Err(Error::Db(format!("get change row: {e}"))),
            None => Ok(None),
        }
    }

    pub fn upsert_bookmark(
        &self,
        repo_id: &str,
        name: &str,
        change_id: &str,
        is_remote: bool,
    ) -> Result<()> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        conn.execute(
            "INSERT INTO bookmarks (repo_id, name, change_id, is_remote)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (repo_id, name) DO UPDATE SET change_id = excluded.change_id,
               is_remote = excluded.is_remote",
            rusqlite::params![repo_id, name, change_id, is_remote],
        )
        .map_err(|e| Error::Db(format!("upsert bookmark: {e}")))?;
        Ok(())
    }

    pub fn get_bookmark(&self, repo_id: &str, name: &str) -> Result<Option<String>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT change_id FROM bookmarks WHERE repo_id = ?1 AND name = ?2")
            .map_err(|e| Error::Db(format!("get bookmark prepare: {e}")))?;
        let mut rows = stmt
            .query_map(rusqlite::params![repo_id, name], |r| r.get::<_, String>(0))
            .map_err(|e| Error::Db(format!("get bookmark query: {e}")))?;
        match rows.next() {
            Some(Ok(v)) => Ok(Some(v)),
            Some(Err(e)) => Err(Error::Db(format!("get bookmark row: {e}"))),
            None => Ok(None),
        }
    }

    pub fn list_bookmarks(&self, repo_id: &str) -> Result<Vec<BookmarkRow>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT name, change_id, is_remote FROM bookmarks WHERE repo_id = ?1")
            .map_err(|e| Error::Db(format!("list bookmarks prepare: {e}")))?;
        let rows = stmt
            .query_map([repo_id], |r| {
                Ok(BookmarkRow {
                    name: r.get(0)?,
                    change_id: r.get(1)?,
                    is_remote: r.get::<_, bool>(2)?,
                })
            })
            .map_err(|e| Error::Db(format!("list bookmarks query: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Db(format!("list bookmarks row: {e}")))?);
        }
        Ok(out)
    }

    pub fn list_conflicts(&self, repo_id: &str) -> Result<Vec<ConflictRow>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, repo_id, change_id, path, adds, removes
                 FROM conflicts WHERE repo_id = ?1 ORDER BY path",
            )
            .map_err(|e| Error::Db(format!("list conflicts prepare: {e}")))?;
        let rows = stmt
            .query_map([repo_id], |r| {
                Ok(ConflictRow {
                    id: r.get(0)?,
                    repo_id: r.get(1)?,
                    change_id: r.get(2)?,
                    path: r.get(3)?,
                    adds: r.get(4)?,
                    removes: r.get(5)?,
                })
            })
            .map_err(|e| Error::Db(format!("list conflicts query: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Db(format!("list conflicts row: {e}")))?);
        }
        Ok(out)
    }

    /// Persist a git-sha → (change-id, commit-id) anchor. Returns true if the
    /// mapping was newly created.
    pub fn set_anchor(
        &self,
        repo_id: &str,
        git_commit_sha: &str,
        change_id: &str,
        commit_id: &str,
    ) -> Result<()> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        conn.execute(
            "INSERT INTO change_id_map (repo_id, git_commit_sha, change_id, commit_id)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (repo_id, git_commit_sha) DO UPDATE SET
               change_id = excluded.change_id, commit_id = excluded.commit_id",
            rusqlite::params![repo_id, git_commit_sha, change_id, commit_id],
        )
        .map_err(|e| Error::Db(format!("set anchor: {e}")))?;
        Ok(())
    }

    /// Look up a git-sha anchor. Returns `None` if not yet anchored.
    pub fn lookup_anchor(&self, repo_id: &str, git_commit_sha: &str) -> Result<Option<AnchorRow>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let mut stmt = conn
            .prepare(
                "SELECT change_id, commit_id FROM change_id_map
                 WHERE repo_id = ?1 AND git_commit_sha = ?2",
            )
            .map_err(|e| Error::Db(format!("lookup anchor prepare: {e}")))?;
        let mut rows = stmt
            .query_map(rusqlite::params![repo_id, git_commit_sha], |r| {
                Ok(AnchorRow {
                    change_id: r.get(0)?,
                    commit_id: r.get(1)?,
                })
            })
            .map_err(|e| Error::Db(format!("lookup anchor query: {e}")))?;
        match rows.next() {
            Some(Ok(v)) => Ok(Some(v)),
            Some(Err(e)) => Err(Error::Db(format!("lookup anchor row: {e}"))),
            None => Ok(None),
        }
    }

    pub fn upsert_conflict(
        &self,
        id: &str,
        repo_id: &str,
        change_id: &str,
        path: &str,
        adds: &str,
        removes: &str,
    ) -> Result<()> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        conn.execute(
            "INSERT INTO conflicts (id, repo_id, change_id, path, adds, removes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id) DO UPDATE SET adds = excluded.adds, removes = excluded.removes",
            rusqlite::params![id, repo_id, change_id, path, adds, removes],
        )
        .map_err(|e| Error::Db(format!("upsert conflict: {e}")))?;
        Ok(())
    }

    // ── merge requests (M3) ──

/// Create an MR, auto-assigning the next number within the repo.
#[allow(clippy::too_many_arguments)]
pub fn create_mr(
    &self,
    repo_id: &str,
    title: &str,
    description: &str,
    author: &str,
    head_change_id: &str,
    head_sha: Option<&str>,
    head_branch: Option<&str>,
    base_rev: &str,
) -> Result<MrRow> {
    let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
    let number: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(number), 0) + 1 FROM merge_requests WHERE repo_id = ?1",
            [repo_id],
            |r| r.get(0),
        )
        .map_err(|e| Error::Db(format!("create mr: {e}")))?;
    conn.execute(
        "INSERT INTO merge_requests
           (repo_id, number, title, description, author, head_change_id, head_sha, head_branch, base_rev)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            repo_id,
            number,
            title,
            description,
            author,
            head_change_id,
            head_sha,
            head_branch,
            base_rev
        ],
    )
    .map_err(|e| Error::Db(format!("create mr: {e}")))?;
    let id = conn.last_insert_rowid();
    Self::get_mr_from(&conn, id)
}

fn get_mr_from(conn: &Connection, id: i64) -> Result<MrRow> {
    conn.query_row(
        "SELECT id, repo_id, number, title, description, author, state,
                head_change_id, head_sha, base_rev
         FROM merge_requests WHERE id = ?1",
        [id],
        |r| {
            Ok(MrRow {
                id: r.get(0)?,
                repo_id: r.get(1)?,
                number: r.get(2)?,
                title: r.get(3)?,
                description: r.get(4)?,
                author: r.get(5)?,
                state: r.get(6)?,
                head_change_id: r.get(7)?,
                head_sha: r.get(8)?,
                base_rev: r.get(9)?,
            })
        },
    )
    .map_err(|e| Error::Db(format!("get mr: {e}")))
}

pub fn list_mrs(&self, repo_id: &str, state: Option<&str>) -> Result<Vec<MrRow>> {
    let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
    let (sql, number_filter): (&str, Option<&str>) = match state {
        Some(st) => (
            "SELECT id FROM merge_requests WHERE repo_id = ?1 AND state = ?2 ORDER BY number",
            Some(st),
        ),
        None => (
            "SELECT id FROM merge_requests WHERE repo_id = ?1 ORDER BY number",
            None,
        ),
    };
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| Error::Db(format!("list mrs: {e}")))?;
    let ids: Vec<i64> = if let Some(st) = number_filter {
        let rows = stmt
            .query_map(rusqlite::params![repo_id, st], |r| r.get(0))
            .map_err(|e| Error::Db(format!("list mrs: {e}")))?;
        rows.filter_map(|r| r.ok()).collect()
    } else {
        let rows = stmt
            .query_map([repo_id], |r| r.get(0))
            .map_err(|e| Error::Db(format!("list mrs: {e}")))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    ids.into_iter().map(|id| Self::get_mr_from(&conn, id)).collect()
}

pub fn get_mr_by_number(&self, repo_id: &str, number: i64) -> Result<Option<MrRow>> {
    let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM merge_requests WHERE repo_id = ?1 AND number = ?2",
            rusqlite::params![repo_id, number],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| Error::Db(format!("get mr by number: {e}")))?;
    match id {
        Some(id) => Ok(Some(Self::get_mr_from(&conn, id)?)),
        None => Ok(None),
    }
}

/// Update MR fields (head moves on force-push; state on close/merge/reopen).
pub fn update_mr(
    &self,
    id: i64,
    state: Option<&str>,
    head_change_id: Option<&str>,
    head_sha: Option<&str>,
) -> Result<MrRow> {
    let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
    if let Some(st) = state {
        conn.execute(
            "UPDATE merge_requests SET state = ?2, updated_at = strftime('%Y-%m-%d %H:%M:%S','now') WHERE id = ?1",
            rusqlite::params![id, st],
        )
        .map_err(|e| Error::Db(format!("update mr state: {e}")))?;
    }
    if let Some(cid) = head_change_id {
        conn.execute(
            "UPDATE merge_requests SET head_change_id = ?2, updated_at = strftime('%Y-%m-%d %H:%M:%S','now') WHERE id = ?1",
            rusqlite::params![id, cid],
        )
        .map_err(|e| Error::Db(format!("update mr head cid: {e}")))?;
    }
    if let Some(sha) = head_sha {
        conn.execute(
            "UPDATE merge_requests SET head_sha = ?2, updated_at = strftime('%Y-%m-%d %H:%M:%S','now') WHERE id = ?1",
            rusqlite::params![id, sha],
        )
        .map_err(|e| Error::Db(format!("update mr head sha: {e}")))?;
    }
    Self::get_mr_from(&conn, id)
}

pub fn add_mr_review(
    &self,
    mr_id: i64,
    reviewer: &str,
    state: &str,
    body: &str,
    commit_sha: Option<&str>,
) -> Result<MrReviewRow> {
    let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
    conn.execute(
        "INSERT INTO mr_reviews (mr_id, reviewer, state, body, commit_sha) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![mr_id, reviewer, state, body, commit_sha],
    )
    .map_err(|e| Error::Db(format!("add mr review: {e}")))?;
    let id = conn.last_insert_rowid();
    conn.query_row(
        "SELECT id, mr_id, reviewer, state, body, commit_sha FROM mr_reviews WHERE id = ?1",
        [id],
        |r| {
            Ok(MrReviewRow {
                id: r.get(0)?,
                mr_id: r.get(1)?,
                reviewer: r.get(2)?,
                state: r.get(3)?,
                body: r.get(4)?,
                commit_sha: r.get(5)?,
            })
        },
    )
    .map_err(|e| Error::Db(format!("get mr review: {e}")))
}

pub fn list_mr_reviews(&self, mr_id: i64) -> Result<Vec<MrReviewRow>> {
    let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
    let mut stmt = conn
        .prepare("SELECT id, mr_id, reviewer, state, body, commit_sha FROM mr_reviews WHERE mr_id = ?1 ORDER BY id")
        .map_err(|e| Error::Db(format!("list mr reviews: {e}")))?;
    let rows = stmt
        .query_map([mr_id], |r| {
            Ok(MrReviewRow {
                id: r.get(0)?,
                mr_id: r.get(1)?,
                reviewer: r.get(2)?,
                state: r.get(3)?,
                body: r.get(4)?,
                commit_sha: r.get(5)?,
            })
        })
        .map_err(|e| Error::Db(format!("list mr reviews: {e}")))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| Error::Db(format!("list mr reviews: {e}")))?);
    }
    Ok(out)
}

pub fn add_mr_comment(
    &self,
    mr_id: i64,
    author: &str,
    body: &str,
    path: Option<&str>,
    commit_sha: Option<&str>,
) -> Result<MrCommentRow> {
    let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
    conn.execute(
        "INSERT INTO mr_comments (mr_id, author, body, path, commit_sha) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![mr_id, author, body, path, commit_sha],
    )
    .map_err(|e| Error::Db(format!("add mr comment: {e}")))?;
    let id = conn.last_insert_rowid();
    conn.query_row(
        "SELECT id, mr_id, author, body, path, commit_sha FROM mr_comments WHERE id = ?1",
        [id],
        |r| {
            Ok(MrCommentRow {
                id: r.get(0)?,
                mr_id: r.get(1)?,
                author: r.get(2)?,
                body: r.get(3)?,
                path: r.get(4)?,
                commit_sha: r.get(5)?,
            })
        },
    )
    .map_err(|e| Error::Db(format!("get mr comment: {e}")))
}

pub fn list_mr_comments(&self, mr_id: i64) -> Result<Vec<MrCommentRow>> {
    let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
    let mut stmt = conn
        .prepare("SELECT id, mr_id, author, body, path, commit_sha FROM mr_comments WHERE mr_id = ?1 ORDER BY id")
        .map_err(|e| Error::Db(format!("list mr comments: {e}")))?;
    let rows = stmt
        .query_map([mr_id], |r| {
            Ok(MrCommentRow {
                id: r.get(0)?,
                mr_id: r.get(1)?,
                author: r.get(2)?,
                body: r.get(3)?,
                path: r.get(4)?,
                commit_sha: r.get(5)?,
            })
        })
        .map_err(|e| Error::Db(format!("list mr comments: {e}")))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| Error::Db(format!("list mr comments: {e}")))?);
    }
    Ok(out)
}

/// Open MRs as (id, head_change_id, head_branch) for force-push re-association.
pub fn list_open_mrs_for_reassoc(&self) -> Result<Vec<(i64, String, Option<String>)>> {
    let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
    let mut stmt = conn
        .prepare("SELECT id, head_change_id, head_branch FROM merge_requests WHERE state = 'open'")
        .map_err(|e| Error::Db(format!("list open mrs: {e}")))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| Error::Db(format!("list open mrs: {e}")))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| Error::Db(format!("list open mrs: {e}")))?);
    }
    Ok(out)
}

/// Aggregated review decision: approve > request_changes > pending.
pub fn mr_review_state(&self, mr_id: i64) -> Result<String> {
    let reviews = self.list_mr_reviews(mr_id)?;
    let has_changes = reviews.iter().any(|r| r.state == "request_changes");
    let has_approvals = reviews.iter().any(|r| r.state == "approved");
    if has_changes {
        Ok("changes_requested".to_string())
    } else if has_approvals {
        Ok("approved".to_string())
    } else {
        Ok("pending".to_string())
    }
}

    // ── releases (M5-A) ──

    pub fn create_release(
        &self,
        repo_id: &str,
        tag_name: &str,
        name: &str,
        body: &str,
        draft: bool,
        prerelease: bool,
    ) -> Result<ReleaseRow> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        conn.execute(
            "INSERT INTO releases (repo_id, tag_name, name, body, draft, prerelease)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (repo_id, tag_name) DO UPDATE SET
               name = excluded.name, body = excluded.body,
               draft = excluded.draft, prerelease = excluded.prerelease",
            rusqlite::params![repo_id, tag_name, name, body, draft, prerelease],
        )
        .map_err(|e| Error::Db(format!("create release: {e}")))?;
        let id = conn.last_insert_rowid();
        Self::release_from(&conn, id)
    }

    fn release_from(conn: &Connection, id: i64) -> Result<ReleaseRow> {
        conn.query_row(
            "SELECT id, repo_id, tag_name, name, body, draft, prerelease
             FROM releases WHERE id = ?1",
            [id],
            |r| {
                Ok(ReleaseRow {
                    id: r.get(0)?,
                    repo_id: r.get(1)?,
                    tag_name: r.get(2)?,
                    name: r.get(3)?,
                    body: r.get(4)?,
                    draft: r.get::<_, i64>(5)? != 0,
                    prerelease: r.get::<_, i64>(6)? != 0,
                })
            },
        )
        .map_err(|e| Error::Db(format!("get release: {e}")))
    }

    pub fn list_releases(&self, repo_id: &str) -> Result<Vec<ReleaseRow>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT id FROM releases WHERE repo_id = ?1 ORDER BY created_at DESC, id DESC")
            .map_err(|e| Error::Db(format!("list releases: {e}")))?;
        let ids: Vec<i64> = stmt
            .query_map([repo_id], |r| r.get(0))
            .map_err(|e| Error::Db(format!("list releases: {e}")))?
            .filter_map(|r| r.ok())
            .collect();
        ids.into_iter().map(|id| Self::release_from(&conn, id)).collect()
    }

    pub fn get_release_by_tag(&self, repo_id: &str, tag: &str) -> Result<Option<ReleaseRow>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let id: Option<i64> = conn
            .query_row(
                "SELECT id FROM releases WHERE repo_id = ?1 AND tag_name = ?2",
                rusqlite::params![repo_id, tag],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| Error::Db(format!("get release by tag: {e}")))?;
        match id {
            Some(id) => Ok(Some(Self::release_from(&conn, id)?)),
            None => Ok(None),
        }
    }

    pub fn delete_release(&self, id: i64) -> Result<()> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        conn.execute("DELETE FROM releases WHERE id = ?1", [id])
            .map_err(|e| Error::Db(format!("delete release: {e}")))?;
        Ok(())
    }

    pub fn add_release_asset(
        &self,
        release_id: i64,
        name: &str,
        size: i64,
        digest: &str,
        content_type: &str,
    ) -> Result<ReleaseAssetRow> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        conn.execute(
            "INSERT INTO release_assets (release_id, name, size, digest, content_type)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (release_id, name) DO UPDATE SET
               size = excluded.size, digest = excluded.digest,
               content_type = excluded.content_type",
            rusqlite::params![release_id, name, size, digest, content_type],
        )
        .map_err(|e| Error::Db(format!("add release asset: {e}")))?;
        let id = conn.last_insert_rowid();
        conn.query_row(
            "SELECT id, release_id, name, size, digest, content_type
             FROM release_assets WHERE id = ?1",
            [id],
            |r| {
                Ok(ReleaseAssetRow {
                    id: r.get(0)?,
                    release_id: r.get(1)?,
                    name: r.get(2)?,
                    size: r.get(3)?,
                    digest: r.get(4)?,
                    content_type: r.get(5)?,
                })
            },
        )
        .map_err(|e| Error::Db(format!("get release asset: {e}")))
    }

    pub fn list_release_assets(&self, release_id: i64) -> Result<Vec<ReleaseAssetRow>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT id, release_id, name, size, digest, content_type
                      FROM release_assets WHERE release_id = ?1 ORDER BY id")
            .map_err(|e| Error::Db(format!("list release assets: {e}")))?;
        let rows = stmt
            .query_map([release_id], |r| {
                Ok(ReleaseAssetRow {
                    id: r.get(0)?,
                    release_id: r.get(1)?,
                    name: r.get(2)?,
                    size: r.get(3)?,
                    digest: r.get(4)?,
                    content_type: r.get(5)?,
                })
            })
            .map_err(|e| Error::Db(format!("list release assets: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Db(format!("list release assets: {e}")))?);
        }
        Ok(out)
    }

    pub fn get_release_asset(&self, release_id: i64, name: &str) -> Result<Option<ReleaseAssetRow>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        conn.query_row(
            "SELECT id, release_id, name, size, digest, content_type
             FROM release_assets WHERE release_id = ?1 AND name = ?2",
            rusqlite::params![release_id, name],
            |r| {
                Ok(ReleaseAssetRow {
                    id: r.get(0)?,
                    release_id: r.get(1)?,
                    name: r.get(2)?,
                    size: r.get(3)?,
                    digest: r.get(4)?,
                    content_type: r.get(5)?,
                })
            },
        )
        .optional()
        .map_err(|e| Error::Db(format!("get release asset: {e}")))
    }

    pub fn delete_release_asset(&self, release_id: i64, name: &str) -> Result<()> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        conn.execute(
            "DELETE FROM release_assets WHERE release_id = ?1 AND name = ?2",
            rusqlite::params![release_id, name],
        )
        .map_err(|e| Error::Db(format!("delete release asset: {e}")))?;
        Ok(())
    }

    // ── actions / CI (M6-C1) ──

    pub fn upsert_workflow(
        &self,
        repo_id: &str,
        name: &str,
        path: &str,
        trigger: &str,
        enabled: bool,
    ) -> Result<i64> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        conn.execute(
            "INSERT INTO workflows (repo_id, name, path, trigger, enabled)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (repo_id, path) DO UPDATE SET
               name = excluded.name, trigger = excluded.trigger, enabled = excluded.enabled",
            rusqlite::params![repo_id, name, path, trigger, enabled],
        )
        .map_err(|e| Error::Db(format!("upsert workflow: {e}")))?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_workflows(&self, repo_id: &str) -> Result<Vec<WorkflowRow>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT id, repo_id, name, path, trigger, enabled FROM workflows WHERE repo_id = ?1 ORDER BY id")
            .map_err(|e| Error::Db(format!("list workflows: {e}")))?;
        let rows = stmt
            .query_map([repo_id], |r| {
                Ok(WorkflowRow {
                    id: r.get(0)?,
                    repo_id: r.get(1)?,
                    name: r.get(2)?,
                    path: r.get(3)?,
                    trigger: r.get(4)?,
                    enabled: r.get::<_, i64>(5)? != 0,
                })
            })
            .map_err(|e| Error::Db(format!("list workflows: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Db(format!("list workflows: {e}")))?);
        }
        Ok(out)
    }

    pub fn get_workflow(&self, id: i64) -> Result<Option<WorkflowRow>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        conn.query_row(
            "SELECT id, repo_id, name, path, trigger, enabled FROM workflows WHERE id = ?1",
            [id],
            |r| {
                Ok(WorkflowRow {
                    id: r.get(0)?,
                    repo_id: r.get(1)?,
                    name: r.get(2)?,
                    path: r.get(3)?,
                    trigger: r.get(4)?,
                    enabled: r.get::<_, i64>(5)? != 0,
                })
            },
        )
        .optional()
        .map_err(|e| Error::Db(format!("get workflow: {e}")))
    }

    pub fn create_run(&self, repo_id: &str, workflow_id: i64, trigger_ref: &str) -> Result<i64> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        conn.execute(
            "INSERT INTO runs (repo_id, workflow_id, trigger_ref) VALUES (?1, ?2, ?3)",
            rusqlite::params![repo_id, workflow_id, trigger_ref],
        )
        .map_err(|e| Error::Db(format!("create run: {e}")))?;
        Ok(conn.last_insert_rowid())
    }

    pub fn set_run_status(&self, id: i64, status: &str) -> Result<()> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let finished: Option<&str> = if status == "success" || status == "failure" {
            Some("now")
        } else {
            None
        };
        if finished.is_some() {
            conn.execute(
                "UPDATE runs SET status = ?2, finished_at = strftime('%Y-%m-%d %H:%M:%S','now') WHERE id = ?1",
                rusqlite::params![id, status],
            )
            .map_err(|e| Error::Db(format!("set run status: {e}")))?;
        } else {
            conn.execute(
                "UPDATE runs SET status = ?2 WHERE id = ?1",
                rusqlite::params![id, status],
            )
            .map_err(|e| Error::Db(format!("set run status: {e}")))?;
        }
        Ok(())
    }

    pub fn get_run(&self, id: i64) -> Result<Option<RunRow>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        conn.query_row(
            "SELECT id, repo_id, workflow_id, trigger_ref, status FROM runs WHERE id = ?1",
            [id],
            |r| {
                Ok(RunRow {
                    id: r.get(0)?,
                    repo_id: r.get(1)?,
                    workflow_id: r.get(2)?,
                    trigger_ref: r.get(3)?,
                    status: r.get(4)?,
                })
            },
        )
        .optional()
        .map_err(|e| Error::Db(format!("get run: {e}")))
    }

    pub fn list_runs(&self, repo_id: &str) -> Result<Vec<RunRow>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT id, repo_id, workflow_id, trigger_ref, status FROM runs WHERE repo_id = ?1 ORDER BY id DESC")
            .map_err(|e| Error::Db(format!("list runs: {e}")))?;
        let rows = stmt
            .query_map([repo_id], |r| {
                Ok(RunRow {
                    id: r.get(0)?,
                    repo_id: r.get(1)?,
                    workflow_id: r.get(2)?,
                    trigger_ref: r.get(3)?,
                    status: r.get(4)?,
                })
            })
            .map_err(|e| Error::Db(format!("list runs: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Db(format!("list runs: {e}")))?);
        }
        Ok(out)
    }

    /// Runs still waiting to be executed by the CI scheduler, oldest first.
    pub fn pending_runs(&self) -> Result<Vec<RunRow>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, repo_id, workflow_id, trigger_ref, status
                 FROM runs WHERE status = 'queued' ORDER BY id ASC",
            )
            .map_err(|e| Error::Db(format!("pending runs prepare: {e}")))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(RunRow {
                    id: r.get(0)?,
                    repo_id: r.get(1)?,
                    workflow_id: r.get(2)?,
                    trigger_ref: r.get(3)?,
                    status: r.get(4)?,
                })
            })
            .map_err(|e| Error::Db(format!("pending runs query: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Db(format!("pending runs row: {e}")))?);
        }
        Ok(out)
    }

    pub fn create_job(&self, run_id: i64, name: &str, log_path: &str) -> Result<i64> {
        self.create_job_detail(run_id, name, log_path, None, None, None, None)
    }

    /// Create a job with its CI execution parameters (image/run/timeout/build).
    /// `image`/`run`/`build_spec` are only stored; execution happens in the kube scheduler.
    #[allow(clippy::too_many_arguments)]
    pub fn create_job_detail(
        &self,
        run_id: i64,
        name: &str,
        log_path: &str,
        image: Option<&str>,
        run: Option<&str>,
        timeout_seconds: Option<i64>,
        build_spec: Option<&str>,
    ) -> Result<i64> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        conn.execute(
            "INSERT INTO jobs (run_id, name, status, log_path, image, run, timeout_seconds, build_spec)
                 VALUES (?1, ?2, 'queued', ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![run_id, name, log_path, image, run, timeout_seconds, build_spec],
        )
        .map_err(|e| Error::Db(format!("create job: {e}")))?;
        Ok(conn.last_insert_rowid())
    }

    pub fn set_job_status(&self, id: i64, status: &str, exit_code: Option<i32>) -> Result<()> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let finished: bool = status == "success" || status == "failure";
        if finished {
            conn.execute(
                "UPDATE jobs SET status = ?2, exit_code = ?3, finished_at = strftime('%Y-%m-%d %H:%M:%S','now') WHERE id = ?1",
                rusqlite::params![id, status, exit_code],
            )
            .map_err(|e| Error::Db(format!("set job status: {e}")))?;
        } else {
            conn.execute(
                "UPDATE jobs SET status = ?2, started_at = COALESCE(started_at, strftime('%Y-%m-%d %H:%M:%S','now')) WHERE id = ?1",
                rusqlite::params![id, status],
            )
            .map_err(|e| Error::Db(format!("set job status: {e}")))?;
        }
        Ok(())
    }

    pub fn get_job(&self, id: i64) -> Result<Option<JobRow>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        conn.query_row(
            "SELECT id, run_id, name, status, exit_code, log_path, image, run, timeout_seconds, build_spec
                 FROM jobs WHERE id = ?1",
            [id],
            |r| {
                Ok(JobRow {
                    id: r.get(0)?,
                    run_id: r.get(1)?,
                    name: r.get(2)?,
                    status: r.get(3)?,
                    exit_code: r.get(4)?,
                    log_path: r.get(5)?,
                    image: r.get(6)?,
                    run: r.get(7)?,
                    timeout_seconds: r.get(8)?,
                    build_spec: r.get(9)?,
                })
            },
        )
        .optional()
        .map_err(|e| Error::Db(format!("get job: {e}")))
    }

    pub fn list_jobs(&self, run_id: i64) -> Result<Vec<JobRow>> {
        let conn = self.pool.get().map_err(|e| Error::Db(format!("db get: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT id, run_id, name, status, exit_code, log_path, image, run, timeout_seconds, build_spec FROM jobs WHERE run_id = ?1 ORDER BY id")
            .map_err(|e| Error::Db(format!("list jobs: {e}")))?;
        let rows = stmt
            .query_map([run_id], |r| {
                Ok(JobRow {
                    id: r.get(0)?,
                    run_id: r.get(1)?,
                    name: r.get(2)?,
                    status: r.get(3)?,
                    exit_code: r.get(4)?,
                    log_path: r.get(5)?,
                    image: r.get(6)?,
                    run: r.get(7)?,
                    timeout_seconds: r.get(8)?,
                    build_spec: r.get(9)?,
                })
            })
            .map_err(|e| Error::Db(format!("list jobs: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Db(format!("list jobs: {e}")))?);
        }
        Ok(out)
    }

}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS orgs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%S','now'))
);
CREATE TABLE IF NOT EXISTS repos (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES orgs(id),
    name TEXT NOT NULL,
    default_bookmark TEXT NOT NULL DEFAULT 'main',
    git_url TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%S','now'))
);
CREATE TABLE IF NOT EXISTS changes (
    change_id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repos(id) ON UPDATE CASCADE,
    description TEXT NOT NULL DEFAULT '',
    author TEXT NOT NULL DEFAULT '',
    committer TEXT NOT NULL DEFAULT '',
    git_commit_sha TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%S','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%S','now'))
);
CREATE TABLE IF NOT EXISTS change_parents (
    change_id TEXT NOT NULL REFERENCES changes(change_id),
    parent_change_id TEXT NOT NULL REFERENCES changes(change_id),
    PRIMARY KEY (change_id, parent_change_id)
);
CREATE TABLE IF NOT EXISTS bookmarks (
    repo_id TEXT NOT NULL REFERENCES repos(id) ON UPDATE CASCADE,
    name TEXT NOT NULL,
    change_id TEXT NOT NULL REFERENCES changes(change_id),
    is_remote INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (repo_id, name)
);
CREATE TABLE IF NOT EXISTS change_id_map (
    repo_id TEXT NOT NULL REFERENCES repos(id) ON UPDATE CASCADE,
    git_commit_sha TEXT NOT NULL,
    change_id TEXT NOT NULL,
    commit_id TEXT,
    PRIMARY KEY (repo_id, git_commit_sha)
);
CREATE TABLE IF NOT EXISTS conflicts (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repos(id) ON UPDATE CASCADE,
    change_id TEXT NOT NULL REFERENCES changes(change_id),
    path TEXT NOT NULL,
    adds TEXT NOT NULL DEFAULT '[]',
    removes TEXT NOT NULL DEFAULT '[]'
);
CREATE TABLE IF NOT EXISTS merge_requests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_id TEXT NOT NULL REFERENCES repos(id) ON UPDATE CASCADE,
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    author TEXT NOT NULL DEFAULT '',
    state TEXT NOT NULL DEFAULT 'open',
    head_change_id TEXT NOT NULL,
    head_sha TEXT,
    head_branch TEXT,
    base_rev TEXT NOT NULL DEFAULT 'main',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%S','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%S','now')),
    UNIQUE (repo_id, number)
);
CREATE TABLE IF NOT EXISTS mr_reviews (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    mr_id INTEGER NOT NULL REFERENCES merge_requests(id) ON DELETE CASCADE,
    reviewer TEXT NOT NULL,
    state TEXT NOT NULL,
    body TEXT NOT NULL DEFAULT '',
    commit_sha TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%S','now'))
);
CREATE TABLE IF NOT EXISTS mr_comments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    mr_id INTEGER NOT NULL REFERENCES merge_requests(id) ON DELETE CASCADE,
    author TEXT NOT NULL,
    body TEXT NOT NULL,
    path TEXT,
    commit_sha TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%S','now'))
);
CREATE TABLE IF NOT EXISTS workflows (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_id TEXT NOT NULL REFERENCES repos(id) ON UPDATE CASCADE,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    trigger TEXT NOT NULL DEFAULT 'push',
    enabled INTEGER NOT NULL DEFAULT 1,
    UNIQUE (repo_id, path)
);
CREATE TABLE IF NOT EXISTS runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_id TEXT NOT NULL REFERENCES repos(id) ON UPDATE CASCADE,
    workflow_id INTEGER NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    trigger_ref TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'queued',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%S','now')),
    finished_at TEXT
);
CREATE TABLE IF NOT EXISTS jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued',
    exit_code INTEGER,
    log_path TEXT,
    image TEXT,
    run TEXT,
    timeout_seconds INTEGER,
    build_spec TEXT,
    started_at TEXT,
    finished_at TEXT
);
CREATE TABLE IF NOT EXISTS releases (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_id TEXT NOT NULL REFERENCES repos(id) ON UPDATE CASCADE,
    tag_name TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    body TEXT NOT NULL DEFAULT '',
    draft INTEGER NOT NULL DEFAULT 0,
    prerelease INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%S','now')),
    UNIQUE (repo_id, tag_name)
);
CREATE TABLE IF NOT EXISTS release_assets (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    release_id INTEGER NOT NULL REFERENCES releases(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    size INTEGER NOT NULL,
    digest TEXT NOT NULL,
    content_type TEXT NOT NULL DEFAULT 'application/octet-stream',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%S','now')),
    UNIQUE (release_id, name)
);
"#;
#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("meta.db")).unwrap();
        (dir, db)
    }

    fn seed_repo(db: &Db, org: &str, repo: &str) -> String {
        let repo_id = format!("{org}/{repo}");
        db.upsert_org(org, org).unwrap();
        db.upsert_repo(&repo_id, org, repo, "main", None).unwrap();
        repo_id
    }

    // ── open ──

    #[test]
    fn open_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        Db::open(&path).unwrap();
        // Second open is a no-op (SCHEMA is CREATE TABLE IF NOT EXISTS).
        Db::open(&path).unwrap();
    }

    // ── MR lifecycle ──

    #[test]
    fn mr_numbers_increment_per_repo() {
        let (_d, db) = db();
        let id = seed_repo(&db, "o", "r");
        let m1 = db.create_mr(&id, "t", "", "a", "cid1", None, Some("b1"), "main").unwrap();
        let m2 = db.create_mr(&id, "t2", "", "a", "cid2", None, Some("b2"), "main").unwrap();
        assert_eq!((m1.number, m2.number), (1, 2));
        // Different repo restarts numbering.
        let id2 = seed_repo(&db, "o", "r2");
        let m3 = db.create_mr(&id2, "t", "", "a", "cid", None, None, "main").unwrap();
        assert_eq!(m3.number, 1);
    }

    #[test]
    fn mr_cascade_deletes_reviews_and_comments() {
        let (_d, db) = db();
        let id = seed_repo(&db, "o", "r");
        let mr = db.create_mr(&id, "t", "", "a", "c", None, None, "main").unwrap();
        db.add_mr_review(mr.id, "rev", "approved", "ok", None).unwrap();
        db.add_mr_comment(mr.id, "rev", "hello", Some("f.rs"), None).unwrap();
        assert_eq!(db.list_mr_reviews(mr.id).unwrap().len(), 1);
        assert_eq!(db.list_mr_comments(mr.id).unwrap().len(), 1);
        // Delete via raw SQL on parent (no delete_mr API — cascade proves FK).
        let conn = db.pool.get().unwrap();
        conn.execute("DELETE FROM merge_requests WHERE id = ?1", [mr.id]).unwrap();
        drop(conn);
        assert!(db.list_mr_reviews(mr.id).unwrap().is_empty());
        assert!(db.list_mr_comments(mr.id).unwrap().is_empty());
    }

    #[test]
    fn review_state_priority_changes_over_approvals() {
        let (_d, db) = db();
        let id = seed_repo(&db, "o", "r");
        let mr = db.create_mr(&id, "t", "", "a", "c", None, None, "main").unwrap();
        assert_eq!(db.mr_review_state(mr.id).unwrap(), "pending");
        db.add_mr_review(mr.id, "a", "approved", "", None).unwrap();
        assert_eq!(db.mr_review_state(mr.id).unwrap(), "approved");
        db.add_mr_review(mr.id, "b", "request_changes", "", None).unwrap();
        // request_changes wins even when an approval exists.
        assert_eq!(db.mr_review_state(mr.id).unwrap(), "changes_requested");
    }

    #[test]
    fn update_mr_tracks_head_and_state() {
        let (_d, db) = db();
        let id = seed_repo(&db, "o", "r");
        let mr = db.create_mr(&id, "t", "", "a", "cid", Some("sha1"), None, "main").unwrap();
        let updated = db.update_mr(mr.id, Some("closed"), Some("cid2"), Some("sha2")).unwrap();
        assert_eq!((updated.state.as_str(), updated.head_sha.as_deref()), ("closed", Some("sha2")));
    }

    #[test]
    fn list_open_mrs_for_reassoc_filters_state() {
        let (_d, db) = db();
        let id = seed_repo(&db, "o", "r");
        let open = db.create_mr(&id, "a", "", "x", "c1", None, Some("b1"), "main").unwrap();
        let closed = db.create_mr(&id, "b", "", "x", "c2", None, Some("b2"), "main").unwrap();
        db.update_mr(closed.id, Some("closed"), None, None).unwrap();
        let rows = db.list_open_mrs_for_reassoc().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, open.id);
        assert_eq!(rows[0].1, "c1");
        assert_eq!(rows[0].2.as_deref(), Some("b1"));
    }

    // ── releases ──

    #[test]
    fn release_upsert_same_tag_replaces_fields() {
        let (_d, db) = db();
        let id = seed_repo(&db, "o", "r");
        let r1 = db.create_release(&id, "v1", "n", "b", false, false).unwrap();
        let r2 = db.create_release(&id, "v1", "n2", "b2", true, true).unwrap();
        assert_eq!(r1.id, r2.id);
        assert_eq!((r2.name.as_str(), r2.draft, r2.prerelease), ("n2", true, true));
        assert_eq!(db.list_releases(&id).unwrap().len(), 1);
    }

    #[test]
    fn release_asset_upsert_and_cascade() {
        let (_d, db) = db();
        let id = seed_repo(&db, "o", "r");
        let rel = db.create_release(&id, "v1", "", "", false, false).unwrap();
        let a1 = db.add_release_asset(rel.id, "bin", 3, &"d".repeat(64), "app/bin").unwrap();
        // Same-name upload replaces the row.
        let a2 = db.add_release_asset(rel.id, "bin", 9, &"e".repeat(64), "text/plain").unwrap();
        assert_eq!(a1.id, a2.id);
        assert_eq!(a2.size, 9);
        assert_eq!(db.list_release_assets(rel.id).unwrap().len(), 1);
        // Deleting the release cascades.
        db.delete_release(rel.id).unwrap();
        assert!(db.get_release_asset(rel.id, "bin").unwrap().is_none());
    }

    #[test]
    fn get_release_by_tag_and_missing() {
        let (_d, db) = db();
        let id = seed_repo(&db, "o", "r");
        db.create_release(&id, "v9", "", "", false, false).unwrap();
        assert!(db.get_release_by_tag(&id, "v9").unwrap().is_some());
        assert!(db.get_release_by_tag(&id, "nope").unwrap().is_none());
    }

    // ── workflows / runs / jobs ──

    #[test]
    fn workflow_upsert_is_keyed_by_path() {
        let (_d, db) = db();
        let id = seed_repo(&db, "o", "r");
        let w1 = db.upsert_workflow(&id, "CI", ".github/workflows/ci.yml", "push", true).unwrap();
        let w2 = db.upsert_workflow(&id, "CI v2", ".github/workflows/ci.yml", "push", false).unwrap();
        assert_eq!(w1, w2);
        let rows = db.list_workflows(&id).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "CI v2");
        assert!(!rows[0].enabled);
    }

    #[test]
    fn run_and_job_status_machine() {
        let (_d, db) = db();
        let id = seed_repo(&db, "o", "r");
        let w = db.upsert_workflow(&id, "CI", "p", "push", true).unwrap();
        let run = db.create_run(&id, w, "sha").unwrap();
        db.set_run_status(run, "running").unwrap();
        let job = db.create_job(run, "build", "/tmp/x.log").unwrap();
        db.set_job_status(job, "running", None).unwrap();
        db.set_job_status(job, "failure", Some(1)).unwrap();
        let j = db.get_job(job).unwrap().unwrap();
        assert_eq!(j.status, "failure");
        assert_eq!(j.exit_code, Some(1));
        db.set_run_status(run, "failure").unwrap();
        assert_eq!(db.get_run(run).unwrap().unwrap().status, "failure");
    }

    #[test]
    fn jobs_cascade_on_run_delete() {
        let (_d, db) = db();
        let id = seed_repo(&db, "o", "r");
        let w = db.upsert_workflow(&id, "CI", "p", "push", true).unwrap();
        let run = db.create_run(&id, w, "sha").unwrap();
        db.create_job(run, "j", "/tmp/l").unwrap();
        let conn = db.pool.get().unwrap();
        conn.execute("DELETE FROM runs WHERE id = ?1", [run]).unwrap();
        drop(conn);
        assert!(db.list_jobs(run).unwrap().is_empty());
    }
}
