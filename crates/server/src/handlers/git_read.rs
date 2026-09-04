use crate::*;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

pub async fn commit_info(
    State(state): State<AppState>,
    Path((org, repo, sha)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let info = match run_jj(move || {
        pollster::block_on(jjlab_git::read::commit_by_sha(&store, &org, &repo, &sha))
    })
    .await
    {
        Ok(i) => i,
        Err(resp) => return resp,
    };
    Json(json!(info)).into_response()
}

/// `GET /repos/{org}/{repo}/git/commits/{sha}/diff` — the commit's diff vs its
/// merged parent trees (`jj show` semantics), the form a "what did this change
/// do" view resolves to.
pub async fn commit_diff(
    State(state): State<AppState>,
    Path((org, repo, sha)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let patch = match run_jj(move || {
        pollster::block_on(jjlab_git::read::commit_patch_merged(
            &store, &org, &repo, &sha,
        ))
    })
    .await
    {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    Json(json!({ "diff": patch })).into_response()
}

/// `GET /repos/{org}/{repo}/annotate/{*path}?rev=` — line-by-line origin
/// annotation (jj-native `file annotate`). `rev` is a strict snapshot
/// (sha/bookmark/tag); each line reports the origin commit + its change-id.
pub async fn annotate_file(
    State(state): State<AppState>,
    Path((org, repo, path)): Path<(String, String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let rev = q.get("rev").cloned().unwrap_or_default();
    let store = state.store.clone();
    let lines = match run_jj(move || {
        pollster::block_on(jjlab_git::read::annotate_file(
            &store, &org, &repo, &rev, &path,
        ))
    })
    .await
    {
        Ok(l) => l,
        Err(resp) => return resp,
    };
    Json(json!({ "annotations": lines })).into_response()
}


pub async fn raw_file(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let rev = q.get("ref").cloned().unwrap_or_default();
    let Some(path) = q.get("path").cloned() else {
        return json_err(StatusCode::BAD_REQUEST, "path required".into());
    };
    let store = state.store.clone();
    match run_jj(move || {
        pollster::block_on(jjlab_git::read::read_file_at(&store, &org, &repo, &rev, &path))
    })
    .await
    {
        Ok(bytes) => bytes.into_response(),
        Err(resp) => resp,
    }
}

// ── change list (rev-anchored, like commits/files) ──

/// `GET /repos/{org}/{repo}/changes?rev=` — list changes reachable from a
/// snapshot rev. Each change-id resolves to a single visible commit in that
/// rev's history (no cross-branch divergence ambiguity).
pub async fn list_changes(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let rev = q.get("rev").cloned().unwrap_or_default();
    let store = state.store.clone();
    let changes = match run_jj(move || {
        pollster::block_on(jjlab_git::read::list_changes(
            &store, &org, &repo, &rev,
        ))
    })
    .await
    {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    Json(json!({ "changes": changes })).into_response()
}