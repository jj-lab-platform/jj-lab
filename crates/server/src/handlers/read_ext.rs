use crate::*;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

pub async fn commit_log_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let page: usize = q.get("page").and_then(|v| v.parse().ok()).unwrap_or(1);
    let limit: usize = q.get("limit").and_then(|v| v.parse().ok()).unwrap_or(20);
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::commit_log(&store, &org, &repo, page.saturating_sub(1), limit))
    })
    .await;
    match r {
        Ok(Ok((items, total))) => Json(json!({ "total_count": total, "commits": items })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn tags_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::tags(&store, &org, &repo))
    })
    .await;
    match r {
        Ok(Ok(tags)) => Json(json!({ "tags": tags })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn refs_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::all_refs(&store, &org, &repo))
    })
    .await;
    match r {
        Ok(Ok(refs)) => {
            let items: Vec<Value> = refs
                .into_iter()
                .map(|(name, sha)| json!({ "ref": name, "sha": sha }))
                .collect();
            Json(json!({ "refs": items })).into_response()
        }
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn contents_handler(
    State(state): State<AppState>,
    Path((org, repo, path)): Path<(String, String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let sha = q.get("ref").cloned().unwrap_or_default();
    let store = state.store.clone();
    let is_dir = path.ends_with('/') || path.is_empty();
    let r = tokio::task::spawn_blocking(move || {
        if is_dir {
            pollster::block_on(jjlab_git::read::contents_dir(&store, &org, &repo, &sha))
                .map(|entries| Json(json!({ "entries": entries })).into_response())
        } else {
            pollster::block_on(jjlab_git::read::contents_entry(&store, &org, &repo, &sha, &path))
                .map(|v| Json(v).into_response())
        }
    })
    .await;
    match r {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn compare_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (Some(base), Some(head)) = (q.get("base").cloned(), q.get("head").cloned()) else {
        return json_err(StatusCode::BAD_REQUEST, "base and head required".into());
    };
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::compare_patch(&store, &org, &repo, &base, &head))
    })
    .await;
    match r {
        Ok(Ok(patch)) => Json(json!({ "diff": patch })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn archive_handler(
    State(state): State<AppState>,
    Path((org, repo, ball_type, sha)): Path<(String, String, String, String)>,
) -> Response {
    if ball_type != "tarball" {
        return json_err(StatusCode::BAD_REQUEST, "only tarball supported".into());
    }
    let store = state.store.clone();
    let (o2, r2, s2) = (org.clone(), repo.clone(), sha.clone());
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::archive_tarball(&store, &o2, &r2, &s2))
    })
    .await;
    match r {
        Ok(Ok(bytes)) => (
            StatusCode::OK,
            [
                ("content-type", "application/gzip"),
                (
                    "content-disposition",
                    &format!("attachment; filename=\"{repo}-{sha}.tar.gz\"")[..],
                ),
            ],
            bytes,
        )
            .into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}


pub async fn contents_root_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let sha = q.get("ref").cloned().unwrap_or_default();
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::contents_dir(&store, &org, &repo, &sha))
    })
    .await;
    match r {
        Ok(Ok(entries)) => Json(json!({ "entries": entries })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
