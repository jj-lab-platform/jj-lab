use crate::*;

use axum::extract::{Path, State};
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
    let head = match run_jj(move || {
        pollster::block_on(async {
            let head =
                jjlab_git::sync::clone_remote(&store, &db, &org, &repo, &url, branch.as_deref())
                    .await?;
            jjlab_git::project::project_repo(&store, &db, &org, &repo).await?;
            Ok(head)
        })
    })
    .await
    {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    Json(json!({ "ok": true, "head": head })).into_response()
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
    let updated = match run_jj(move || {
        pollster::block_on(async {
            let updated =
                jjlab_git::sync::fetch_remote(&store, &org, &repo, &remote, &url).await?;
            jjlab_git::project::project_repo(&store, &db, &org, &repo).await?;
            Ok(updated)
        })
    })
    .await
    {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    Json(json!({ "ok": true, "updated_bookmarks": updated })).into_response()
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
    let () = match run_jj(move || {
        pollster::block_on(async {
            jjlab_git::sync::push_mirror(&store, &org, &repo, &url, &secret).await?;
            jjlab_git::project::project_repo(&store, &db, &org, &repo).await
        })
    })
    .await
    {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    Json(json!({ "ok": true })).into_response()
}