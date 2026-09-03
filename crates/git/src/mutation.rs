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
/// file's blob sha the client read earlier. When the target path already
/// exists and `base_sha` is Some but differs from the current blob sha, this
/// rejects with a 409-style Conflict so a stale edit is refused rather than
/// silently overwriting a concurrent change.
fn assert_blob_base(
    current: Option<String>,
    base_sha: Option<&str>,
) -> Result<(), RepoError> {
    if let Some(base) = base_sha {
        if base.is_empty() {
            return Ok(());
        }
        if let Some(cur) = current {
            if cur != base {
                return Err(RepoError::Conflict(format!(
                    "file blob changed concurrently (base {base}, current {cur})"
                )));
            }
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
pub async fn rebase_branch(
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
    default_branch: &str,
    author: (String, String),
) -> Result<(), RepoError> {
    jjlab_core::validate_segment(org, "org").map_err(RepoError::Invalid)?;
    jjlab_core::validate_segment(repo, "repo").map_err(RepoError::Invalid)?;
    jjlab_core::validate_ref_name(default_branch, "branch").map_err(RepoError::Invalid)?;
    if store.exists(org, repo) {
        return Err(RepoError::Conflict(format!("repository {org}/{repo} already exists")));
    }
    db.upsert_org(org, org)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    db.upsert_repo(&format!("{org}/{repo}"), org, repo, default_branch, None)
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

            let name: jj_lib::ref_name::RefNameBuf = default_branch.to_string().into();
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
    let dir = store.repo_dir_checked(org, repo)?;
    tokio::fs::remove_dir_all(&dir)
        .await
        .map_err(|e| RepoError::Other(format!("delete {dir:?}: {e}")))?;
    Ok(())
}

/// Move a bookmark (git branch) to a snapshot or change-id view. `target`
/// (snapshot: sha/bookmark/tag) and `change` (change-id) are mutually
/// exclusive; exactly one must be non-empty.
pub async fn set_branch(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    name: &str,
    target: &str,
    change: &str,
) -> Result<String, RepoError> {
    jjlab_core::validate_ref_name(name, "branch").map_err(RepoError::Invalid)?;
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
pub async fn delete_branch(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    name: &str,
) -> Result<(), RepoError> {
    jjlab_core::validate_ref_name(name, "branch").map_err(RepoError::Invalid)?;
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

/// Write (create/update) a file at `path` on top of `branch`'s head.
///
/// `amend` selects the change semantics:
///   - `amend = true` (default): rewrite the branch's head change (jj-native
///     amend), keeping its change-id stable. The head commit is *rewritten*
///     (its predecessor is recorded, so the change stays a single commit in
///     visible history) rather than extended with a same-change-id child.
///   - `amend = false`: create a fresh change on top of the head.
///
/// Amend only ever targets the head: a change becomes immutable once it is no
/// longer the branch tip.
#[allow(clippy::too_many_arguments)]
pub async fn write_file(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    branch: &str,
    path: &str,
    mut content: &[u8],
    message: &str,
    author: (String, String),
    amend: bool,
    base_sha: Option<&str>,
) -> Result<EditOutcome, RepoError> {
    jjlab_core::validate_ref_name(branch, "branch").map_err(RepoError::Invalid)?;
    let handle = store.open(org, repo).await?;
    let signature = jj_lib::backend::Signature {
        name: author.0,
        email: author.1,
        timestamp: jj_lib::backend::Timestamp::now(),
    };
    let repo_arc = handle.repo.clone();
    let base = pollster::block_on(async { crate::read::resolve_snapshot(&repo_arc, branch) })?;

    let outcome = pollster::block_on(async {
        let mut tx = handle.repo.start_transaction();
        let outcome = {
            let mut_repo = tx.repo_mut();
            let store_ = mut_repo.store().clone();

            // Optimistic-lock: reject a stale edit whose base blob sha no
            // longer matches the file's current blob sha (concurrent change).
            let parent = mut_repo
                .store()
                .get_commit(&base)
                .map_err(|e| RepoError::Other(e.to_string()))?;
            let current = file_blob_sha(&parent, path).await?;
            assert_blob_base(current, base_sha)?;

            // Start from the head's merged tree and set the single path. Using
            // MergedTreeBuilder (not a resolved TreeBuilder) means any other
            // first-class conflicts in the head tree survive: writing a file's
            // content is exactly how a conflict at `path` gets resolved, while
            // unrelated conflicts stay conflicted.
            let file_id = store_
                .write_file(&RepoPathBuf::root(), &mut content)
                .await
                .map_err(|e| RepoError::Other(format!("write file: {e}")))?;
            let repo_path = RepoPathBuf::from_internal_string(path)
                .map_err(|e| RepoError::Other(format!("bad path {path:?}: {e}")))?;
            let mut builder = MergedTreeBuilder::new(parent.tree());
            builder.set_or_remove(
                repo_path,
                Merge::normal(TreeValue::File {
                    id: file_id,
                    executable: false,
                    copy_id: CopyId::placeholder(),
                }),
            );
            let merged = builder
                .write_tree()
                .await
                .map_err(|e| RepoError::Other(format!("write tree: {e}")))?;

            // Amend = rewrite the head commit (change stays immutable: one
            // commit per change in visible history). A root/empty head has no
            // predecessor to rewrite, so fall back to a fresh change.
            let can_amend = amend && base != mut_repo.store().root_commit_id().clone();
            let commit = if can_amend {
                let mut builder = mut_repo.rewrite_commit(&parent);
                builder = builder
                    .set_tree(merged)
                    .set_description(message.to_string());
                let commit = builder
                    .write()
                    .await
                    .map_err(|e| RepoError::Other(format!("rewrite commit: {e}")))?;
                // rebase_descendants rewrites any descendants of the old head
                // and moves bookmarks pointing at it onto the new commit.
                mut_repo
                    .rebase_descendants()
                    .await
                    .map_err(|e| RepoError::Other(format!("rebase descendants: {e}")))?;
                commit
            } else {
                let mut builder = mut_repo.new_commit(vec![base.clone()], merged);
                builder = builder
                    .set_description(message.to_string())
                    .set_author(signature.clone())
                    .set_committer(signature.clone());
                let commit = builder
                    .write()
                    .await
                    .map_err(|e| RepoError::Other(format!("write commit: {e}")))?;
                let target: jj_lib::ref_name::RefNameBuf = branch.to_string().into();
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
        tx.commit("jjlab: write file")
            .await
            .map_err(|e| RepoError::Other(e.to_string()))?;
        Ok::<EditOutcome, RepoError>(outcome)
    })?;

    project::project_repo(store, db, org, repo).await?;
    // Writing a workflow file syncs it and (per push semantics) enqueues runs.
    if path.starts_with(".github/workflows/") || path == ".jjlab-ci.yml" {
        let logs_root = std::path::PathBuf::from(
            std::env::var("JJLAB_LOGS").unwrap_or_else(|_| "/data/logs".to_string()),
        );
        if let Err(e) =
            crate::actions::on_push(store, db, org, repo, &outcome.sha, &logs_root).await
        {
            tracing::warn!(org, repo, err = %e, "actions on_push after workflow write failed");
        }
    }
    Ok(outcome)
}

/// Delete a file at `path` on top of `branch`. `amend` selects whether to
/// rewrite the branch's head change (`true`, stable change-id) or create a
/// fresh change (`false`). Amend only ever targets the head.
#[allow(clippy::too_many_arguments)]
pub async fn delete_file(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    branch: &str,
    path: &str,
    message: &str,
    author: (String, String),
    amend: bool,
    base_sha: Option<&str>,
) -> Result<EditOutcome, RepoError> {
    jjlab_core::validate_ref_name(branch, "branch").map_err(RepoError::Invalid)?;
    let handle = store.open(org, repo).await?;
    let signature = jj_lib::backend::Signature {
        name: author.0,
        email: author.1,
        timestamp: jj_lib::backend::Timestamp::now(),
    };
    let repo_arc = handle.repo.clone();
    let base = pollster::block_on(async { crate::read::resolve_snapshot(&repo_arc, branch) })?;

    let outcome = pollster::block_on(async {
        let mut tx = handle.repo.start_transaction();
        let outcome = {
            let mut_repo = tx.repo_mut();
            let parent = mut_repo
                .store()
                .get_commit(&base)
                .map_err(|e| RepoError::Other(e.to_string()))?;

            // Optimistic-lock: a delete must target the file the client read.
            let current = file_blob_sha(&parent, path).await?;
            assert_blob_base(current, base_sha)?;

            let repo_path = RepoPathBuf::from_internal_string(path)
                .map_err(|e| RepoError::Other(format!("bad path {path:?}: {e}")))?;
            // Remove the path from the head's merged tree, preserving any other
            // first-class conflicts (deleting a file resolves only its conflict).
            let mut builder = MergedTreeBuilder::new(parent.tree());
            builder.set_or_remove(repo_path, Merge::absent());
            let merged = builder
                .write_tree()
                .await
                .map_err(|e| RepoError::Other(format!("write tree: {e}")))?;

            let can_amend = amend && base != mut_repo.store().root_commit_id().clone();
            let commit = if can_amend {
                let mut builder = mut_repo.rewrite_commit(&parent);
                builder = builder
                    .set_tree(merged)
                    .set_description(message.to_string());
                let commit = builder
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
                let target: jj_lib::ref_name::RefNameBuf = branch.to_string().into();
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
        tx.commit("jjlab: delete file")
            .await
            .map_err(|e| RepoError::Other(e.to_string()))?;
        Ok::<EditOutcome, RepoError>(outcome)
    })?;

    project::project_repo(store, db, org, repo).await?;
    Ok(outcome)
}

/// Atomically write (create/update) several files as one change on top of
/// `branch`'s head. `edits` carries per-file content + an optional optimistic
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
    branch: &str,
    edits: &[BatchEdit],
    message: &str,
    author: (String, String),
    amend: bool,
) -> Result<EditOutcome, RepoError> {
    jjlab_core::validate_ref_name(branch, "branch").map_err(RepoError::Invalid)?;
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
    let base = pollster::block_on(async { crate::read::resolve_snapshot(&repo_arc, branch) })?;

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
                let target: jj_lib::ref_name::RefNameBuf = branch.to_string().into();
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
