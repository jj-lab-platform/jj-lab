//! Operation-log cloud sync (M4): SubscribeOps (SSE) + UndoOperation.
//!
//! Every server-side mutation appends an op_log row. Clients subscribe via
//! SSE and receive events in order; `undo` rolls the repo back to the
//! operation's parent operation (jj-native: load the repo at the op's first
//! parent and re-point the op head), then re-projects metadata.

use std::sync::Arc;

use jj_lib::op_store::OperationId;
use jj_lib::repo::Repo as _;

use jjlab_core::db::{OpLogRow, MrRow};
use jjlab_core::Db;

use crate::project;
use crate::repo::{RepoError, RepoStore};

/// A single operation event for SSE subscribers.
pub struct OpEvent {
    pub id: String,
    pub repo_id: String,
    pub op_type: String,
    pub payload: String,
    pub undo_of: Option<String>,
}

pub fn event_from_row(r: OpLogRow) -> OpEvent {
    OpEvent {
        id: r.id,
        repo_id: r.repo_id,
        op_type: r.op_type,
        payload: r.payload,
        undo_of: r.undo_of,
    }
}

/// Serialize an event as an SSE data frame.
pub fn sse_frame(ev: &OpEvent) -> String {
    let data = serde_json::json!({
        "id": ev.id,
        "repo_id": ev.repo_id,
        "op_type": ev.op_type,
        "payload": ev.payload,
        "undo_of": ev.undo_of,
    })
    .to_string();
    format!("id: {}\nevent: op\ndata: {}\n\n", ev.id, data)
}

/// List ops after `after_id` (exclusive) for a repo, oldest first — used by
/// clients catching up before/alongside an SSE subscription.
pub fn ops_since(db: &Db, repo_id: &str, after_id: &str) -> Vec<OpEvent> {
    let Ok(rows) = db.list_op_log(repo_id) else {
        return Vec::new();
    };
    let start = rows
        .iter()
        .position(|r| r.id == after_id)
        .map(|i| i + 1)
        .unwrap_or(0);
    rows.into_iter().skip(start).map(event_from_row).collect()
}

/// Undo the operation identified by `op_id`. For content edits the op_log row
/// carries the authoritative jj operation id (`payload.jj_op_id`), so undo
/// rolls back to exactly that operation's parent. Otherwise it falls back to
/// the previous "walk past bookkeeping" behavior.
///
/// Implementation: load the repo at the target operation's first parent and
/// re-point the op head there (a transaction that publishes the parent view).
pub async fn undo_operation(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    op_id: &str,
) -> Result<OpEvent, RepoError> {
    let repo_id = format!("{org}/{repo}");
    // Find the op row (list is oldest-first; match by id).
    let rows = db
        .list_op_log(&repo_id)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    let target = rows
        .iter()
        .find(|r| r.id == op_id)
        .ok_or_else(|| RepoError::Other(format!("op {op_id} not found")))?;
    if target.undo_of.is_some() {
        return Err(RepoError::Conflict("operation is already an undo".into()));
    }

    // Authoritative jj op id recorded by content edits.
    let jj_op_id: Option<OperationId> = target
        .payload
        .parse::<serde_json::Value>()
        .ok()
        .and_then(|v| v.get("jj_op_id").and_then(|s| s.as_str()).map(str::to_string))
        .and_then(|hex| OperationId::try_from_hex(&hex));

    let handle = store.open(org, repo).await?;
    let repo_arc = handle.repo.clone();

    // Current op = repo.operation(); undo = load at its first parent and make
    // that the new head by writing an operation on top of the parent's view.
    pollster::block_on(async {
        let current_op = repo_arc.operation().clone();
        // Walk past bookkeeping ops ('jjlab: project export') so undo reverts
        // the last *meaningful* operation, not the metadata projection itself.
        let mut parent_op = None;
        let mut cursor = current_op.clone();
        for _ in 0..64 {
            // Precise path: if the target op id is recorded, revert to its
            // first parent (exact rollback of that edit).
            if let Some(ref target_id) = jj_op_id {
                let parent_ids = repo_arc
                    .op_store()
                    .read_operation(target_id)
                    .await
                    .map_err(|e| RepoError::Other(format!("read op {target_id}: {e}")))?
                    .parents;
                let Some(pid) = parent_ids.into_iter().next() else {
                    return Err(RepoError::Other("operation has no parents".into()));
                };
                let parent_data = repo_arc
                    .op_store()
                    .read_operation(&pid)
                    .await
                    .map_err(|e| RepoError::Other(format!("read op parent {pid}: {e}")))?;
                parent_op = Some(jj_lib::operation::Operation::new(
                    repo_arc.op_store().clone(),
                    pid,
                    parent_data,
                ));
                break;
            }

            let parents = cursor
                .parents()
                .await
                .map_err(|e| RepoError::Other(format!("op parents: {e}")))?;
            let Some(p) = parents.first() else { break };
            let is_bookkeeping = p.metadata().description.starts_with("jjlab: project export")
                || p.metadata().description.starts_with("reconcile divergent");
            if is_bookkeeping {
                cursor = p.clone();
                continue;
            }
            // Undo reverts the last meaningful op: target its parent.
            let pp = p.parents().await
                .map_err(|e| RepoError::Other(format!("op parents: {e}")))?;
            parent_op = pp.first().cloned();
            break;
        }
        let Some(parent_op) = parent_op else {
            return Err(RepoError::Other("operation has no parents".into()));
        };

        // Load the repo at the parent operation, then write a new operation
        // with that view so the op head moves backwards semantically.
        let loader = repo_arc.loader();
        let parent_repo = loader.load_at(&parent_op).await
            .map_err(|e| RepoError::Other(format!("load at parent op: {e}")))?;
        let new_head = {
            let tx = parent_repo.start_transaction();
            tx.commit("jjlab: undo").await
                .map_err(|e| RepoError::Other(e.to_string()))?
        };

        // The old head is still a head (divergent with the undo op). Remove it
        // so the undo op becomes the sole op head.
        repo_arc
            .op_heads_store()
            .update_op_heads(
                std::slice::from_ref(current_op.id()),
                new_head.operation().id(),
            )
            .await
            .map_err(|e| RepoError::Other(format!("update op heads: {e}")))?;
        Ok::<(), RepoError>(())
    })?;

    // Re-project metadata from the rolled-back repo state.
    project::project_repo(store, db, org, repo).await?;

    // Record the undo op.
    let undo_id = format!("undo-{}-{}", op_id, std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0));
    let row = OpLogRow {
        id: undo_id,
        repo_id: repo_id.clone(),
        op_type: "undo".to_string(),
        payload: serde_json::json!({ "undo_of": op_id }).to_string(),
        undo_of: Some(op_id.to_string()),
    };
    db.append_op_log(&row)
        .map_err(|e| RepoError::Other(e.to_string()))?;
    Ok(event_from_row(row))
}

/// Get an MR by op-scoped repo (helper reused by SSE filter paths).
pub fn mr_by_number(db: &Db, repo_id: &str, number: i64) -> Option<MrRow> {
    db.get_mr_by_number(repo_id, number).ok().flatten()
}

/// Utility: current op head id of a repo (for clients to detect divergence).
pub async fn current_op_id(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
) -> Result<String, RepoError> {
    let handle = store.open(org, repo).await?;
    Ok(handle.repo.operation().id().to_string())
}
