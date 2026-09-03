//! MR GC-anchor reconciler. Runs inside the CI scheduler loop (its interval
//! already exists and it already visits repos), pruning hidden
//! `refs/jjlab/mr/*/head` anchors whose MR is no longer open. This closes the
//! crash window between a DB state change and the ref delete (or a repo that
//! lost its MR row entirely): stale anchors are harmless — they only pin an
//! extra commit — but they must never accumulate.

use std::sync::Arc;

use jjlab_core::Db;

use crate::mr_anchor;
use crate::repo::RepoStore;

/// Scan every repo and drop stale MR anchors. Returns the count pruned.
pub async fn prune_stale_mr_anchors(
    db: &Arc<Db>,
    store: &Arc<RepoStore>,
) -> Result<usize, String> {
    let repos = db.list_repos().map_err(|e| e.to_string())?;
    let mut pruned = 0;
    for repo_row in repos {
        let Some((org, repo)) = repo_row.id.split_once('/') else { continue };
        let org = org.to_string();
        let repo = repo.to_string();
        let repo_id = repo_row.id.clone();
        let open_mr = |n: i64| -> bool {
            db.get_mr_by_number(&repo_id, n)
                .map(|m| m.map(|mr| mr.state == "open").unwrap_or(false))
                .unwrap_or(false)
        };
        match mr_anchor::prune_stale_mr_heads(
            store,
            &org,
            &repo,
            open_mr,
        )
        .await
        {
            Ok(n) => pruned += n,
            Err(e) => tracing::warn!(repo = repo_id, err = %e, "prune MR anchors failed"),
        }
    }
    Ok(pruned)
}
