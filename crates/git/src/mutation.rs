//! Mutations (write surface): repo lifecycle, bookmarks, tags, and
//! file-content edits. Every content edit produces a new commit on top of the
//! target, carrying the change-id of the change it amends (or a fresh one) —
//! this is the change-centric core: editing a file is creating a change.

use std::sync::Arc;

use jj_lib::backend::{CopyId, TreeValue};
use jj_lib::object_id::ObjectId as _;
use jj_lib::repo::Repo as _;
use jj_lib::repo_path::RepoPathBuf;
use jj_lib::tree_builder::TreeBuilder;

use jjlab_core::Db;

use crate::project;
use crate::read::{self, resolve_commit};
use crate::repo::{RepoError, RepoStore};

/// Create an empty repo with a README (rucoder-neo semantics).
pub async fn create_repo(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    default_branch: &str,
    author: (String, String),
) -> Result<(), RepoError> {
    jjlab_core::validate_segment(org, "org").map_err(RepoError::Other)?;
    jjlab_core::validate_segment(repo, "repo").map_err(RepoError::Other)?;
    jjlab_core::validate_ref_name(default_branch, "branch").map_err(RepoError::Other)?;
    if store.exists(org, repo) {
        return Err(RepoError::Other(format!("repository {org}/{repo} already exists")));
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

/// Move a bookmark (git branch) to a rev. Creates it when absent.
pub async fn set_branch(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    name: &str,
    rev: &str,
) -> Result<String, RepoError> {
    jjlab_core::validate_ref_name(name, "branch").map_err(RepoError::Other)?;
    let handle = store.open(org, repo).await?;
    let commit_id = pollster::block_on(async {
        let repo_arc = handle.repo.clone();
        resolve_commit(&repo_arc, rev)
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
    jjlab_core::validate_ref_name(name, "branch").map_err(RepoError::Other)?;
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

/// Create/move a tag to a rev.
pub async fn set_tag(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    name: &str,
    rev: &str,
) -> Result<String, RepoError> {
    jjlab_core::validate_ref_name(name, "tag").map_err(RepoError::Other)?;
    let handle = store.open(org, repo).await?;
    let commit_id = pollster::block_on(async {
        let repo_arc = handle.repo.clone();
        resolve_commit(&repo_arc, rev)
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
    jjlab_core::validate_ref_name(name, "tag").map_err(RepoError::Other)?;
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

/// Write (create/update) a file at `path` on top of `branch`'s head,
/// producing a new change. If `amend_change` is given (a change-id hex
/// prefix), the edit rewrites that change instead of creating a new one.
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
    amend_change: Option<&str>,
) -> Result<EditOutcome, RepoError> {
    jjlab_core::validate_ref_name(branch, "branch").map_err(RepoError::Other)?;
    let handle = store.open(org, repo).await?;
    let signature = jj_lib::backend::Signature {
        name: author.0,
        email: author.1,
        timestamp: jj_lib::backend::Timestamp::now(),
    };
    let repo_arc = handle.repo.clone();
    let base = pollster::block_on(async { resolve_commit(&repo_arc, branch) })?;

    let outcome = pollster::block_on(async {
        let mut tx = handle.repo.start_transaction();
        let outcome = {
            let mut_repo = tx.repo_mut();
            let store_ = mut_repo.store().clone();

            // Parent = the head of the branch (or the amended change itself).
            let (parent_id, change_id) = match amend_change {
                Some(cid) => {
                    let amended = read::resolve_change(&repo_arc, cid)?;
                    let amended_commit = repo_arc
                        .store()
                        .get_commit(&amended)
                        .map_err(|e| RepoError::Other(e.to_string()))?;
                    let cid = amended_commit.change_id().clone();
                    (amended.clone(), Some(cid))
                }
                None => (base.clone(), None),
            };
            let parent = mut_repo
                .store()
                .get_commit(&parent_id)
                .map_err(|e| RepoError::Other(e.to_string()))?;

            // Rebuild the tree with the file updated.
            let file_id = store_
                .write_file(&RepoPathBuf::root(), &mut content)
                .await
                .map_err(|e| RepoError::Other(format!("write file: {e}")))?;
            let repo_path = RepoPathBuf::from_internal_string(path)
                .map_err(|e| RepoError::Other(format!("bad path {path:?}: {e}")))?;

            // Start from the parent's root tree and set the single path.
            let tree = parent.tree();
            let mut builder = TreeBuilder::new(store_.clone(), store_.empty_tree_id().clone());
            // Materialize the existing tree into the builder by walking it.
            collect_tree(&tree, "", &mut builder).await?;
            builder.set(
                repo_path,
                TreeValue::File {
                    id: file_id,
                    executable: false,
                    copy_id: CopyId::placeholder(),
                },
            );
            let tree_id = builder
                .write_tree()
                .await
                .map_err(|e| RepoError::Other(format!("write tree: {e}")))?;
            let merged = jj_lib::merged_tree::MergedTree::resolved(store_, tree_id);

            let mut builder = mut_repo.new_commit(vec![parent_id], merged);
            builder = builder
                .set_description(message.to_string())
                .set_author(signature.clone())
                .set_committer(signature.clone());
            if let Some(cid) = change_id {
                builder = builder.set_change_id(cid);
            }
            let commit = builder
                .write()
                .await
                .map_err(|e| RepoError::Other(format!("write commit: {e}")))?;

            // Move the branch to the new commit.
            let target: jj_lib::ref_name::RefNameBuf = branch.to_string().into();
            mut_repo.set_local_bookmark_target(
                &target,
                jj_lib::op_store::RefTarget::normal(commit.id().clone()),
            );

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

/// Delete a file at `path` on top of `branch`, producing a new change.
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
) -> Result<EditOutcome, RepoError> {
    jjlab_core::validate_ref_name(branch, "branch").map_err(RepoError::Other)?;
    let handle = store.open(org, repo).await?;
    let signature = jj_lib::backend::Signature {
        name: author.0,
        email: author.1,
        timestamp: jj_lib::backend::Timestamp::now(),
    };
    let repo_arc = handle.repo.clone();
    let base = pollster::block_on(async { resolve_commit(&repo_arc, branch) })?;

    let outcome = pollster::block_on(async {
        let mut tx = handle.repo.start_transaction();
        let outcome = {
            let mut_repo = tx.repo_mut();
            let store_ = mut_repo.store().clone();
            let parent = mut_repo
                .store()
                .get_commit(&base)
                .map_err(|e| RepoError::Other(e.to_string()))?;

            let repo_path = RepoPathBuf::from_internal_string(path)
                .map_err(|e| RepoError::Other(format!("bad path {path:?}: {e}")))?;
            let tree = parent.tree();
            let mut builder = TreeBuilder::new(store_.clone(), store_.empty_tree_id().clone());
            collect_tree(&tree, "", &mut builder).await?;
            builder.remove(repo_path);
            let tree_id = builder
                .write_tree()
                .await
                .map_err(|e| RepoError::Other(format!("write tree: {e}")))?;
            let merged = jj_lib::merged_tree::MergedTree::resolved(store_, tree_id);

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

/// Recursively materialize a (resolved) tree into the builder.
async fn collect_tree(
    tree: &jj_lib::merged_tree::MergedTree,
    prefix: &str,
    builder: &mut TreeBuilder,
) -> Result<(), RepoError> {
    for (path_buf, value_res) in tree.entries() {
        let value = value_res
            .map_err(|e| RepoError::Other(format!("tree entry: {e}")))?;
        let resolved = value
            .into_resolved()
            .map_err(|_| RepoError::Other("conflicted tree".to_string()))?;
        let Some(entry) = resolved else { continue };
        let full = if prefix.is_empty() {
            path_buf.into_internal_string()
        } else {
            format!("{prefix}/{}", path_buf.into_internal_string())
        };
        match entry {
            TreeValue::File { id, executable, copy_id } => {
                let p = RepoPathBuf::from_internal_string(&full)
                    .map_err(|e| RepoError::Other(format!("bad path {full:?}: {e}")))?;
                builder.set(
                    p,
                    TreeValue::File { id, executable, copy_id },
                );
            }
            TreeValue::Symlink(id) => {
                let p = RepoPathBuf::from_internal_string(&full)
                    .map_err(|e| RepoError::Other(format!("bad path {full:?}: {e}")))?;
                builder.set(p, TreeValue::Symlink(id));
            }
            TreeValue::GitSubmodule(_) => {}
            TreeValue::Tree(_) => {}
        }
    }
    Ok(())
}