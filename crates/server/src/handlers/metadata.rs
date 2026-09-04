use crate::*;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

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

/// `GET /repos/{org}/{repo}/bookmarks` — merged bookmark view: real-time git
/// tips (name + sha from `read::bookmarks`) joined with the DB projection
/// (`is_remote`). `change_id` is NOT returned — it is derivable live from jj
/// and no longer stored per-bookmark. A DB row whose git ref vanished is
/// dropped; bookmarks absent from the DB default to is_remote=false.
pub async fn list_bookmarks(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let store = state.store.clone();
    let db = state.db.clone();
    let r = run_jj(move || {
        let org = org.clone();
        let repo = repo.clone();
        pollster::block_on(async {
            let live = jjlab_git::read::bookmarks(&store, &org, &repo).await?;
            let rows = db.list_bookmarks(&repo_id).map_err(|e| {
                jjlab_git::repo::RepoError::Other(e.to_string())
            })?;
            let remote: std::collections::HashMap<String, bool> = rows
                .into_iter()
                .map(|r| (r.name.clone(), r.is_remote))
                .collect();
            let mut out = Vec::new();
            for b in live {
                out.push(serde_json::json!({
                    "name": b.name,
                    "sha": b.sha,
                    "is_remote": remote.get(&b.name).copied().unwrap_or(false),
                }));
            }
            Ok::<_, jjlab_git::repo::RepoError>(out)
        })
    })
    .await;
    match r {
        Ok(bookmarks) => Json(json!({ "bookmarks": bookmarks })).into_response(),
        Err(resp) => resp,
    }
}
