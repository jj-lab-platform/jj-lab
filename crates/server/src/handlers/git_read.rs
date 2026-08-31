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
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::commit_by_sha(&store, &org, &repo, &sha))
    })
    .await;
    match r {
        Ok(Ok(info)) => Json(json!(info)).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn list_branches(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::branches(&store, &org, &repo))
    })
    .await;
    match r {
        Ok(Ok(branches)) => Json(json!({ "branches": branches })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn raw_file(
    State(state): State<AppState>,
    Path((org, repo, path)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::raw_at_head(&store, &org, &repo, &path))
    })
    .await;
    match r {
        Ok(Ok(bytes)) => bytes.into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn tree_at_sha(
    State(state): State<AppState>,
    Path((org, repo, sha)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::tree_at_sha(&store, &org, &repo, &sha))
    })
    .await;
    match r {
        Ok(Ok(entries)) => Json(json!({ "tree": entries })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── jj-native: change addressed read ──

pub async fn change_info(
    State(state): State<AppState>,
    Path((org, repo, change_id)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::change_info(
            &store,
            &org,
            &repo,
            &change_id,
        ))
    })
    .await;
    match r {
        Ok(Ok(info)) => Json(json!(info)).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
