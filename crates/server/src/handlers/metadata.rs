use crate::*;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

pub async fn list_op_log(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let db = state.db.clone();
    let rows = match db_run(&db, move |db| db.list_op_log(&repo_id)).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let ops: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "repo_id": r.repo_id,
                "op_type": r.op_type,
                "payload": r.payload,
                "undo_of": r.undo_of,
            })
        })
        .collect();
    Json(json!({ "ops": ops })).into_response()
}

pub async fn list_conflicts(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let db = state.db.clone();
    let rows = match db_run(&db, move |db| db.list_conflicts(&repo_id)).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let conflicts: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "repo_id": r.repo_id,
                "change_id": r.change_id,
                "path": r.path,
                "adds": serde_json::from_str::<Value>(&r.adds).unwrap_or(Value::Null),
                "removes": serde_json::from_str::<Value>(&r.removes).unwrap_or(Value::Null),
            })
        })
        .collect();
    Json(json!({ "conflicts": conflicts })).into_response()
}

pub async fn list_bookmarks(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let db = state.db.clone();
    let rows = match db_run(&db, move |db| db.list_bookmarks(&repo_id)).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let bookmarks: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "name": r.name,
                "change_id": r.change_id,
                "is_remote": r.is_remote,
            })
        })
        .collect();
    Json(json!({ "bookmarks": bookmarks })).into_response()
}

pub async fn list_change_ids(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let store = state.store.clone();
    let ids = match run_jj(move || {
        pollster::block_on(jjlab_git::sync::list_change_ids(&store, &org, &repo))
    })
    .await
    {
        Ok(i) => i,
        Err(resp) => return resp,
    };
    Json(json!({ "change_ids": ids })).into_response()
}
