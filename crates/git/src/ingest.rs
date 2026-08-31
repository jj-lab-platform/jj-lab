//! Git → jj 升格（Ingest）：把一个裸 git 仓库逐 commit 翻译为 jj change，
//! 应用 `Change-Id` 稳定锚定，并将冲突标记实例化为 first-class conflict。
//!
//! M1 交付：从本地裸 git 仓库逐 commit 写入原生存储、锚定 change-id，并把
//! 「含冲突 marker 的文件」登记为原生冲突记录。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use gix::bstr::ByteSlice;
use gix::objs::tree::EntryKind;
use jj_lib::backend::{CommitId, Signature, Timestamp, TreeValue};
use jj_lib::merged_tree::MergedTree;
use jj_lib::object_id::ObjectId as _;
use jj_lib::repo::Repo;
use jj_lib::repo_path::RepoPathBuf;
use jj_lib::tree_builder::TreeBuilder;

use jjlab_core::db::ChangeRow;
use jjlab_core::Db;

use crate::anchor;
use crate::repo::{RepoError, RepoStore};

pub struct IngestOutcome {
    /// Number of git commits translated into native jj changes.
    pub commits: usize,
    /// Number of commits whose blobs carry git conflict markers.
    pub conflicts: usize,
}

pub async fn ingest_bare_repo(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo_name: &str,
    git_dir: &Path,
) -> Result<IngestOutcome, RepoError> {
    let repo_id = format!("{org}/{repo_name}");
    let gix_repo = gix::open(git_dir)
        .or_else(|_| gix::discover(git_dir).map_err(|e| e.to_string()))
        .map_err(|e| RepoError::Other(e.to_string()))?;
    let commits = read_git_commits(&gix_repo)?;

    let handle = store.open_or_init(org, repo_name).await?;

    db.upsert_org(org, org)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    db.upsert_repo(&repo_id, org, repo_name, "main", None)
        .map_err(|e| RepoError::Other(e.to_string()))?;

    let mut tx = handle.repo.start_transaction();
    let (mut commits_written, mut conflicts_seen) = (0usize, 0usize);

    // git sha → jj CommitId, built as we walk parents (parents precede children).
    let mut sha_to_commit: HashMap<String, CommitId> = HashMap::new();

    {
        let mut_repo = tx.repo_mut();
        let root_id = mut_repo.store().root_commit_id().clone();
        for gc in commits {
            let git_sha = gc.id.to_string();

            let anchor_opt = db
                .lookup_anchor(&repo_id, &git_sha)
                .map_err(|e| RepoError::Other(e.to_string()))?;

            // Idempotent re-ingest: a commit already translated to a jj CommitId
            // is recorded for its children and skipped.
            if let Some(anchor_row) = &anchor_opt {
                if let Some(commit_hex) = &anchor_row.commit_id {
                    if let Some(existing) = jj_lib::backend::CommitId::try_from_hex(commit_hex) {
                        sha_to_commit.insert(git_sha.clone(), existing);
                    }
                    continue;
                }
            }

            let change_id = match &anchor_opt {
                Some(anchor_row) => jj_lib::backend::ChangeId::try_from_reverse_hex(&anchor_row.change_id)
                    .ok_or_else(|| RepoError::Other(format!("invalid change-id {}", anchor_row.change_id)))?,
                None => gc.change_id.clone(),
            };
            let change_id_hex = change_id.reverse_hex();

            let mut parent_ids = Vec::new();
            for p in &gc.parents {
                let key = p.to_string();
                if let Some(cid) = sha_to_commit.get(&key) {
                    parent_ids.push(cid.clone());
                }
            }
            if parent_ids.is_empty() {
                parent_ids.push(root_id.clone());
            }

            let (tree_id, tree_conflicts) = write_tree(mut_repo, &gix_repo, &gc).await?;
            let merged_tree = MergedTree::resolved(mut_repo.store().clone(), tree_id);

            let mut builder = mut_repo.new_commit(parent_ids, merged_tree);
            builder = builder
                .set_description(gc.message.clone())
                .set_change_id(change_id)
                .set_author(Signature {
                    name: gc.author_name.clone(),
                    email: gc.author_email.clone(),
                    timestamp: Timestamp {
                        timestamp: jj_lib::backend::MillisSinceEpoch(gc.author_time * 1000),
                        tz_offset: gc.author_tz,
                    },
                })
                .set_committer(Signature {
                    name: gc.committer_name.clone(),
                    email: gc.committer_email.clone(),
                    timestamp: Timestamp {
                        timestamp: jj_lib::backend::MillisSinceEpoch(gc.committer_time * 1000),
                        tz_offset: gc.committer_tz,
                    },
                });

            let commit = builder
                .write()
                .await
                .map_err(|e| RepoError::Other(e.to_string()))?;
            let commit_id_hex = commit.id().hex();
            sha_to_commit.insert(git_sha.clone(), commit.id().clone());

            db.set_anchor(&repo_id, &git_sha, &change_id_hex, &commit_id_hex)
                .map_err(|e| RepoError::Other(e.to_string()))?;

            db.upsert_change(&ChangeRow {
                change_id: change_id_hex.clone(),
                repo_id: repo_id.clone(),
                description: gc.message.clone(),
                author: format!("{} <{}>", gc.author_name, gc.author_email),
                committer: format!("{} <{}>", gc.committer_name, gc.committer_email),
                git_commit_sha: Some(git_sha.clone()),
            })
            .map_err(|e| RepoError::Other(e.to_string()))?;

            commits_written += 1;
            conflicts_seen += tree_conflicts.len();
            for c in &tree_conflicts {
                let conflict_id = format!("{change_id_hex}:{}", c.path);
                let adds_json =
                    serde_json::to_string(&c.adds).unwrap_or_else(|_| "[]".to_string());
                let removes_json =
                    serde_json::to_string(&c.removes).unwrap_or_else(|_| "[]".to_string());
                db.upsert_conflict(
                    &conflict_id,
                    &repo_id,
                    &change_id_hex,
                    &c.path,
                    &adds_json,
                    &removes_json,
                )
                .map_err(|e| RepoError::Other(e.to_string()))?;
            }
        }
    }

    for (branch_name, head_sha) in head_branches(&gix_repo)? {
        if let Some(anchor) = db
            .lookup_anchor(&repo_id, &head_sha)
            .map_err(|e| RepoError::Other(e.to_string()))?
        {
            db.upsert_bookmark(&repo_id, &branch_name, &anchor.change_id, false)
                .map_err(|e| RepoError::Other(e.to_string()))?;
        }
    }

    tx.commit("jjlab: ingest")
        .await
        .map_err(|e| RepoError::Other(e.to_string()))?;

    Ok(IngestOutcome {
        commits: commits_written,
        conflicts: conflicts_seen,
    })
}

struct GitCommit {
    id: gix::ObjectId,
    parents: Vec<gix::ObjectId>,
    tree: gix::ObjectId,
    change_id: jj_lib::backend::ChangeId,
    message: String,
    author_name: String,
    author_email: String,
    author_time: i64,
    author_tz: i32,
    committer_name: String,
    committer_email: String,
    committer_time: i64,
    committer_tz: i32,
}

fn read_git_commits(repo: &gix::Repository) -> Result<Vec<GitCommit>, RepoError> {
    let mut heads = Vec::new();
    for reference in repo
        .references()
        .map_err(|e| RepoError::Other(e.to_string()))?
        .all()
        .map_err(|e| RepoError::Other(e.to_string()))?
    {
        let reference = reference.map_err(|e| RepoError::Other(e.to_string()))?;
        if let Some(target) = reference.target().try_id() {
            heads.push(target.to_owned());
        }
    }
    if heads.is_empty() {
        if let Ok(id) = repo.head_id().map_err(|e| RepoError::Other(e.to_string())) {
            heads.push(id.detach());
        }
    }
    heads.sort();
    heads.dedup();

    // Build a topologically-ordered list: parents always precede children.
    let mut visiting: HashMap<String, bool> = HashMap::new();
    let mut ordered: Vec<GitCommit> = Vec::new();
    for head in &heads {
        push_commit(repo, head, &mut visiting, &mut ordered)?;
    }
    Ok(ordered)
}

fn head_branches(repo: &gix::Repository) -> Result<Vec<(String, String)>, RepoError> {
    let mut branches = Vec::new();
    for reference in repo
        .references()
        .map_err(|e| RepoError::Other(e.to_string()))?
        .all()
        .map_err(|e| RepoError::Other(e.to_string()))?
    {
        let reference = reference.map_err(|e| RepoError::Other(e.to_string()))?;
        let name = reference.name().to_string();
        if let Some(short) = name.strip_prefix("refs/heads/") {
            if let Some(id) = reference.target().try_id() {
                branches.push((short.to_string(), id.to_string()));
            }
        }
    }
    Ok(branches)
}

fn push_commit(
    repo: &gix::Repository,
    id: &gix::ObjectId,
    visiting: &mut HashMap<String, bool>,
    ordered: &mut Vec<GitCommit>,
) -> Result<(), RepoError> {
    let key = id.to_string();
    let is_root = visiting.get(&key).cloned().unwrap_or(false);
    if is_root {
        return Ok(());
    }
    visiting.insert(key, true);

    let commit = repo
        .find_commit(*id)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    for parent in commit.parent_ids() {
        push_commit(repo, &parent.detach(), visiting, ordered)?;
    }

    let author = commit
        .author()
        .map(|a| a.trim())
        .map_err(|e| RepoError::Other(e.to_string()))?;
    let committer = commit
        .committer()
        .map(|c| c.trim())
        .map_err(|e| RepoError::Other(e.to_string()))?;
    // Reconstruct the full message (title + body) from the parsed message ref.
    let message = commit
        .message()
        .map(|m| {
            let title = m.title.to_str_lossy();
            match m.body {
                Some(body) => format!("{title}\n\n{}", body.to_str_lossy()),
                None => title.to_string(),
            }
        })
        .unwrap_or_else(|_| commit.message_raw_sloppy().to_string());

    let (author_name, author_email, author_time, author_tz) = signature_parts(&author);
    let (committer_name, committer_email, committer_time, committer_tz) = signature_parts(&committer);

    let tree = commit
        .tree()
        .map(|t| t.id().detach())
        .map_err(|e| RepoError::Other(e.to_string()))?;

    let cref = commit
        .decode()
        .map_err(|e| RepoError::Other(e.to_string()))?;
    let change_id =
        anchor::resolve_change_id(&cref, &jj_lib::backend::CommitId::from_bytes(id.as_bytes()));

    ordered.push(GitCommit {
        id: id.to_owned(),
        parents: commit.parent_ids().map(|p| p.detach()).collect(),
        tree,
        change_id,
        message,
        author_name,
        author_email,
        author_time,
        author_tz,
        committer_name,
        committer_email,
        committer_time,
        committer_tz,
    });
    Ok(())
}

fn signature_parts(sig: &gix::actor::SignatureRef<'_>) -> (String, String, i64, i32) {
    let seconds = sig.seconds();
    let (tz_offset, _) = sig
        .time()
        .map(|t| (t.offset, t.seconds))
        .unwrap_or((0, seconds));
    (
        sig.name.trim().to_str_lossy().to_string(),
        sig.email.trim().to_str_lossy().to_string(),
        seconds,
        tz_offset,
    )
}

struct ResolvedConflict {
    path: String,
    adds: Vec<String>,
    removes: Vec<String>,
}

fn conflict_sides(data: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut left: Vec<u8> = Vec::new();
    let mut right: Vec<u8> = Vec::new();
    let mut seen_start = false;
    let mut seen_mid = false;
    let mut in_left = false;
    let mut in_right = false;
    for line in data.split_inclusive(|&b| b == b'\n') {
        if line.starts_with(b"<<<<<<<") {
            seen_start = true;
            in_left = true;
            in_right = false;
        } else if line.starts_with(b"=======") {
            seen_mid = true;
            in_left = false;
            in_right = true;
        } else if line.starts_with(b">>>>>>>") {
            break;
        } else if in_left {
            left.extend_from_slice(line);
        } else if in_right {
            right.extend_from_slice(line);
        }
    }
    if seen_start && seen_mid {
        Some((left, right))
    } else {
        None
    }
}

async fn write_tree(
    mut_repo: &mut jj_lib::repo::MutableRepo,
    gix_repo: &gix::Repository,
    gc: &GitCommit,
) -> Result<(jj_lib::backend::TreeId, Vec<ResolvedConflict>), RepoError> {
    let store = mut_repo.store().clone();
    let mut builder = TreeBuilder::new(store.clone(), store.empty_tree_id().clone());
    let mut conflicts = Vec::new();
    write_git_tree(
        gix_repo,
        &gc.tree,
        &mut builder,
        RepoPathBuf::root(),
        store.as_ref(),
        &mut conflicts,
    )
    .await?;
    let tree_id = builder
        .write_tree()
        .await
        .map_err(|e| RepoError::Other(e.to_string()))?;
    Ok((tree_id, conflicts))
}

async fn write_git_tree(
    gix_repo: &gix::Repository,
    tree_id: &gix::ObjectId,
    builder: &mut TreeBuilder,
    path: RepoPathBuf,
    store: &jj_lib::store::Store,
    conflicts: &mut Vec<ResolvedConflict>,
) -> Result<(), RepoError> {
    let tree = gix_repo
        .find_tree(*tree_id)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    for entry in tree.iter() {
        let entry = entry.map_err(|e| RepoError::Other(e.to_string()))?;
        let name = entry
            .filename()
            .to_str()
            .map_err(|e| RepoError::Other(format!("non-utf8 tree entry: {e}")))?;
        let child_path = path.join(
            &jj_lib::repo_path::RepoPathComponentBuf::new(name)
                .map_err(|e| RepoError::Other(format!("invalid path component {name}: {e}")))?,
        );

        match entry.mode().kind() {
            EntryKind::Tree => {
                let child = entry.id().detach();
                Box::pin(write_git_tree(
                    gix_repo,
                    &child,
                    builder,
                    child_path,
                    store,
                    conflicts,
                ))
                .await?;
            }
            EntryKind::Blob | EntryKind::BlobExecutable => {
                let is_exec = matches!(entry.mode().kind(), EntryKind::BlobExecutable);
                let blob = gix_repo
                    .find_blob(entry.id().detach())
                    .map_err(|e| RepoError::Other(e.to_string()))?;
                let data = blob.data.clone();
                if let Some((left, right)) = conflict_sides(&data) {
                    conflicts.push(ResolvedConflict {
                        path: child_path.as_internal_file_string().to_string(),
                        adds: vec![
                            String::from_utf8_lossy(&left).into_owned(),
                            String::from_utf8_lossy(&right).into_owned(),
                        ],
                        removes: Vec::new(),
                    });
                }
                let file_id = write_blob(store, &child_path, &data).await?;
                builder.set(
                    child_path,
                    TreeValue::File {
                        id: file_id,
                        executable: is_exec,
                        copy_id: jj_lib::backend::CopyId::placeholder(),
                    },
                );
            }
            EntryKind::Link => {
                let blob = gix_repo
                    .find_blob(entry.id().detach())
                    .map_err(|e| RepoError::Other(e.to_string()))?;
                let target = std::str::from_utf8(&blob.data)
                    .map_err(|e| RepoError::Other(format!("symlink target: {e}")))?
                    .to_string();
                let symlink_id = store
                    .write_symlink(&child_path, &target)
                    .await
                    .map_err(|e| RepoError::Other(e.to_string()))?;
                builder.set(child_path, TreeValue::Symlink(symlink_id));
            }
            EntryKind::Commit => {
                // git submodule: skip for M1 (no commit graph wiring yet).
                continue;
            }
        }
    }
    Ok(())
}

async fn write_blob(
    store: &jj_lib::store::Store,
    path: &RepoPathBuf,
    data: &[u8],
) -> Result<jj_lib::backend::FileId, RepoError> {
    let mut cursor = futures_util::io::Cursor::new(data);
    store
        .write_file(path, &mut cursor)
        .await
        .map_err(|e| RepoError::Other(e.to_string()))
}