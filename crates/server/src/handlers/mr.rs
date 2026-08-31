use crate::*;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

fn mr_json(mr: &jjlab_core::db::MrRow, review_state: String) -> Value {
    json!({
        "number": mr.number,
        "title": mr.title,
        "body": mr.description,
        "author": mr.author,
        "state": mr.state,
        "head_change_id": mr.head_change_id,
        "head_sha": mr.head_sha,
        "base": mr.base_rev,
        "review_state": review_state,
    })
}

#[allow(clippy::result_large_err)]
fn load_mr(state: &AppState, org: &str, repo: &str, number: i64) -> Result<jjlab_core::db::MrRow, Response> {
    let repo_id = format!("{org}/{repo}");
    state
        .db
        .get_mr_by_number(&repo_id, number)
        .map_err(|e| gitea_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| gitea_err(StatusCode::NOT_FOUND, format!("pull request {number} not found")))
}

pub async fn create_mr_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<CreateMrBody>,
) -> Response {
    // Resolve head rev to its change-id + sha so force-pushes can re-associate.
    let store = state.store.clone();
    let head = body.head.clone();
    let (o2, r2) = (org.clone(), repo.clone());
    let resolved = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::resolve_rev(&store, &o2, &r2, &head))
    })
    .await;
    let (head_change_id, head_sha) = match resolved {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let repo_id = format!("{org}/{repo}");
    let _ = state.db.upsert_org(&org, &org);
    let _ = state.db.upsert_repo(&repo_id, &org, &repo, "main", None);
    match state.db.create_mr(
        &repo_id,
        &body.title,
        &body.body,
        &server_author().0,
        &head_change_id,
        Some(&head_sha),
        Some(&body.head),
        &body.base,
    ) {
        Ok(mr) => (
            StatusCode::CREATED,
            Json(mr_json(&mr, "pending".to_string())),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn list_mrs(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let mrs = match state.db.list_mrs(&repo_id, q.get("state").map(String::as_str)) {
        Ok(v) => v,
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let mut items = Vec::new();
    for mr in mrs {
        let rs = state.db.mr_review_state(mr.id).unwrap_or_default();
        items.push(mr_json(&mr, rs));
    }
    Json(json!({ "pull_requests": items })).into_response()
}

pub async fn get_mr_handler(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    let rs = state.db.mr_review_state(mr.id).unwrap_or_default();
    Json(mr_json(&mr, rs)).into_response()
}

pub async fn update_mr_handler(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<UpdateMrBody>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    match state.db.update_mr(mr.id, Some(&body.state), None, None) {
        Ok(updated) => {
            let rs = state.db.mr_review_state(mr.id).unwrap_or_default();
            Json(mr_json(&updated, rs)).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn add_review(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<ReviewBody>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    match state.db.add_mr_review(
        mr.id,
        &server_author().0,
        &body.state,
        &body.body,
        mr.head_sha.as_deref(),
    ) {
        Ok(_) => {
            let rs = state.db.mr_review_state(mr.id).unwrap_or_default();
            Json(json!({ "review_state": rs })).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn list_reviews(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    match state.db.list_mr_reviews(mr.id) {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    json!({
                        "reviewer": r.reviewer,
                        "state": r.state,
                        "body": r.body,
                        "commit_sha": r.commit_sha,
                    })
                })
                .collect();
            Json(json!({ "reviews": items })).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn add_comment(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<CommentBody>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    match state.db.add_mr_comment(
        mr.id,
        &server_author().0,
        &body.body,
        body.path.as_deref(),
        mr.head_sha.as_deref(),
    ) {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn list_comments(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    match state.db.list_mr_comments(mr.id) {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    json!({
                        "author": r.author,
                        "body": r.body,
                        "path": r.path,
                        "commit_sha": r.commit_sha,
                    })
                })
                .collect();
            Json(json!({ "comments": items })).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// Unified diff between MR base and head.
pub async fn mr_diff(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    let Some(head_sha) = mr.head_sha.clone() else {
        return json_err(StatusCode::CONFLICT, "mr has no head sha".into());
    };
    let store = state.store.clone();
    let base = mr.base_rev.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::compare_patch(&store, &org, &repo, &base, &head_sha))
    })
    .await;
    match r {
        Ok(Ok(patch)) => Json(json!({ "diff": patch })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
