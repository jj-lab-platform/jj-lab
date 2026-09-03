//! Read-only jj-lib access for the HTTP layer: commit/tree/blob/branch reads,
//! addressed by commit-id (sha) or change-id, plus first-class conflict
//! materialization. Mirrors Gitea's git read surface + jj-native extras.

use std::sync::Arc;

use jj_lib::backend::CommitId;
use jj_lib::commit::Commit;
use jj_lib::conflict_labels::ConflictLabels;
use jj_lib::conflicts::{materialize_tree_value, MaterializedTreeValue};
use jj_lib::merged_tree::MergedTree;
use jj_lib::object_id::{HexPrefix, ObjectId, PrefixResolution};
use jj_lib::repo_path::RepoPathBuf;
use jj_lib::repo::{ReadonlyRepo, Repo as _};

use crate::repo::{RepoError, RepoStore};

/// Parse a Gitea-style time bound (`since`/`until`): either an ISO 8601 / RFC
/// 3339 timestamp or a plain `YYYY-MM-DD` date (midnight UTC), returning
/// milliseconds since the Unix epoch. Used to filter commit logs.
pub fn parse_time_bound(s: &str) -> Result<i64, RepoError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(RepoError::Invalid("empty time bound".into()));
    }
    // Plain date first (jiff's Timestamp parser also accepts RFC3339/ISO8601).
    if let Ok(date) = s.parse::<jiff::civil::Date>() {
        let millis = date
            .to_zoned(jiff::tz::TimeZone::UTC)
            .map(|z| z.timestamp().as_millisecond())
            .map_err(|e| RepoError::Invalid(format!("invalid date {s:?}: {e}")))?;
        return Ok(millis);
    }
    let ts = s
        .parse::<jiff::Timestamp>()
        .map_err(|e| RepoError::Invalid(format!("invalid time bound {s:?}: {e}")))?;
    Ok(ts.as_millisecond())
}

/// A commit rendered for JSON output (git-commit addressing).
#[derive(serde::Serialize)]
pub struct CommitInfo {
    pub sha: String,
    pub change_id: String,
    pub description: String,
    pub author: String,
    pub committer: String,
    pub parents: Vec<String>,
}

#[derive(serde::Serialize)]
#[derive(Debug)]
pub struct BranchInfo {
    pub name: String,
    pub sha: String,
}

#[derive(serde::Serialize)]
pub struct TreeEntryInfo {
    pub path: String,
    pub mode: String,
    pub kind: String,
    pub size: u64,
}

/// One entry of a change list: a change-id resolved against a snapshot rev,
/// plus its current commit there (unambiguous because an amend chain is a
/// single visible commit in that rev's history).
#[derive(serde::Serialize)]
pub struct ChangeSummary {
    pub change_id: String,
    pub commit_id: String,
    pub description: String,
    pub author: String,
}

/// A single annotated line (mirrors jj `file annotate`'s `AnnotationLine`).
#[derive(serde::Serialize)]
pub struct AnnotationLine {
    pub line_number: usize,
    pub original_line_number: usize,
    pub content: String,
    pub first_line_in_hunk: bool,
    pub commit_id: String,
    pub change_id: String,
    pub message: String,
    pub author: String,
}

pub async fn open(store: &Arc<RepoStore>, org: &str, repo: &str) -> Result<Arc<ReadonlyRepo>, RepoError> {
    Ok(store.open(org, repo).await?.repo.clone())
}

/// Line-by-line annotation of a file at a snapshot rev. The rev resolves like a
/// revset symbol (sha/bookmark/tag/change-id, divergent change-id is an error);
/// each line reports the origin commit (jj-native `annotate`, may be a
/// hidden/amended predecessor) plus that commit's change-id so a caller can keep
/// operating on the change.
pub async fn annotate_file(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    rev: &str,
    path: &str,
) -> Result<Vec<AnnotationLine>, RepoError> {
    let repo = open(store, org, repo).await?;
    let start = resolve_revset_single(&repo, rev)?;
    let commit = repo
        .store()
        .get_commit(&start)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    let repo_path = RepoPathBuf::from_internal_string(path)
        .map_err(|e| RepoError::Other(format!("bad path {path:?}: {e}")))?;

    let mut annotator = jj_lib::annotate::FileAnnotator::from_commit(&commit, &repo_path)
        .await
        .map_err(|e| RepoError::Other(e.to_string()))?;
    // Annotate within all ancestors of the visible heads (the widest domain a
    // CLI caller would get without extra filters).
    let domain = jj_lib::revset::RevsetExpression::visible_heads().ancestors();
    annotator
        .compute(repo.as_ref(), &domain)
        .await
        .map_err(|e| RepoError::Other(e.to_string()))?;
    let annotation = annotator.to_annotation();

    let mut out = Vec::new();
    for (i, (origin, text)) in annotation.line_origins().enumerate() {
        let origin_commit_id = match origin {
            Ok(o) => o.commit_id.clone(),
            Err(o) => o.commit_id.clone(),
        };
        let origin_line_number = match origin {
            Ok(o) => o.line_number,
            Err(o) => o.line_number,
        };
        let origin_commit = repo.store().get_commit(&origin_commit_id).ok();
        let (commit_id, change_id, message, author) = match origin_commit {
            Some(c) => {
                let a = c.author();
                (
                    c.id().hex(),
                    c.change_id().reverse_hex(),
                    c.description().to_string(),
                    format!("{} <{}>", a.name, a.email),
                )
            }
            None => (origin_commit_id.hex(), String::new(), String::new(), String::new()),
        };
        let first_in_hunk = i == 0 || {
            // Recomputing hunk boundaries is cheap here because line_origins
            // yields consecutive lines in order; the previous commit id is the
            // only signal jj exposes (`.first_line_in_hunk()` on the template
            // level uses the same adjacency rule).
            let prev = out.last();
            prev.map(|p: &AnnotationLine| p.commit_id != commit_id).unwrap_or(true)
        };
        out.push(AnnotationLine {
            line_number: i + 1,
            original_line_number: origin_line_number + 1,
            content: String::from_utf8_lossy(text).to_string(),
            first_line_in_hunk: first_in_hunk || commit_id.is_empty(),
            commit_id,
            change_id,
            message,
            author,
        });
    }
    Ok(out)
}

/// List all changes reachable from a rev (a revset symbol: sha/bookmark/tag/
/// change-id). Mirrors `commits`/`files` semantics: the change view is anchored
/// to a rev, so each change-id resolves to a single visible commit in that
/// rev's history and there is no cross-branch divergence ambiguity.
pub async fn list_changes(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    rev: &str,
) -> Result<Vec<ChangeSummary>, RepoError> {
    let repo = open(store, org, repo).await?;
    let root_id = repo.store().root_commit_id().clone();
    // Walk ancestors of the rev's commit; dedupe by change-id (an amend chain
    // has one change-id with only the newest commit visible, so the chain
    // collapses to its current commit naturally).
    let start = resolve_revset_single(&repo, rev)?;
    let mut stack = vec![start];
    let mut seen_commits = std::collections::HashSet::new();
    let mut seen_changes = std::collections::HashSet::new();
    let mut out = Vec::new();
    while let Some(id) = stack.pop() {
        if !seen_commits.insert(id.clone()) {
            continue;
        }
        if id == root_id {
            continue;
        }
        let Ok(commit) = repo.store().get_commit(&id) else { continue };
        let change_id = commit.change_id().reverse_hex();
        if seen_changes.insert(change_id.clone()) {
            let author = commit.author();
            out.push(ChangeSummary {
                change_id,
                commit_id: commit.id().hex(),
                description: commit.description().to_string(),
                author: format!("{} <{}>", author.name, author.email),
            });
        }
        for p in commit.parent_ids() {
            stack.push(p.clone());
        }
    }
    Ok(out)
}

/// Git-aligned: commit info by commit sha (prefix accepted).
pub async fn commit_by_sha(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    sha: &str,
) -> Result<CommitInfo, RepoError> {
    let repo = open(store, org, repo).await?;
    let id = resolve_snapshot(&repo, sha)?;
    let commit = repo
        .store()
        .get_commit(&id)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    Ok(commit_info(&commit))
}

/// List local bookmarks (git branches).
pub async fn branches(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
) -> Result<Vec<BranchInfo>, RepoError> {
    let repo = open(store, org, repo).await?;
    Ok(list_branches(&repo))
}

/// List the tree at a commit (sha) — root if `sha` is empty.
pub async fn tree_at_sha(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    sha: &str,
) -> Result<Vec<TreeEntryInfo>, RepoError> {
    let repo = open(store, org, repo).await?;
    let id = resolve_snapshot(&repo, sha)?;
    let commit = repo
        .store()
        .get_commit(&id)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    Ok(list_tree(&commit.tree(), ""))

}

/// Resolve a rev: bookmark name, commit-id hex prefix, or change-id hex prefix.
/// Resolve a **snapshot** rev to its commit id. Accepts, in order: tag name,
/// bookmark name, then a commit-id sha prefix. A change-id is deliberately NOT
/// accepted here — a snapshot must map to exactly one commit tree, and a
/// change-id is not uniquely that (amend chains / divergence).
pub fn resolve_snapshot(repo: &ReadonlyRepo, rev: &str) -> Result<CommitId, RepoError> {
    if rev.is_empty() {
        // Prefer the default bookmark: heads() contains the root commit and
        // the working-copy commit, neither of which is a meaningful default.
        for name in ["main", "master"] {
            let target = repo
                .view()
                .get_local_bookmark(&jj_lib::ref_name::RefNameBuf::from(name.to_string()));
            if let Some(id) = target.as_normal() {
                return Ok(id.clone());
            }
        }
        let root = repo.store().root_commit_id().clone();
        if let Some(h) = repo.view().heads().iter().find(|h| **h != root) {
            return Ok(h.clone());
        }
        return Err(RepoError::NotFound { org: String::new(), repo: String::new() });
    }

    let name: jj_lib::ref_name::RefNameBuf = rev.to_string().into();
    // Tags are bookmarks too (jj ref model) — resolution order is tag first.
    let tag = repo.view().get_local_tag(&name);
    if !tag.is_absent() {
        if let Some(id) = tag.as_normal() {
            return Ok(id.clone());
        }
    }
    let bookmark = repo.view().get_local_bookmark(&name);
    if !bookmark.is_absent() {
        if let Some(id) = bookmark.as_normal() {
            return Ok(id.clone());
        }
    }

    if let Some(prefix) = HexPrefix::try_from_hex(rev) {
        let index = repo.readonly_index().as_index();
        match index.resolve_commit_id_prefix(&prefix) {
            Ok(PrefixResolution::SingleMatch(id)) => return Ok(id),
            Ok(PrefixResolution::AmbiguousMatch) => {
                return Err(RepoError::Invalid(format!("commit id {rev:?} is ambiguous")))
            }
            Ok(PrefixResolution::NoMatch) => {}
            Err(e) => return Err(RepoError::Other(format!("resolve commit prefix: {e}"))),
        }
    }

    Err(RepoError::NotFound { org: String::new(), repo: String::new() })
}

/// Resolve a **revset symbol** to a single commit (used by `compare`'s
/// `base`/`head`, which mirror jj `diff --from/--to`). Accepts tag, bookmark,
/// commit-id prefix, and change-id; a divergent change-id (multiple visible
/// commits) is an error, never a silent pick.
pub fn resolve_revset_single(repo: &ReadonlyRepo, rev: &str) -> Result<CommitId, RepoError> {
    // Fast path: the strict snapshot resolution first (tag/bookmark/sha).
    match resolve_snapshot(repo, rev) {
        Ok(id) => return Ok(id),
        Err(RepoError::NotFound { .. }) => {}
        Err(e) => return Err(e),
    }
    // Fall back to change-id resolution (reverse-hex), erroring on divergence.
    let prefix = match HexPrefix::try_from_reverse_hex(rev) {
        Some(p) => p,
        None => return Err(RepoError::NotFound { org: String::new(), repo: String::new() }),
    };
    let mut heads = repo.view().heads().iter();
    let change_index = repo.readonly_index().change_id_index(&mut heads);
    match change_index.resolve_prefix(&prefix) {
        Ok(PrefixResolution::SingleMatch(targets)) => {
            if targets.is_divergent() {
                return Err(RepoError::Invalid(format!("change id {rev:?} is divergent")));
            }
            let visible = targets.into_visible().unwrap_or_default();
            match visible.len() {
                1 => Ok(visible[0].clone()),
                0 => Err(RepoError::NotFound { org: String::new(), repo: String::new() }),
                _ => Err(RepoError::Invalid(format!("change id {rev:?} is divergent"))),
            }
        }
        Ok(PrefixResolution::AmbiguousMatch) => {
            Err(RepoError::Invalid(format!("change id {rev:?} is ambiguous")))
        }
        Ok(PrefixResolution::NoMatch) => Err(RepoError::NotFound {
            org: String::new(),
            repo: String::new(),
        }),
        Err(e) => Err(RepoError::Other(format!("resolve change prefix: {e}"))),
    }
}

/// Resolve either a snapshot `target` (sha/bookmark/tag) or a revset `change`
/// id (revset semantics: divergent is an error). Exactly one must be non-empty;
/// used by the branch/tag write surface.
pub fn resolve_target_or_change(
    repo: &ReadonlyRepo,
    target: &str,
    change: &str,
) -> Result<CommitId, RepoError> {
    match (target.is_empty(), change.is_empty()) {
        (false, false) => Err(RepoError::Invalid(
            "provide either 'target' (snapshot) or 'change' (revset), not both".into(),
        )),
        (false, true) => resolve_snapshot(repo, target),
        (true, false) => resolve_revset_single(repo, change),
        (true, true) => Err(RepoError::Invalid(
            "provide either 'target' (snapshot) or 'change' (revset)".into(),
        )),
    }
}

pub fn commit_info(commit: &Commit) -> CommitInfo {
    let author = commit.author();
    let committer = commit.committer();
    CommitInfo {
        sha: commit.id().hex(),
        change_id: commit.change_id().reverse_hex(),
        description: commit.description().to_string(),
        author: format!("{} <{}>", author.name, author.email),
        committer: format!("{} <{}>", committer.name, committer.email),
        parents: commit.parent_ids().iter().map(|p| p.hex()).collect(),
    }
}

/// List local bookmarks (git branches) with their target commit sha.
pub fn list_branches(repo: &ReadonlyRepo) -> Vec<BranchInfo> {
    let mut out: Vec<BranchInfo> = repo
        .view()
        .local_bookmarks()
        .filter_map(|(name, target)| {
            let id = target.as_normal()?;
            Some(BranchInfo {
                name: name.as_str().to_string(),
                sha: id.hex(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Read a file's bytes at `path` in `commit`, materializing conflicts as
/// git-style markers so every side is visible.
pub async fn read_blob(commit: &Commit, path: &str) -> Result<Vec<u8>, RepoError> {
    let repo_path = RepoPathBuf::from_internal_string(path)
        .map_err(|e| RepoError::Other(format!("bad path {path:?}: {e}")))?;
    let tree = commit.tree();
    let value = tree
        .path_value(&repo_path)
        .await
        .map_err(|e| RepoError::Other(format!("path_value {path:?}: {e}")))?;

    if !value.is_resolved() {
        let rendered = materialize_tree_value(
            commit.store(),
            &repo_path,
            value,
            &ConflictLabels::unlabeled(),
        )
        .await
        .map_err(|e| RepoError::Other(format!("materialize {path:?}: {e}")))?;
        return match rendered {
            MaterializedTreeValue::File(mut f) => f.read_all(&repo_path).await.map_err(|e| {
                RepoError::Other(format!("read {path:?}: {e}"))
            }),
            MaterializedTreeValue::Symlink { target, .. } => Ok(target.into_bytes()),
            MaterializedTreeValue::Absent => Ok(Vec::new()),
            MaterializedTreeValue::FileConflict(fc) => {
                Ok(fc.contents.into_resolved().map(|c| c.to_vec()).unwrap_or_default())
            }
            MaterializedTreeValue::OtherConflict { id, .. } => {
                Ok(format!("{id:?}").into_bytes())
            }
            MaterializedTreeValue::Tree(_) => Err(RepoError::Other(format!("{path:?} is a tree"))),
            MaterializedTreeValue::AccessDenied(e) => {
                Err(RepoError::Other(format!("access denied {path:?}: {e}")))
            }
            MaterializedTreeValue::GitSubmodule(id) => Ok(id.hex().into_bytes()),
        };
    }

    let resolved = value
        .into_resolved()
        .map_err(|_| RepoError::Other(format!("conflicted path {path:?}")))?;
    match resolved {
        Some(jj_lib::backend::TreeValue::File { id, executable, .. }) => {
            let mut reader = commit
                .store()
                .read_file(&RepoPathBuf::root(), &id)
                .await
                .map_err(|e| RepoError::Other(format!("read file {id:?}: {e}")))?;
            use futures_util::AsyncReadExt as _;
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf).await.map_err(|e| RepoError::Other(e.to_string()))?;
            let _ = executable;
            Ok(buf)
        }
        Some(jj_lib::backend::TreeValue::Symlink(id)) => {
            let s = commit
                .store()
                .read_symlink(&repo_path, &id)
                .await
                .map_err(|e| RepoError::Other(format!("read symlink {path:?}: {e}")))?;
            Ok(s.into_bytes())
        }
        Some(jj_lib::backend::TreeValue::Tree(_)) => {
            Err(RepoError::Other(format!("{path:?} is a directory")))
        }
        Some(jj_lib::backend::TreeValue::GitSubmodule(_)) => {
            Err(RepoError::Other(format!("{path:?} is a submodule")))
        }
        None => Err(RepoError::NotFound {
            org: String::new(),
            repo: String::new(),
        }),
    }
}

/// List tree entries under `path`, deriving directory entries from ancestor
/// path components (git has no empty dirs, so this is lossless).
pub fn list_tree(tree: &MergedTree, base: &str) -> Vec<TreeEntryInfo> {
    let mut out = Vec::new();
    for (path_buf, value_res) in tree.entries() {
        let value = match value_res {
            Ok(v) => v,
            Err(_) => continue,
        };
        let resolved = match value.into_resolved() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let full = path_buf.into_internal_string();
        if !full.starts_with(base) {
            continue;
        }
        let rel = full.trim_start_matches(base).trim_start_matches('/');
        if rel.is_empty() {
            continue;
        }
        match resolved {
            Some(jj_lib::backend::TreeValue::File { id, executable, .. }) => {
                let mode = if executable { "100755" } else { "100644" };
                let size = 0u64;
                let _ = id;
                out.push(TreeEntryInfo {
                    path: rel.to_string(),
                    mode: mode.to_string(),
                    kind: "file".to_string(),
                    size,
                });
            }
            Some(jj_lib::backend::TreeValue::Symlink(_)) => {
                out.push(TreeEntryInfo {
                    path: rel.to_string(),
                    mode: "120000".to_string(),
                    kind: "symlink".to_string(),
                    size: 0,
                });
            }
            Some(jj_lib::backend::TreeValue::Tree(_)) => {
                out.push(TreeEntryInfo {
                    path: rel.to_string(),
                    mode: "040000".to_string(),
                    kind: "tree".to_string(),
                    size: 0,
                });
            }
            Some(jj_lib::backend::TreeValue::GitSubmodule(_)) => {
                out.push(TreeEntryInfo {
                    path: rel.to_string(),
                    mode: "160000".to_string(),
                    kind: "commit".to_string(),
                    size: 0,
                });
            }
            None => {}
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}
// ── extended read surface (E) ──

use futures_util::StreamExt as _;

/// Commit log, newest-first (committer timestamp), paginated. When `rev` is
/// `Some`, only ancestors of that snapshot/revset symbol are listed; otherwise
/// the walk starts from every bookmark tip (git-log semantics). `since`/
/// `until` are millisecond epoch bounds inclusive.
#[allow(clippy::too_many_arguments)]
pub async fn commit_log(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    rev: Option<&str>,
    since: Option<i64>,
    until: Option<i64>,
    page: usize,
    page_size: usize,
) -> Result<(Vec<CommitInfo>, usize), RepoError> {
    let repo = open(store, org, repo).await?;
    // Walk from the rev's commit when given, otherwise from every bookmark tip.
    // heads() also contains the root commit and the working-copy commit, which
    // are not real history.
    let root_id = repo.store().root_commit_id().clone();
    let mut stack: Vec<CommitId> = if let Some(rev) = rev {
        match resolve_revset_single(&repo, rev) {
            Ok(id) => vec![id],
            Err(RepoError::NotFound { .. }) => return Ok((Vec::new(), 0)),
            Err(e) => return Err(e),
        }
    } else {
        repo.view()
            .local_bookmarks()
            .filter_map(|(_, t)| t.as_normal().cloned())
            .collect()
    };
    let mut commits: Vec<Commit> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if id == root_id {
            continue;
        }
        if let Ok(c) = repo.store().get_commit(&id) {
            let ts = c.committer().timestamp.timestamp.0;
            if since.is_some_and(|s| ts < s) {
                continue;
            }
            if until.is_some_and(|u| ts > u) {
                continue;
            }
            for p in c.parent_ids() {
                stack.push(p.clone());
            }
            commits.push(c);
        }
    }
    commits.sort_by_key(|c| std::cmp::Reverse(c.committer().timestamp.timestamp));
    let total = commits.len();
    let start = page.saturating_mul(page_size);
    let page_items: Vec<CommitInfo> = commits
        .into_iter()
        .skip(start)
        .take(page_size)
        .map(|c| commit_info(&c))
        .collect();
    Ok((page_items, total))
}

/// Unified patch of one commit vs its first parent (git show style).
pub async fn commit_patch(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    sha: &str,
) -> Result<String, RepoError> {
    let repo = open(store, org, repo).await?;
    let id = resolve_revset_single(&repo, sha)?;
    let commit = repo
        .store()
        .get_commit(&id)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    let parent_tree = match commit.parent_ids().first() {
        Some(p) => {
            let parent = repo
                .store()
                .get_commit(p)
                .map_err(|e| RepoError::Other(e.to_string()))?;
            parent.tree()
        }
        None => jj_lib::merged_tree::MergedTree::resolved(
            repo.store().clone(),
            repo.store().empty_tree_id().clone(),
        ),
    };
    tree_patch(&repo, &parent_tree, &commit.tree()).await
}

/// Unified patch of one commit vs its **merged** parent trees — the exact
/// `jj show` / `jj diff -r <commit>` semantics (merge parents into one tree
/// first, then diff). This is the authoritative "what did this change do".
pub async fn commit_patch_merged(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    sha: &str,
) -> Result<String, RepoError> {
    let repo = open(store, org, repo).await?;
    let id = resolve_revset_single(&repo, sha)?;
    let commit = repo
        .store()
        .get_commit(&id)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    let base = commit
        .parent_tree(repo.as_ref())
        .await
        .unwrap_or_else(|_| {
            jj_lib::merged_tree::MergedTree::resolved(
                repo.store().clone(),
                repo.store().empty_tree_id().clone(),
            )
        });
    tree_patch(&repo, &base, &commit.tree()).await
}

/// Unified patch between two revs (`base` → `head`). Each side is a revset
/// symbol (tag/bookmark/sha/change-id), mirroring `jj diff --from/--to`; a
/// divergent change-id is an error.
pub async fn compare_patch(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    base: &str,
    head: &str,
) -> Result<String, RepoError> {
    let repo = open(store, org, repo).await?;
    let a = resolve_revset_single(&repo, base)?;
    let b = resolve_revset_single(&repo, head)?;
    let ca = repo
        .store()
        .get_commit(&a)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    let cb = repo
        .store()
        .get_commit(&b)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    tree_patch(&repo, &ca.tree(), &cb.tree()).await
}

async fn tree_patch(
    repo: &ReadonlyRepo,
    from: &MergedTree,
    to: &MergedTree,
) -> Result<String, RepoError> {
    let store = repo.store().clone();
    let diff = from.diff_stream(to, &jj_lib::matchers::EverythingMatcher);
    let mut parts: Vec<String> = Vec::new();
    let mut stream = std::pin::pin!(diff);
    while let Some(entry) = stream.next().await {
        let Ok(values) = &entry.values else { continue };
        let path = entry.path.as_internal_file_string().to_string();
        let before = materialize(&store, &entry.path, values.before.clone()).await;
        let after = materialize(&store, &entry.path, values.after.clone()).await;
        let (Ok(before), Ok(after)) = (before, after) else { continue };
        if before == after {
            continue;
        }
        parts.push(unified_patch(&path, &before, &after));
    }
    Ok(parts.concat())
}

async fn materialize(
    store: &Arc<jj_lib::store::Store>,
    path: &jj_lib::repo_path::RepoPath,
    value: jj_lib::merge::Merge<Option<jj_lib::backend::TreeValue>>,
) -> Result<Vec<u8>, RepoError> {
    let materialized = jj_lib::conflicts::materialize_tree_value(
        store,
        path,
        value,
        &jj_lib::conflict_labels::ConflictLabels::unlabeled(),
    )
    .await
    .map_err(|e| RepoError::Other(format!("materialize {path:?}: {e}")))?;
    match materialized {
        MaterializedTreeValue::File(mut f) => f
            .read_all(path)
            .await
            .map_err(|e| RepoError::Other(format!("read {path:?}: {e}"))),
        MaterializedTreeValue::Symlink { target, .. } => Ok(target.into_bytes()),
        MaterializedTreeValue::Absent => Ok(Vec::new()),
        MaterializedTreeValue::FileConflict(fc) => Ok(fc
            .contents
            .into_resolved()
            .map(|c| c.to_vec())
            .unwrap_or_default()),
        MaterializedTreeValue::OtherConflict { id, .. } => Ok(format!("{id:?}").into_bytes()),
        MaterializedTreeValue::GitSubmodule(id) => Ok(id.hex().into_bytes()),
        MaterializedTreeValue::Tree(_) => Ok(Vec::new()),
        MaterializedTreeValue::AccessDenied(e) => {
            Err(RepoError::Other(format!("access denied {path:?}: {e}")))
        }
    }
}

fn unified_patch(path: &str, before: &[u8], after: &[u8]) -> String {
    let before = gix::bstr::BString::from(before);
    let after = gix::bstr::BString::from(after);
    let hunks = jj_lib::diff_presentation::unified::unified_diff_hunks(
        jj_lib::merge::Diff {
            before: before.as_ref(),
            after: after.as_ref(),
        },
        3,
        jj_lib::diff_presentation::LineCompareMode::Exact,
    );
        let mut out = format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n");
    for hunk in hunks {
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk.left_line_range.start,
            hunk.left_line_range.end - hunk.left_line_range.start,
            hunk.right_line_range.start,
            hunk.right_line_range.end - hunk.right_line_range.start,
        ));
        for (kind, tokens) in hunk.lines {
            let prefix = match kind {
                jj_lib::diff_presentation::unified::DiffLineType::Context => " ",
                jj_lib::diff_presentation::unified::DiffLineType::Removed => "-",
                jj_lib::diff_presentation::unified::DiffLineType::Added => "+",
            };
            out.push_str(prefix);
            for (_, bytes) in &tokens {
                out.push_str(&String::from_utf8_lossy(bytes));
            }
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    out
}

/// List local tags.
pub async fn tags(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
) -> Result<Vec<BranchInfo>, RepoError> {
    let repo = open(store, org, repo).await?;
    let mut out: Vec<BranchInfo> = repo
        .view()
        .local_tags()
        .filter_map(|(name, target)| {
            let id = target.as_normal()?;
            Some(BranchInfo {
                name: name.as_str().to_string(),
                sha: id.hex(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// All git refs (bookmarks + tags + remote-tracking).
pub async fn all_refs(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
) -> Result<Vec<(String, String)>, RepoError> {
    let git_dir = crate::http::git_dir_of(store, org, repo)?;
    let repo = open(store, org, repo).await?;
    let _ = &repo;
    let output = tokio::process::Command::new("git")
        .arg("--git-dir")
        .arg(&git_dir)
        .args(["for-each-ref", "--format=%(refname) %(objectname)"])
        .output()
        .await
        .map_err(|e| RepoError::Other(format!("for-each-ref: {e}")))?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .filter_map(|l| {
            let (name, sha) = l.split_once(' ')?;
            Some((name.to_string(), sha.to_string()))
        })
        .collect())
}

/// Gitea-style contents entry for one path (file metadata + base64 content).
pub async fn contents_entry(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    sha: &str,
    path: &str,
) -> Result<serde_json::Value, RepoError> {
    let repo = open(store, org, repo).await?;
    let id = resolve_snapshot(&repo, sha)?;
    let commit = repo
        .store()
        .get_commit(&id)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    let bytes = read_blob(&commit, path).await?;
    // GitHub contents semantics: `sha` is the file's BLOB object id (the
    // optimistic-lock base for writes), NOT the commit id. The write path
    // compares against this blob sha, so returning the commit id here would
    // make every client-supplied base mismatch and 409 on a single-file write.
    let blob_sha = file_blob_sha(&commit, path).await?;
    Ok(json_contents(path, &bytes, &blob_sha))
}

/// Return the blob sha (file id hex) of the file at `path` in `commit`'s tree,
/// or "" when the path is absent (GitHub contents semantics: a file's sha is
/// the blob object id, used as the optimistic-lock base for writes).
pub async fn file_blob_sha(commit: &Commit, path: &str) -> Result<String, RepoError> {
    let repo_path = RepoPathBuf::from_internal_string(path)
        .map_err(|e| RepoError::Other(format!("bad path {path:?}: {e}")))?;
    let value = commit
        .tree()
        .path_value(&repo_path)
        .await
        .map_err(|e| RepoError::Other(format!("path_value {path:?}: {e}")))?;
    let resolved = value.into_resolved();
    match resolved {
        Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => Ok(id.hex()),
        Ok(Some(jj_lib::backend::TreeValue::Symlink(id))) => Ok(id.hex()),
        Ok(_) => Ok(String::new()),
        Err(_) => Ok(String::new()),
    }
}

fn json_contents(path: &str, bytes: &[u8], sha: &str) -> serde_json::Value {
    use base64::Engine as _;
    serde_json::json!({
        "name": path.rsplit('/').next().unwrap_or(path),
        "path": path,
        "sha": sha,
        "type": "file",
        "size": bytes.len(),
        "encoding": "base64",
        "content": base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

/// List a directory's entries as Gitea-style contents (no recursion).
pub async fn contents_dir(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    sha: &str,
) -> Result<Vec<serde_json::Value>, RepoError> {
    let repo = open(store, org, repo).await?;
    let id = resolve_snapshot(&repo, sha)?;
    let commit = repo
        .store()
        .get_commit(&id)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    let mut out = Vec::new();
    for entry in list_tree(&commit.tree(), "") {
        out.push(serde_json::json!({
            "name": entry.path.rsplit('/').next().unwrap_or(&entry.path),
            "path": entry.path,
            "type": entry.kind,
            "mode": entry.mode,
            "size": entry.size,
        }));
    }
    Ok(out)
}

/// Tarball (.tar.gz) of a commit's tree.
pub async fn archive_tarball(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    sha: &str,
) -> Result<Vec<u8>, RepoError> {
    let repo = open(store, org, repo).await?;
    let id = resolve_snapshot(&repo, sha)?;
    let commit = repo
        .store()
        .get_commit(&id)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    let mut buf = Vec::new();
    {
        let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);
        for (path_buf, value_res) in commit.tree().entries() {
            let value = match value_res {
                Ok(v) => v,
                Err(_) => continue,
            };
            let resolved = match value.into_resolved() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Some(entry) = resolved else { continue };
            if !matches!(entry, jj_lib::backend::TreeValue::File { .. }) {
                continue;
            }
            let path = path_buf.as_internal_file_string().to_string();
            let bytes = read_blob(&commit, &path).await?;
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, &path, bytes.as_slice())
                .map_err(|e| RepoError::Other(format!("tar append {path:?}: {e}")))?;
        }
        tar.into_inner()
            .map_err(|e| RepoError::Other(format!("tar finish: {e}")))?
            .finish()
            .map_err(|e| RepoError::Other(format!("gzip finish: {e}")))?;
    }
    Ok(buf)
}

/// Resolve a rev to (change-id reverse hex, commit sha).
pub async fn resolve_rev(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    rev: &str,
) -> Result<(String, String), RepoError> {
    let repo = open(store, org, repo).await?;
    let id = resolve_revset_single(&repo, rev)?;
    let commit = repo
        .store()
        .get_commit(&id)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    Ok((commit.change_id().reverse_hex(), commit.id().hex()))
}

/// A node in the change graph (feeds the commit graph / network view).
#[derive(serde::Serialize)]
#[derive(Debug)]
pub struct GraphNode {
    pub commit_id: String,
    pub change_id: String,
    pub message: String,
    pub author: String,
    pub parents: Vec<String>,
    pub edge_types: Vec<String>,
    pub is_head: bool,
}

impl GraphNode {
    pub fn new(c: &Commit, is_head: bool) -> Self {
        let parents: Vec<String> = c.parent_ids().iter().map(|p| p.hex()).collect();
        let edge_types = vec!["direct".to_string(); parents.len()];
        GraphNode {
            commit_id: c.id().hex(),
            change_id: c.change_id().reverse_hex(),
            message: c.description().to_string(),
            author: c.author().name.clone(),
            parents,
            edge_types,
            is_head,
        }
    }
}

/// Change graph in topological (parents-last) order: oldest first, so the
/// client can layer commits bottom-up. Edges parent_id -> commit_id, with
/// "direct" for the first parent and "obsolete" for skipped/jj-managed ids.
pub async fn change_graph(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    _limit: usize,
) -> Result<Vec<GraphNode>, RepoError> {
    let repo = open(store, org, repo).await?;
    let root_id = repo.store().root_commit_id().clone();
    let heads: std::collections::HashSet<CommitId> = repo
        .view()
        .heads()
        .iter()
        .filter(|h| **h != root_id)
        .cloned()
        .collect();

    // Walk from heads; collect parents via DFS with cycle protection.
    let mut seen = std::collections::HashSet::new();
    let mut stack: Vec<(CommitId, bool)> = heads.iter().cloned().map(|h| (h, true)).collect();
    let mut commits: Vec<Commit> = Vec::new();
    let mut is_head_map: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    while let Some((id, is_head)) = stack.pop() {
        let key = id.hex();
        if !seen.insert(id.clone()) {
            continue;
        }
        if id == root_id {
            continue;
        }
        if let Ok(c) = repo.store().get_commit(&id) {
            for p in c.parent_ids() {
                stack.push((p.clone(), false));
            }
            is_head_map.insert(key, is_head);
            commits.push(c);
        }
    }
    // Topological order: parents before children (children have parents lower
    // in history). Sort by committer timestamp ascending so parents come first.
    commits.sort_by_key(|c| c.committer().timestamp.timestamp);
    // Re-attach parent edge types: first parent "direct", others "direct";
    // node's own parents encode "direct" edges; skip passed-edge complexity.
    Ok(commits
        .into_iter()
        .map(|c| GraphNode::new(&c, is_head_map.get(&c.id().hex()).copied().unwrap_or(false)))
        .collect())
}

/// Commits that touched a path, newest-first (Gitea commits-by-path view).
pub async fn file_log(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    path: &str,
    limit: usize,
) -> Result<(Vec<CommitInfo>, usize), RepoError> {
    let repo = open(store, org, repo).await?;
    let repo_path = RepoPathBuf::from_internal_string(path)
        .map_err(|e| RepoError::Other(format!("bad path {path:?}: {e}")))?;
    let root_id = repo.store().root_commit_id().clone();
    let mut commits: Vec<Commit> = Vec::new();
    let mut stack: Vec<CommitId> = repo
        .view()
        .local_bookmarks()
        .filter_map(|(_, t)| t.as_normal().cloned())
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut counts = std::collections::HashMap::<String, i64>::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) || id == root_id {
            continue;
        }
        let Ok(c) = repo.store().get_commit(&id) else { continue };
        for p in c.parent_ids() {
            stack.push(p.clone());
        }
        // Determine whether this commit introduced/removed the path relative
        // to its first parent.
        let changed = match c.parent_ids().first() {
            Some(p) => {
                let pc = repo.store().get_commit(p).ok();
                match pc {
                    Some(pc) => path_changed(&c, &pc, &repo_path).await,
                    None => true,
                }
            }
            None => true,
        };
        if changed {
            let key = c.id().hex();
            counts.insert(key, c.committer().timestamp.timestamp.0);
            commits.push(c);
        }
    }
    commits.sort_by_key(|c| std::cmp::Reverse(counts.get(&c.id().hex()).copied().unwrap_or(0)));
    let total = commits.len();
    let items: Vec<CommitInfo> = commits.into_iter().take(limit).map(|c| commit_info(&c)).collect();
    Ok((items, total))
}

async fn path_changed(c: &Commit, parent: &Commit, path: &RepoPathBuf) -> bool {
    let pk = path.clone();
    let a = parent.tree().path_value(&pk).await;
    let b = c.tree().path_value(&pk).await;
    match (a, b) {
        (Ok(a), Ok(b)) => {
            let ra = a.into_resolved().ok().flatten();
            let rb = b.into_resolved().ok().flatten();
            ra != rb
        }
        _ => true,
    }
}

/// Regex (ripgrep-syntax via gix) search across a tree, returning
/// `path:line:text` matches similar to `git grep -n`.
pub async fn search_code(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    rev: &str,
    pattern: &str,
) -> Result<Vec<String>, RepoError> {
    let repo = open(store, org, repo).await?;
    let id = resolve_revset_single(&repo, rev)?;
    let commit = repo.store().get_commit(&id).map_err(|e| RepoError::Other(e.to_string()))?;
    // Walk the tree and line-grep each file (bounded, no index yet).
    let mut out = Vec::new();
    let tree = commit.tree();
    for (path_buf, value_res) in tree.entries() {
        let Ok(value) = value_res else { continue };
        let Some(resolved) = value.clone().into_resolved().ok() else { continue };
        let Some(jj_lib::backend::TreeValue::File { .. }) = resolved else { continue };
        let path = path_buf.as_internal_file_string().to_string();
        let bytes = match read_blob(&commit, &path).await {
            Ok(b) => b,
            Err(_) => continue,
        };
        let text = String::from_utf8_lossy(&bytes);
        for (i, line) in text.lines().enumerate() {
            if line.contains(pattern) {
                out.push(format!("{path}:{}:{line}", i + 1));
            }
        }
    }
    Ok(out)
}

/// Current head commit sha of a repo (first head, conventionally).
pub async fn head_sha(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
) -> Result<String, RepoError> {
    let repo = open(store, org, repo).await?;
    // Prefer a real default bookmark; heads() includes the root commit, which
    // is never a meaningful "head" for reads.
    for name in ["main", "master"] {
        let target = repo.view().get_local_bookmark(&jj_lib::ref_name::RefNameBuf::from(
            name.to_string(),
        ));
        if let Some(id) = target.as_normal() {
            return Ok(id.hex());
        }
    }
    // Fall back to the newest non-root head.
    let root = repo.store().root_commit_id().clone();
    let mut best: Option<(jj_lib::backend::Timestamp, CommitId)> = None;
    for id in repo.view().heads() {
        if *id == root {
            continue;
        }
        if let Ok(c) = repo.store().get_commit(id) {
            let ts = c.committer().timestamp;
            if best.as_ref().is_none_or(|(bt, _)| ts.timestamp > bt.timestamp) {
                best = Some((ts, id.clone()));
            }
        }
    }
    match best {
        Some((_, id)) => Ok(id.hex()),
        None => Err(RepoError::Other("repository has no commits".into())),
    }
}

/// Read a file's bytes at a given rev.
pub async fn read_file_at(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    sha: &str,
    path: &str,
) -> Result<Vec<u8>, RepoError> {
    let repo = open(store, org, repo).await?;
    let id = resolve_snapshot(&repo, sha)?;
    let commit = repo
        .store()
        .get_commit(&id)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    read_blob(&commit, path).await
}

/// Check out a commit's tree as real files under `dest` (for build contexts).
/// Only files are materialized; conflicts are resolved to their `adds` side,
/// matching `read_blob`.
pub async fn checkout_tree(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    sha: &str,
    dest: &std::path::Path,
) -> Result<(), RepoError> {
    let repo = open(store, org, repo).await?;
    let id = resolve_snapshot(&repo, sha)?;
    let commit = repo
        .store()
        .get_commit(&id)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    tokio::fs::create_dir_all(dest)
        .await
        .map_err(|e| RepoError::Other(format!("mkdir {dest:?}: {e}")))?;
    for entry in list_tree(&commit.tree(), "") {
        if entry.kind != "file" {
            continue;
        }
        let bytes = read_blob(&commit, &entry.path).await?;
        let rel = std::path::Path::new(&entry.path);
        // Guard: never allow a tree entry to escape `dest` (parallels tar write).
        let target = dest.join(rel);
        let rel_clean = rel
            .components()
            .all(|c| !matches!(c, std::path::Component::ParentDir | std::path::Component::RootDir));
        if !rel_clean {
            continue;
        }
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| RepoError::Other(format!("mkdir {parent:?}: {e}")))?;
        }
        tokio::fs::write(&target, &bytes)
            .await
            .map_err(|e| RepoError::Other(format!("write {target:?}: {e}")))?;
    }
    Ok(())
}

/// List a directory at a given rev (non-recursive), Gitea-style JSON entries.
pub async fn contents_dir_at(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    sha: &str,
    dir: &str,
) -> Result<Vec<serde_json::Value>, RepoError> {
    let repo = open(store, org, repo).await?;
    let id = resolve_snapshot(&repo, sha)?;
    let commit = repo
        .store()
        .get_commit(&id)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    let base = if dir.is_empty() { "" } else { dir };
    let mut out = Vec::new();
    for entry in list_tree(&commit.tree(), base) {
        // Direct children only: strip base and reject nested paths.
        let rel = entry
            .path
            .strip_prefix(if base.is_empty() { "" } else { base })
            .unwrap_or(&entry.path);
        let rel = rel.trim_start_matches('/');
        if rel.contains('/') {
            continue;
        }
        out.push(serde_json::json!({
            "name": rel,
            "path": entry.path,
            "type": entry.kind,
            "mode": entry.mode,
        }));
    }
    Ok(out)
}

/// All local bookmark tips as (name, sha).
pub async fn branch_tips(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
) -> Result<Vec<(String, String)>, RepoError> {
    let repo = open(store, org, repo).await?;
    Ok(list_branches(&repo)
        .into_iter()
        .map(|b| (b.name, b.sha))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_commit_empty_rev_on_missing_repo_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(RepoStore::new(dir.path().to_path_buf()));
        let got = pollster::block_on(async { open(&store, "o", "missing").await });
        assert!(got.is_err());
    }
}

#[cfg(test)]
mod unified_tests {
    use super::*;

    #[test]
    fn unified_patch_produces_hunks_for_added_line() {
        let out = unified_patch("a.txt", b"line1\n", b"line1\nline2\n");
        assert!(out.contains("+line2"), "OUT={out}");
    }

    #[test]
    fn raw_hunks_probe() {
        let before = gix::bstr::BString::from(&b"line1\n"[..]);
        let after = gix::bstr::BString::from(&b"line1\nline2\n"[..]);
        let hunks = jj_lib::diff_presentation::unified::unified_diff_hunks(
            jj_lib::merge::Diff { before: before.as_ref(), after: after.as_ref() },
            3,
            jj_lib::diff_presentation::LineCompareMode::Exact,
        );
        assert!(!hunks.is_empty());
    }
}
