//! Mutations (write surface): repo lifecycle, bookmarks, tags, and
//! file-content edits. Every content edit produces a new commit on top of the
//! target, carrying the change-id of the change it amends (or a fresh one) —
//! this is the change-centric core: editing a file is creating a change.

use std::sync::Arc;

use jj_lib::backend::{CopyId, TreeValue};
use jj_lib::merge::Merge;
use jj_lib::merged_tree_builder::MergedTreeBuilder;
use jj_lib::object_id::ObjectId as _;
use jj_lib::repo::Repo as _;
use jj_lib::repo_path::RepoPathBuf;
use jj_lib::tree_builder::TreeBuilder;

use jjlab_core::Db;

use crate::project;
use crate::repo::{RepoError, RepoStore};

/// Return the blob sha (file id hex) of the file at `path` in `commit`'s tree,
/// or None when the path is absent (GitHub contents semantics: a file's sha is
/// the blob object id, used as the optimistic-lock base for writes).
async fn file_blob_sha(commit: &jj_lib::commit::Commit, path: &str) -> Result<Option<String>, RepoError> {
    let repo_path = RepoPathBuf::from_internal_string(path)
        .map_err(|e| RepoError::Other(format!("bad path {path:?}: {e}")))?;
    let value = commit
        .tree()
        .path_value(&repo_path)
        .await
        .map_err(|e| RepoError::Other(format!("path_value {path:?}: {e}")))?;
    let resolved = value.into_resolved();
    match resolved {
        Ok(Some(TreeValue::File { id, .. })) => Ok(Some(id.hex())),
        Ok(Some(TreeValue::Symlink(id))) => Ok(Some(id.hex())),
        Ok(_) => Ok(None),
        Err(_) => Ok(None),
    }
}

/// Enforce the optimistic-lock base for a single file edit. `base_sha` is the
/// file's blob sha the client read earlier. When `base_sha` is Some and
/// non-empty but the current file state does not equal it exactly (whether the
/// file was concurrently modified OR is now absent), this rejects with a
/// 409-style Conflict — a stale edit is refused, never silently overwritten or
/// turned into a blind create.
fn assert_blob_base(
    current: Option<String>,
    base_sha: Option<&str>,
) -> Result<(), RepoError> {
    if let Some(base) = base_sha {
        if base.is_empty() {
            return Ok(());
        }
        if current.as_deref() != Some(base) {
            return Err(RepoError::Conflict(format!(
                "file blob changed concurrently (base {base}, current {:?})",
                current
            )));
        }
    }
    Ok(())
}

/// One file edit in an atomic batch.
pub struct BatchEdit {
    pub path: String,
    pub content: Vec<u8>,
    pub base_sha: Option<String>,
}

/// One file deletion in an atomic batch.
pub struct BatchDelete {
    pub path: String,
    pub base_sha: Option<String>,
}


/// Outcome of a rebase: the dest tip commit, its change-id, and any paths that
/// still conflict after the native (conflict-tolerant) rebase.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RebaseOutcome {
    pub commit_id: String,
    pub change_id: String,
    pub conflicts: Vec<String>,
}

/// Rebase the source snapshot's commits onto the dest snapshot, advancing the
/// dest bookmark. jj-native: the rebase never "stops" on conflicts — conflicts
/// are carried as first-class tree conflicts in the resulting commit and
/// reported as paths.
pub async fn rebase_bookmark(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    source: &str,
    dest: &str,
) -> Result<RebaseOutcome, RepoError> {
    let handle = store.open(org, repo).await?;
    let source_id = pollster::block_on(async {
        let arc = handle.repo.clone();
        crate::read::resolve_snapshot(&arc, source)
    })?;
    let dest_id = pollster::block_on(async {
        let arc = handle.repo.clone();
        crate::read::resolve_snapshot(&arc, dest)
    })?;

    let tip = pollster::block_on(async {
        let mut tx = handle.repo.start_transaction();
        let tip = {
            let source_commit = tx
                .repo()
                .store()
                .get_commit(&source_id)
                .map_err(|e| RepoError::Other(e.to_string()))?;
            let mut_repo = tx.repo_mut();
            let rebased = jj_lib::rewrite::rebase_commit(
                mut_repo,
                source_commit,
                vec![dest_id.clone()],
            )
            .await
            .map_err(|e| RepoError::Other(format!("rebase commit: {e}")))?;
            // Rebase everything that sat on top of the old source (bookmarks,
            // the working-copy commit, descendants) onto the rebased commit.
            mut_repo
                .rebase_descendants()
                .await
                .map_err(|e| RepoError::Other(format!("rebase descendants: {e}")))?;
            // Move the dest bookmark onto the rebased commit.
            let target: jj_lib::ref_name::RefNameBuf = dest.to_string().into();
            mut_repo.set_local_bookmark_target(
                &target,
                jj_lib::op_store::RefTarget::normal(rebased.id().clone()),
            );
            rebased.id().clone()
        };
        tx.commit(&format!("jjlab: rebase {source} onto {dest}"))
            .await
            .map_err(|e| RepoError::Other(e.to_string()))?;
        Ok::<_, RepoError>(tip)
    })?;

    project::project_repo(store, db, org, repo).await?;

    let handle = store.open(org, repo).await?;
    let commit = handle
        .repo
        .store()
        .get_commit(&tip)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    let mut conflicts = Vec::new();
    for (path, _) in commit.tree().conflicts() {
        conflicts.push(path.as_internal_file_string().to_string());
    }
    conflicts.sort();
    Ok(RebaseOutcome {
        commit_id: commit.id().hex(),
        change_id: commit.change_id().reverse_hex(),
        conflicts,
    })
}

/// Create an empty repo with a README (rucoder-neo semantics).
pub async fn create_repo(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    default_bookmark: &str,
    author: (String, String),
) -> Result<(), RepoError> {
    jjlab_core::validate_segment(org, "org").map_err(RepoError::Invalid)?;
    jjlab_core::validate_segment(repo, "repo").map_err(RepoError::Invalid)?;
    jjlab_core::validate_ref_name(default_bookmark, "bookmark").map_err(RepoError::Invalid)?;
    if store.exists(org, repo) {
        return Err(RepoError::Conflict(format!("repository {org}/{repo} already exists")));
    }
    db.upsert_org(org, org)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    db.upsert_repo(&format!("{org}/{repo}"), org, repo, default_bookmark, None)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    let handle = store.open_or_init(org, repo).await?;
    let signature = jj_lib::backend::Signature {
        name: author.0,
        email: author.1,
        timestamp: jj_lib::backend::Timestamp::now(),
    };

    pollster::block_on(async {
        let mut tx = handle.repo.start_transaction();
        {
            let mut_repo = tx.repo_mut();
            let store_ = mut_repo.store().clone();
            let readme = store_
                .write_file(&RepoPathBuf::root(), &mut format!("# {repo}\n").as_bytes())
                .await
                .map_err(|e| RepoError::Other(format!("write README: {e}")))?;

            let mut builder = TreeBuilder::new(store_.clone(), store_.empty_tree_id().clone());
            let readme_path = RepoPathBuf::from_internal_string("README.md")
                .map_err(|e| RepoError::Other(format!("bad path: {e}")))?;
            builder.set(
                readme_path,
                TreeValue::File {
                    id: readme,
                    executable: false,
                    copy_id: CopyId::placeholder(),
                },
            );
            let tree_id = builder
                .write_tree()
                .await
                .map_err(|e| RepoError::Other(format!("write tree: {e}")))?;
            let tree = jj_lib::merged_tree::MergedTree::resolved(store_, tree_id);

            let parent = mut_repo.store().root_commit_id().clone();
            let commit = mut_repo
                .new_commit(vec![parent], tree)
                .set_description("initial commit".to_string())
                .set_author(signature.clone())
                .set_committer(signature.clone())
                .write()
                .await
                .map_err(|e| RepoError::Other(format!("write commit: {e}")))?;

            let name: jj_lib::ref_name::RefNameBuf = default_bookmark.to_string().into();
            mut_repo.set_local_bookmark_target(
                &name,
                jj_lib::op_store::RefTarget::normal(commit.id().clone()),
            );
        }
        tx.commit("jjlab: init")
            .await
            .map_err(|e| RepoError::Other(e.to_string()))?;
        Ok::<(), RepoError>(())
    })?;
    project::project_repo(store, db, org, repo).await
}

/// Delete a repo directory.
pub async fn delete_repo(store: &Arc<RepoStore>, org: &str, repo: &str) -> Result<(), RepoError> {
    if !store.exists(org, repo) {
        return Err(RepoError::NotFound {
            org: org.to_string(),
            repo: repo.to_string(),
        });
    }
    // Release MR GC anchors before the git dir is torn down.
    if let Err(e) = crate::mr_anchor::clear_repo_mr_heads(store, org, repo).await {
        tracing::warn!(err = %e, org, repo, "clear MR anchors on repo delete failed");
    }
    let dir = store.repo_dir_checked(org, repo)?;
    tokio::fs::remove_dir_all(&dir)
        .await
        .map_err(|e| RepoError::Other(format!("delete {dir:?}: {e}")))?;
    Ok(())
}

/// Move a bookmark (git branch) to a snapshot or change-id view. `target`
/// (snapshot: sha/bookmark/tag) and `change` (change-id) are mutually
/// exclusive; exactly one must be non-empty.
pub async fn set_bookmark(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    name: &str,
    target: &str,
    change: &str,
) -> Result<String, RepoError> {
    jjlab_core::validate_ref_name(name, "bookmark").map_err(RepoError::Invalid)?;
    let handle = store.open(org, repo).await?;
    let commit_id = pollster::block_on(async {
        let repo_arc = handle.repo.clone();
        crate::read::resolve_target_or_change(&repo_arc, target, change)
    })?;

    pollster::block_on(async {
        let mut tx = handle.repo.start_transaction();
        {
            let mut_repo = tx.repo_mut();
            let target: jj_lib::ref_name::RefNameBuf = name.to_string().into();
            mut_repo.set_local_bookmark_target(
                &target,
                jj_lib::op_store::RefTarget::normal(commit_id.clone()),
            );
        }
        tx.commit(&format!("jjlab: set bookmark {name}"))
            .await
            .map_err(|e| RepoError::Other(e.to_string()))?;
        Ok::<(), RepoError>(())
    })?;

    project::project_repo(store, db, org, repo).await?;
    Ok(commit_id.hex())
}

/// Delete a bookmark.
pub async fn delete_bookmark(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    name: &str,
) -> Result<(), RepoError> {
    jjlab_core::validate_ref_name(name, "bookmark").map_err(RepoError::Invalid)?;
    let handle = store.open(org, repo).await?;
    pollster::block_on(async {
        let mut tx = handle.repo.start_transaction();
        {
            let mut_repo = tx.repo_mut();
            let target: jj_lib::ref_name::RefNameBuf = name.to_string().into();
            mut_repo.set_local_bookmark_target(&target, jj_lib::op_store::RefTarget::absent());
        }
        tx.commit(&format!("jjlab: delete bookmark {name}"))
            .await
            .map_err(|e| RepoError::Other(e.to_string()))?;
        Ok::<(), RepoError>(())
    })?;
    project::project_repo(store, db, org, repo).await
}

/// Create/move a tag to a snapshot or change-id view. `target` (snapshot:
/// sha/bookmark/tag) and `change` (change-id) are mutually exclusive.
pub async fn set_tag(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    name: &str,
    target: &str,
    change: &str,
) -> Result<String, RepoError> {
    jjlab_core::validate_ref_name(name, "tag").map_err(RepoError::Invalid)?;
    let handle = store.open(org, repo).await?;
    let commit_id = pollster::block_on(async {
        let repo_arc = handle.repo.clone();
        crate::read::resolve_target_or_change(&repo_arc, target, change)
    })?;
    pollster::block_on(async {
        let mut tx = handle.repo.start_transaction();
        {
            let mut_repo = tx.repo_mut();
            let target: jj_lib::ref_name::RefNameBuf = name.to_string().into();
            mut_repo.set_local_tag_target(
                &target,
                jj_lib::op_store::RefTarget::normal(commit_id.clone()),
            );
        }
        tx.commit(&format!("jjlab: set tag {name}"))
            .await
            .map_err(|e| RepoError::Other(e.to_string()))?;
        Ok::<(), RepoError>(())
    })?;
    project::project_repo(store, db, org, repo).await?;
    Ok(commit_id.hex())
}

/// Delete a tag.
pub async fn delete_tag(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    name: &str,
) -> Result<(), RepoError> {
    jjlab_core::validate_ref_name(name, "tag").map_err(RepoError::Invalid)?;
    let handle = store.open(org, repo).await?;
    pollster::block_on(async {
        let mut tx = handle.repo.start_transaction();
        {
            let mut_repo = tx.repo_mut();
            let target: jj_lib::ref_name::RefNameBuf = name.to_string().into();
            mut_repo.set_local_tag_target(&target, jj_lib::op_store::RefTarget::absent());
        }
        tx.commit(&format!("jjlab: delete tag {name}"))
            .await
            .map_err(|e| RepoError::Other(e.to_string()))?;
        Ok::<(), RepoError>(())
    })?;
    project::project_repo(store, db, org, repo).await
}

/// Outcome of a content edit: the new commit (git sha) and its change-id.
pub struct EditOutcome {
    pub sha: String,
    pub change_id: String,
}

/// Atomically apply a mixed set of create/update/delete actions as ONE change
/// on top of `bookmark`'s head (the unified write path, GitLab commits style).
/// Every action's base blob sha is validated up front; a mismatch on ANY
/// action rejects the WHOLE commit (409) before anything is written. Writes
/// and deletions are combined into a single `MergedTreeBuilder` → one commit →
/// one change id. Either all succeed or nothing is written (atomic).
#[allow(clippy::too_many_arguments)]
pub async fn commit_edits(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    bookmark: &str,
    writes: &[BatchEdit],
    deletes: &[BatchDelete],
    message: &str,
    author: (String, String),
    amend: bool,
) -> Result<EditOutcome, RepoError> {
    jjlab_core::validate_ref_name(bookmark, "bookmark").map_err(RepoError::Invalid)?;
    if writes.is_empty() && deletes.is_empty() {
        return Err(RepoError::Invalid("commit contains no actions".into()));
    }
    let handle = store.open(org, repo).await?;
    let signature = jj_lib::backend::Signature {
        name: author.0,
        email: author.1,
        timestamp: jj_lib::backend::Timestamp::now(),
    };
    let repo_arc = handle.repo.clone();
    let base = pollster::block_on(async { crate::read::resolve_snapshot(&repo_arc, bookmark) })?;

    let outcome = pollster::block_on(async {
        let mut tx = handle.repo.start_transaction();
        let outcome = {
            let mut_repo = tx.repo_mut();
            let store_ = mut_repo.store().clone();
            let parent = mut_repo
                .store()
                .get_commit(&base)
                .map_err(|e| RepoError::Other(e.to_string()))?;

            // Validate every base sha BEFORE mutating anything: a mismatch on
            // any action rejects the whole commit (atomic, no partial write).
            for edit in writes {
                let current = file_blob_sha(&parent, &edit.path).await?;
                assert_blob_base(current, edit.base_sha.as_deref())?;
            }
            for del in deletes {
                let current = file_blob_sha(&parent, &del.path).await?;
                assert_blob_base(current, del.base_sha.as_deref())?;
            }

            let mut builder = MergedTreeBuilder::new(parent.tree());
            for edit in writes {
                let repo_path = RepoPathBuf::from_internal_string(&edit.path)
                    .map_err(|e| RepoError::Other(format!("bad path {:?}: {e}", edit.path)))?;
                if edit.content.is_empty() {
                    builder.set_or_remove(repo_path, Merge::absent());
                    continue;
                }
                let file_id = store_
                    .write_file(&RepoPathBuf::root(), &mut futures_util::io::Cursor::new(edit.content.clone()))
                    .await
                    .map_err(|e| RepoError::Other(format!("write file {}: {e}", edit.path)))?;
                builder.set_or_remove(
                    repo_path,
                    Merge::normal(TreeValue::File {
                        id: file_id,
                        executable: false,
                        copy_id: CopyId::placeholder(),
                    }),
                );
            }
            for del in deletes {
                let repo_path = RepoPathBuf::from_internal_string(&del.path)
                    .map_err(|e| RepoError::Other(format!("bad path {:?}: {e}", del.path)))?;
                builder.set_or_remove(repo_path, Merge::absent());
            }
            let merged = builder
                .write_tree()
                .await
                .map_err(|e| RepoError::Other(format!("write tree: {e}")))?;

            let can_amend = amend && base != mut_repo.store().root_commit_id().clone();
            let commit = if can_amend {
                let mut b = mut_repo.rewrite_commit(&parent);
                b = b.set_tree(merged).set_description(message.to_string());
                let commit = b
                    .write()
                    .await
                    .map_err(|e| RepoError::Other(format!("rewrite commit: {e}")))?;
                mut_repo
                    .rebase_descendants()
                    .await
                    .map_err(|e| RepoError::Other(format!("rebase descendants: {e}")))?;
                commit
            } else {
                let commit = mut_repo
                    .new_commit(vec![base.clone()], merged)
                    .set_description(message.to_string())
                    .set_author(signature.clone())
                    .set_committer(signature.clone())
                    .write()
                    .await
                    .map_err(|e| RepoError::Other(format!("write commit: {e}")))?;
                let target: jj_lib::ref_name::RefNameBuf = bookmark.to_string().into();
                mut_repo.set_local_bookmark_target(
                    &target,
                    jj_lib::op_store::RefTarget::normal(commit.id().clone()),
                );
                commit
            };

            EditOutcome {
                sha: commit.id().hex(),
                change_id: commit.change_id().reverse_hex(),
            }
        };
        tx.commit("jjlab: commit edits")
            .await
            .map_err(|e| RepoError::Other(e.to_string()))?;
        Ok::<EditOutcome, RepoError>(outcome)
    })?;

    project::project_repo(store, db, org, repo).await?;
    // Writing a workflow file syncs it and (per push semantics) enqueues runs.
    if hasWorkflowWrite(writes) {
        let logs_root = std::path::PathBuf::from(
            std::env::var("JJLAB_LOGS").unwrap_or_else(|_| "/data/logs".to_string()),
        );
        if let Err(e) = crate::actions::on_push(store, db, org, repo, &outcome.sha, &logs_root).await {
            tracing::warn!(org, repo, err = %e, "actions on_push after workflow commit failed");
        }
    }
    Ok(outcome)
}

// hasWorkflowWrite reports whether any write action targets a workflow file
// (which, per push semantics, triggers a CI run after the change).
fn hasWorkflowWrite(writes: &[BatchEdit]) -> bool {
    writes.iter().any(|e| {
        e.path.starts_with(".github/workflows/") || e.path == ".jjlab-ci.yml"
    })
}

/// Atomically write (create/update) several files as one change on top of
/// `bookmark`'s head. `edits` carries per-file content + an optional optimistic
/// base blob sha. Every edit's base sha is validated up front; a mismatch on
/// ANY file rejects the WHOLE batch with a Conflict before anything is written.
/// The files land in a single `MergedTreeBuilder`, produce one commit, and one
/// change id. Either all succeed or nothing is written (atomic).
#[allow(clippy::too_many_arguments)]
pub async fn write_files(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    bookmark: &str,
    edits: &[BatchEdit],
    message: &str,
    author: (String, String),
    amend: bool,
) -> Result<EditOutcome, RepoError> {
    jjlab_core::validate_ref_name(bookmark, "bookmark").map_err(RepoError::Invalid)?;
    if edits.is_empty() {
        return Err(RepoError::Invalid("batch contains no files".into()));
    }
    let handle = store.open(org, repo).await?;
    let signature = jj_lib::backend::Signature {
        name: author.0,
        email: author.1,
        timestamp: jj_lib::backend::Timestamp::now(),
    };
    let repo_arc = handle.repo.clone();
    let base = pollster::block_on(async { crate::read::resolve_snapshot(&repo_arc, bookmark) })?;

    let outcome = pollster::block_on(async {
        let mut tx = handle.repo.start_transaction();
        let outcome = {
            let mut_repo = tx.repo_mut();
            let store_ = mut_repo.store().clone();
            let parent = mut_repo
                .store()
                .get_commit(&base)
                .map_err(|e| RepoError::Other(e.to_string()))?;

            // Validate every base sha BEFORE mutating anything: a mismatch on
            // any file rejects the whole batch (atomic, no partial write).
            for edit in edits {
                let current = file_blob_sha(&parent, &edit.path).await?;
                assert_blob_base(current, edit.base_sha.as_deref())?;
            }

            let mut builder = MergedTreeBuilder::new(parent.tree());
            for edit in edits {
                let repo_path = RepoPathBuf::from_internal_string(&edit.path)
                    .map_err(|e| RepoError::Other(format!("bad path {:?}: {e}", edit.path)))?;
                // New file (content), or delete when content is empty & base is
                // Some (the client deletes by supplying an empty body with its
                // known blob sha). Simpler: content as file; empty body => absent.
                if edit.content.is_empty() {
                    builder.set_or_remove(repo_path, Merge::absent());
                    continue;
                }
                let file_id = store_
                    .write_file(&RepoPathBuf::root(), &mut futures_util::io::Cursor::new(edit.content.clone()))
                    .await
                    .map_err(|e| RepoError::Other(format!("write file {}: {e}", edit.path)))?;
                builder.set_or_remove(
                    repo_path,
                    Merge::normal(TreeValue::File {
                        id: file_id,
                        executable: false,
                        copy_id: CopyId::placeholder(),
                    }),
                );
            }
            let merged = builder
                .write_tree()
                .await
                .map_err(|e| RepoError::Other(format!("write tree: {e}")))?;

            let can_amend = amend && base != mut_repo.store().root_commit_id().clone();
            let commit = if can_amend {
                let mut b = mut_repo.rewrite_commit(&parent);
                b = b.set_tree(merged).set_description(message.to_string());
                let commit = b
                    .write()
                    .await
                    .map_err(|e| RepoError::Other(format!("rewrite commit: {e}")))?;
                mut_repo
                    .rebase_descendants()
                    .await
                    .map_err(|e| RepoError::Other(format!("rebase descendants: {e}")))?;
                commit
            } else {
                let commit = mut_repo
                    .new_commit(vec![base.clone()], merged)
                    .set_description(message.to_string())
                    .set_author(signature.clone())
                    .set_committer(signature.clone())
                    .write()
                    .await
                    .map_err(|e| RepoError::Other(format!("write commit: {e}")))?;
                let target: jj_lib::ref_name::RefNameBuf = bookmark.to_string().into();
                mut_repo.set_local_bookmark_target(
                    &target,
                    jj_lib::op_store::RefTarget::normal(commit.id().clone()),
                );
                commit
            };

            EditOutcome {
                sha: commit.id().hex(),
                change_id: commit.change_id().reverse_hex(),
            }
        };
        tx.commit("jjlab: write files")
            .await
            .map_err(|e| RepoError::Other(e.to_string()))?;
        Ok::<EditOutcome, RepoError>(outcome)
    })?;

    project::project_repo(store, db, org, repo).await?;
    Ok(outcome)
}

/// Atomically delete several files as one change on top of `bookmark`'s head.
/// Every delete's base blob sha is validated up front; a mismatch on ANY file
/// rejects the WHOLE batch (409) before anything is removed. All removals land
/// in a single `MergedTreeBuilder`, produce one commit, and one change id.
#[allow(clippy::too_many_arguments)]
pub async fn delete_files(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    bookmark: &str,
    deletes: &[BatchDelete],
    message: &str,
    author: (String, String),
    amend: bool,
) -> Result<EditOutcome, RepoError> {
    jjlab_core::validate_ref_name(bookmark, "bookmark").map_err(RepoError::Invalid)?;
    if deletes.is_empty() {
        return Err(RepoError::Invalid("batch contains no files".into()));
    }
    let handle = store.open(org, repo).await?;
    let signature = jj_lib::backend::Signature {
        name: author.0,
        email: author.1,
        timestamp: jj_lib::backend::Timestamp::now(),
    };
    let repo_arc = handle.repo.clone();
    let base = pollster::block_on(async { crate::read::resolve_snapshot(&repo_arc, bookmark) })?;

    let outcome = pollster::block_on(async {
        let mut tx = handle.repo.start_transaction();
        let outcome = {
            let mut_repo = tx.repo_mut();
            let parent = mut_repo
                .store()
                .get_commit(&base)
                .map_err(|e| RepoError::Other(e.to_string()))?;

            for del in deletes {
                let current = file_blob_sha(&parent, &del.path).await?;
                assert_blob_base(current, del.base_sha.as_deref())?;
            }

            let mut builder = MergedTreeBuilder::new(parent.tree());
            for del in deletes {
                let repo_path = RepoPathBuf::from_internal_string(&del.path)
                    .map_err(|e| RepoError::Other(format!("bad path {:?}: {e}", del.path)))?;
                builder.set_or_remove(repo_path, Merge::absent());
            }
            let merged = builder
                .write_tree()
                .await
                .map_err(|e| RepoError::Other(format!("write tree: {e}")))?;

            let can_amend = amend && base != mut_repo.store().root_commit_id().clone();
            let commit = if can_amend {
                let mut b = mut_repo.rewrite_commit(&parent);
                b = b.set_tree(merged).set_description(message.to_string());
                let commit = b
                    .write()
                    .await
                    .map_err(|e| RepoError::Other(format!("rewrite commit: {e}")))?;
                mut_repo
                    .rebase_descendants()
                    .await
                    .map_err(|e| RepoError::Other(format!("rebase descendants: {e}")))?;
                commit
            } else {
                let commit = mut_repo
                    .new_commit(vec![base.clone()], merged)
                    .set_description(message.to_string())
                    .set_author(signature.clone())
                    .set_committer(signature.clone())
                    .write()
                    .await
                    .map_err(|e| RepoError::Other(format!("write commit: {e}")))?;
                let target: jj_lib::ref_name::RefNameBuf = bookmark.to_string().into();
                mut_repo.set_local_bookmark_target(
                    &target,
                    jj_lib::op_store::RefTarget::normal(commit.id().clone()),
                );
                commit
            };

            EditOutcome {
                sha: commit.id().hex(),
                change_id: commit.change_id().reverse_hex(),
            }
        };
        tx.commit("jjlab: delete files")
            .await
            .map_err(|e| RepoError::Other(e.to_string()))?;
        Ok::<EditOutcome, RepoError>(outcome)
    })?;

    project::project_repo(store, db, org, repo).await?;
    Ok(outcome)
}
