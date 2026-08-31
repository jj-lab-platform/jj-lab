use crate::*;

use axum::extract::{Path, State};
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

pub async fn list_branches(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let store = state.store.clone();
    let branches = match run_jj(move || {
        pollster::block_on(jjlab_git::read::branches(&store, &org, &repo))
    })
    .await
    {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    Json(json!({ "branches": branches })).into_response()
}

pub async fn raw_file(
    State(state): State<AppState>,
    Path((org, repo, path)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    match run_jj(move || {
        pollster::block_on(jjlab_git::read::raw_at_head(&store, &org, &repo, &path))
    })
    .await
    {
        Ok(bytes) => bytes.into_response(),
        Err(resp) => resp,
    }
}

pub async fn tree_at_sha(
    State(state): State<AppState>,
    Path((org, repo, sha)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let entries = match run_jj(move || {
        pollster::block_on(jjlab_git::read::tree_at_sha(&store, &org, &repo, &sha))
    })
    .await
    {
        Ok(e) => e,
        Err(resp) => return resp,
    };
    Json(json!({ "tree": entries })).into_response()
}

// ── jj-native: change addressed read ──

pub async fn change_info(
    State(state): State<AppState>,
    Path((org, repo, change_id)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let info = match run_jj(move || {
        pollster::block_on(jjlab_git::read::change_info(
            &store,
            &org,
            &repo,
            &change_id,
        ))
    })
    .await
    {
        Ok(i) => i,
        Err(resp) => return resp,
    };
    Json(json!(info)).into_response()
}