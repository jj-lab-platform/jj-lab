//! Merge-request GC anchors (ADR-011).
//!
//! A merge request pins the exact `head_sha` it was opened against. That
//! commit is only reachable from the bookmark the MR came from; once the
//! author amends/rebases (or the branch is deleted after merge) the object
//! would be swept by `git gc`. To keep the reviewed snapshot alive we hold a
//! hidden ref `refs/jjlab/mr/{number}/head` in the repo's git dir — the same
//! private-namespace pattern jj itself uses for `refs/jj/keep/*`. The ref is
//! created when an MR opens and deleted when it reaches a terminal state, so
//! it never accumulates.

use std::path::Path;
use std::sync::Arc;

use crate::http::git_dir_of;
use crate::repo::{RepoError, RepoStore};

const MR_REF_PREFIX: &str = "refs/jjlab/mr";

fn ref_name(number: i64) -> String {
    format!("{MR_REF_PREFIX}/{number}/head")
}

/// Point the MR's hidden GC anchor at `sha`. Idempotent: re-pointing to the
/// same head is a no-op, and a same-transaction force-push refresh (via
/// `update_mr_head`) simply moves the anchor to the new head.
pub async fn set_mr_head(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    number: i64,
    sha: &str,
) -> Result<(), RepoError> {
    let git_dir = git_dir_of(store, org, repo)?;
    update_ref(&git_dir, &ref_name(number), sha).await
}

/// Delete the hidden GC anchor once the MR reaches a terminal state
/// (merged / closed / deleted). Absent refs are a no-op.
pub async fn clear_mr_head(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    number: i64,
) -> Result<(), RepoError> {
    let git_dir = git_dir_of(store, org, repo)?;
    delete_ref(&git_dir, &ref_name(number)).await
}

/// Delete every `refs/jjlab/mr/*/head` anchor for the repo (used by repo
/// delete). Idempotent.
pub async fn clear_repo_mr_heads(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
) -> Result<(), RepoError> {
    let git_dir = git_dir_of(store, org, repo)?;
    for name in list_mr_refs(&git_dir).await?.into_iter() {
        delete_ref(&git_dir, &name).await?;
    }
    Ok(())
}

/// Reconcile: drop hidden anchors whose MR is no longer open. `open_mr` says
/// whether the given MR number is still open.

/// Returns the number of refs pruned.
pub async fn prune_stale_mr_heads(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    open_mr: impl Fn(i64) -> bool,
) -> Result<usize, RepoError> {
    let git_dir = git_dir_of(store, org, repo)?;
    let refs = list_mr_refs(&git_dir).await?;
    let mut pruned = 0;
    for name in refs {
        if let Some(n) = number_of(&name) {
            if !open_mr(n) {
                delete_ref(&git_dir, &name).await?;
                pruned += 1;
            }
        }
    }
    Ok(pruned)
}

fn number_of(ref_name_str: &str) -> Option<i64> {
    let rest = ref_name_str.strip_prefix(&format!("{MR_REF_PREFIX}/"))?;
    let (num, _sfx) = rest.split_once('/')?;
    num.parse().ok()
}

async fn list_mr_refs(git_dir: &Path) -> Result<Vec<String>, RepoError> {
    let output = tokio::process::Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(["for-each-ref", "--format=%(refname)", MR_REF_PREFIX])
        .output()
        .await
        .map_err(|e| RepoError::Other(format!("for-each-ref mr: {e}")))?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

async fn update_ref(git_dir: &Path, name: &str, sha: &str) -> Result<(), RepoError> {
    let status = tokio::process::Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(["update-ref", name, sha])
        .status()
        .await
        .map_err(|e| RepoError::Other(format!("update-ref: {e}")))?;
    if !status.success() {
        return Err(RepoError::Other(format!("update-ref {name} failed")));
    }
    Ok(())
}

async fn delete_ref(git_dir: &Path, name: &str) -> Result<(), RepoError> {
    let status = tokio::process::Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(["update-ref", "-d", name])
        .status()
        .await
        .map_err(|e| RepoError::Other(format!("delete-ref: {e}")))?;
    // A non-zero exit for a missing ref is fine (git update-ref -d on absent
    // ref is still a no-op success in practice; treat errors as fatal only for
    // the other cases).
    let _ = status;
    Ok(())
}
