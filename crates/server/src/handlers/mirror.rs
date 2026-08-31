use crate::*;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

pub async fn clone_remote(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<SyncBody>,
) -> Response {
    let url = body.url.clone();
    let branch = body.branch.clone();
    let store = state.store.clone();
    let db = state.db.clone();
    match tokio::task::spawn_blocking(move || {
        (|| {
            pollster::block_on(jjlab_git::sync::clone_remote(
                &store,
                &db,
                &org,
                &repo,
                &url,
                branch.as_deref(),
            ))?;
            pollster::block_on(jjlab_git::project::project_repo(&store, &db, &org, &repo))
        })()
    })
    .await
    {
        Ok(Ok(head)) => Json(json!({ "ok": true, "head": head })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn fetch_remote(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<SyncBody>,
) -> Response {
    let remote = body.remote.clone().unwrap_or_else(|| "origin".to_string());
    let url = body.url.clone();
    let store = state.store.clone();
    let db = state.db.clone();
    match tokio::task::spawn_blocking(move || {
        (|| {
            let updated =
                pollster::block_on(jjlab_git::sync::fetch_remote(&store, &org, &repo, &remote, &url))?;
            pollster::block_on(jjlab_git::project::project_repo(&store, &db, &org, &repo))?;
            Ok::<usize, jjlab_git::repo::RepoError>(updated)
        })()
    })
    .await
    {
        Ok(Ok(updated)) => Json(json!({ "ok": true, "updated_bookmarks": updated })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn push_mirror(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<SyncBody>,
) -> Response {
    let url = body.url.clone();
    let secret = body.secret.clone();
    let store = state.store.clone();
    let db = state.db.clone();
    match tokio::task::spawn_blocking(move || {
        (|| {
            pollster::block_on(jjlab_git::sync::push_mirror(&store, &org, &repo, &url, &secret))?;
            pollster::block_on(jjlab_git::project::project_repo(&store, &db, &org, &repo))
        })()
    })
    .await
    {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
