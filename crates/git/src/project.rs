//! Project the jj view into SQLite metadata (ADR: DB is a cache/overlay over
//! the jj truth source). Called after clone/fetch/receive so that orgs/repos/
//! bookmarks/conflicts rows reflect the live repo state.

use std::sync::Arc;

use jj_lib::backend::TreeValue;
use jj_lib::object_id::ObjectId as _;
use jj_lib::repo::Repo as _;

use jjlab_core::db::{ChangeRow, OpLogRow};
use jjlab_core::Db;

use crate::repo::{RepoError, RepoStore};

/// Upsert org/repo rows and project bookmarks + reachable changes into the DB.
pub async fn project_repo(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
) -> Result<(), RepoError> {
    let db = db.clone();
    let repo_id = format!("{org}/{repo}");
    db.upsert_org(org, org)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    db.upsert_repo(&repo_id, org, repo, "main", None)
        .map_err(|e| RepoError::Other(e.to_string()))?;

    let handle = store.open(org, repo).await?;


    // Export jj bookmarks to git refs so the repo is visible to plain git
    // (clone/fetch) and to mirrors.
    let export_repo = handle.repo.clone();
    pollster::block_on(async {
        let mut tx = export_repo.start_transaction();
        let stats = {
            let mut_repo = tx.repo_mut();
            jj_lib::git::export_refs(mut_repo)
                .map_err(|e| RepoError::Other(format!("export refs: {e}")))?
        };
        if !stats.failed_bookmarks.is_empty() {
            tracing::warn!(failed = ?stats.failed_bookmarks, "export_refs failed bookmarks");
        }
        if !stats.failed_tags.is_empty() {
            tracing::warn!(failed = ?stats.failed_tags, "export_refs failed tags");
        }
        tracing::info!(
            bookmarks = stats.failed_bookmarks.len(),
            "export_refs ran"
        );
        tx.commit("jjlab: project export")
            .await
            .map_err(|e| RepoError::Other(e.to_string()))?;
        Ok::<(), RepoError>(())
    })?;

    let repo_arc = handle.repo.clone();
    let repo_id2 = repo_id.clone();

    let db2 = db.clone();
    tokio::task::spawn_blocking(move || -> Result<(), RepoError> {
        let db = &db2;
        // Bookmarks: local bookmarks point at commit shas.
        for (name, target) in repo_arc.view().local_bookmarks() {
            if let Some(id) = target.as_normal() {
                // The bookmark references a change; find its change-id via the
                // commit. Insert the change row first to satisfy the FK.
                let commit = repo_arc
                    .store()
                    .get_commit(id)
                    .map_err(|e| RepoError::Other(e.to_string()))?;
                let change_id = commit.change_id().reverse_hex();
                db.upsert_change(&ChangeRow {
                    change_id: change_id.clone(),
                    repo_id: repo_id2.clone(),
                    description: commit.description().to_string(),
                    author: format!(
                        "{} <{}>",
                        commit.author().name,
                        commit.author().email
                    ),
                    committer: format!(
                        "{} <{}>",
                        commit.committer().name,
                        commit.committer().email
                    ),
                    git_commit_sha: Some(id.hex()),
                })
                .map_err(|e| RepoError::Other(e.to_string()))?;
                db.upsert_bookmark(&repo_id2, name.as_str(), &change_id, false)
                    .map_err(|e| RepoError::Other(e.to_string()))?;
            }
        }

        // Changes: every reachable commit becomes a change row.
        let mut stack: Vec<jj_lib::backend::CommitId> =
            repo_arc.view().heads().iter().cloned().collect();
        let mut seen = std::collections::HashSet::new();
        while let Some(id) = stack.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let commit = match repo_arc.store().get_commit(&id) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let has_conflict = commit.tree().has_conflict();
            db.upsert_change(&ChangeRow {
                change_id: commit.change_id().reverse_hex(),
                repo_id: repo_id2.clone(),
                description: commit.description().to_string(),
                author: format!("{} <{}>", commit.author().name, commit.author().email),
                committer: format!(
                    "{} <{}>",
                    commit.committer().name,
                    commit.committer().email
                ),
                git_commit_sha: Some(id.hex()),
            })
            .map_err(|e| RepoError::Other(e.to_string()))?;

            if has_conflict {
                // Record conflicted paths with their sides as terms.
                for (path, value) in commit.tree().conflicts() {
                    let Ok(merge) = value else { continue };
                    let mut adds: Vec<String> = Vec::new();
                    let mut removes: Vec<String> = Vec::new();
                    for (i, term) in merge.iter().enumerate() {
                        let label = if i % 2 == 0 { "add" } else { "remove" };
                        match term {
                            None => continue,
                            Some(TreeValue::File { .. }) => {
                                let entry = if label == "add" {
                                    &mut adds
                                } else {
                                    &mut removes
                                };
                                entry.push(path.as_internal_file_string().to_string());
                            }
                            Some(_) => {
                                let entry = if label == "add" {
                                    &mut adds
                                } else {
                                    &mut removes
                                };
                                entry.push(format!("{}:{label}", path.as_internal_file_string()));
                            }
                        }
                    }
                    let adds_json = serde_json::to_string(&adds).unwrap_or_else(|_| "[]".into());
                    let removes_json =
                        serde_json::to_string(&removes).unwrap_or_else(|_| "[]".into());
                    let cid = format!("{}:{}", commit.change_id().reverse_hex(), path.as_internal_file_string());
                    db.upsert_conflict(
                        &cid,
                        &repo_id2,
                        &commit.change_id().reverse_hex(),
                        path.as_internal_file_string(),
                        &adds_json,
                        &removes_json,
                    )
                    .map_err(|e| RepoError::Other(e.to_string()))?;
                }
            }

            for p in commit.parent_ids() {
                stack.push(p.clone());
            }
        }

        // Force-push survival: an open MR whose head branch moved keeps its
        // reviews. Two cases:
        //   1. jj semantics: the tip commit carries the same change-id
        //      header -> re-associate directly.
        //   2. plain-git force-push (no change-id header): fall back to
        //      matching the bookmark name recorded on the MR.
        let open_mrs: Vec<(i64, String, Option<String>)> = db
            .list_open_mrs_for_reassoc()
            .map_err(|e| RepoError::Other(e.to_string()))?;
        for (mr_id, mr_change_id, mr_branch) in &open_mrs {
            // Only the MR's own head branch may move its head; a blanket
            // change-id match across ALL bookmarks would drag the MR back to
            // an unrelated branch that still carries the old change.
            let Some(branch) = mr_branch else { continue };
            let target = repo_arc
                .view()
                .get_local_bookmark(&jj_lib::ref_name::RefNameBuf::from(branch.clone()));
            let Some(id) = target.as_normal() else { continue };
            let Ok(commit) = repo_arc.store().get_commit(id) else { continue };
            let change_id = commit.change_id().reverse_hex();
            let sha = commit.id().hex();
            // Re-associate the MR head when the tip moved (new sha), whether
            // by a new change or an amend of the same change.
            if mr_change_id.as_str() != change_id {
                let _ = db.update_mr(*mr_id, None, Some(&change_id), Some(&sha));
            } else {
                // Same change-id (amend): only the sha moved.
                let _ = db.update_mr(*mr_id, None, None, Some(&sha));
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| RepoError::Other(e.to_string()))??;

    let _ = db2;

    // Point git HEAD at the default branch so `git clone` resolves a real
    // branch (no "initial branch" fallback) and always checks out the newest
    // tip even after a plain-git history rewrite. jj's git export leaves HEAD
    // as a symref to an unborn/other branch (e.g. `refs/heads/master`) or the
    // store detaches it, neither of which matches the served default branch.
    let head_repo = handle.repo.clone();
    pollster::block_on(async {
        let Ok(backend) = jj_lib::git::get_git_backend(head_repo.store()) else { return };
        let git_repo = backend.git_repo();

        // Default branch: prefer a local bookmark named main/master, else the
        // first bookmark, else "main". This must match the branch git clients
        // receive on clone.
        let branch_name = {
            let bookmarks: Vec<String> = head_repo
                .view()
                .local_bookmarks()
                .map(|(n, _)| n.as_str().to_string())
                .collect();
            if bookmarks.is_empty() {
                // Fall back to the default branch ref that actually exists.
                ["main", "master"]
                    .iter()
                    .find(|n| git_repo.find_reference(format!("refs/heads/{n}").as_str()).is_ok())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "main".to_string())
            } else if let Some(b) = bookmarks.iter().find(|n| *n == "main" || *n == "master") {
                b.clone()
            } else {
                bookmarks[0].clone()
            }
        };

        // Write HEAD symbolic to that branch (standard git layout: HEAD is a
        // loose file containing `ref: refs/heads/<name>`).
        let head_file = git_repo.git_dir().join("HEAD");
        let symbolic = format!("ref: refs/heads/{branch_name}\n");
        let current = std::fs::read(&head_file).unwrap_or_default();
        if current.as_slice() != symbolic.as_bytes() {
            let _ = std::fs::write(&head_file, symbolic.as_bytes());
        }
    });

    db.append_op_log(&OpLogRow {
        id: format!("project-{}-{}", repo_id.replace("/", "_"), std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)),
        repo_id,
        op_type: "project".to_string(),
        payload: "{}".to_string(),
        undo_of: None,
    })
    .map_err(|e| RepoError::Other(e.to_string()))?;
    Ok(())
}