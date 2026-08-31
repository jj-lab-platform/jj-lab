use crate::*;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// SSE stream of op-log events for a repo. Query: `after=<op_id>` to catch up.
pub async fn subscribe_ops(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let repo_id = format!("{org}/{repo}");

    // Catch-up replay (after=op_id) sent inline before live polling loop.
    let after = q.get("after").cloned();
    let db = state.db.clone();

    let stream = async_stream::stream! {
        if let Some(after_id) = &after {
            for ev in jjlab_git::ops::ops_since(&db, &repo_id, after_id) {
                yield Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default()
                    .event("op")
                    .id(ev.id.clone())
                    .data(serde_json::json!({
                        "id": ev.id,
                        "repo_id": ev.repo_id,
                        "op_type": ev.op_type,
                        "payload": ev.payload,
                        "undo_of": ev.undo_of,
                    }).to_string()));
            }
        }
        // Live tail: poll the DB on an interval (simple, no broadcast bus yet).
        let mut seen = after;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let Ok(rows) = db.list_op_log(&repo_id) else { continue };
            let start = rows.iter().position(|r| Some(&r.id) == seen.as_ref()).map(|i| i + 1).unwrap_or(0);
            for ev in rows.into_iter().skip(start) {
                seen = Some(ev.id.clone());
                yield Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default()
                    .event("op")
                    .id(ev.id.clone())
                    .data(serde_json::json!({
                        "id": ev.id,
                        "repo_id": ev.repo_id,
                        "op_type": ev.op_type,
                        "payload": ev.payload,
                        "undo_of": ev.undo_of,
                    }).to_string()));
            }
        }
    };

    axum::response::Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response()
}

pub async fn undo_op(
    State(state): State<AppState>,
    Path((org, repo, op_id)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::ops::undo_operation(&store, &db, &org, &repo, &op_id))
    })
    .await;
    match r {
        Ok(Ok(ev)) => Json(json!({
            "ok": true,
            "undo_op_id": ev.id,
            "undo_of": ev.undo_of,
        }))
        .into_response(),
        Ok(Err(e)) => json_err(StatusCode::CONFLICT, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn op_head(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::ops::current_op_id(&store, &org, &repo))
    })
    .await;
    match r {
        Ok(Ok(id)) => Json(json!({ "op_id": id })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
