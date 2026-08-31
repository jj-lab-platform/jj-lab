use crate::*;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

struct OrgRepoEntry {
    name: String,
    default_bookmark: String,
}

pub async fn list_orgs(State(state): State<AppState>) -> Response {
    let Ok(orgs) = state.db.list_orgs() else {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "list orgs".into());
    };
    let mut items: Vec<Value> = Vec::new();
    for (id, name) in orgs {
        let repos = match state.db.list_repos() {
            Ok(all) => all
                .into_iter()
                .filter(|r| r.org_id == id)
                .map(|r| OrgRepoEntry { name: r.name, default_bookmark: r.default_bookmark })
                .collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        };
        items.push(json!({
            "org": name,
            "repos": repos.into_iter().map(|r| json!({
                "repo": r.name,
                "default_bookmark": r.default_bookmark,
            })).collect::<Vec<_>>(),
        }));
    }
    Json(json!({ "orgs": items })).into_response()
}

// ── graph / history / search reads ──

pub async fn graph_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let limit: usize = q.get("limit").and_then(|v| v.parse().ok()).unwrap_or(100);
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::change_graph(&store, &org, &repo, limit))
    })
    .await;
    match r {
        Ok(Ok(nodes)) => Json(json!({ "graph": nodes })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn file_log_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(path) = q.get("path").cloned() else {
        return json_err(StatusCode::BAD_REQUEST, "path required".into());
    };
    let limit: usize = q.get("limit").and_then(|v| v.parse().ok()).unwrap_or(50);
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::file_log(&store, &org, &repo, &path, limit))
    })
    .await;
    match r {
        Ok(Ok((items, total))) => Json(json!({ "total_count": total, "commits": items })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn search_handler(
    State(state): State<AppState>,
    Path((org, repo, _rev)): Path<(String, String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let rev = q.get("ref").cloned().unwrap_or_default();
    let Some(pattern) = q.get("pattern").cloned().filter(|p| !p.is_empty()) else {
        return json_err(StatusCode::BAD_REQUEST, "pattern required".into());
    };
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::search_code(&store, &org, &repo, &rev, &pattern))
    })
    .await;
    match r {
        Ok(Ok(matches)) => Json(json!({ "matches": matches })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── static SPA ──

/// Serve the embedded frontend. `/` and non-asset paths fall back to
/// index.html (client-side hash routing); asset paths resolve within `dist`.
pub async fn spa(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/').to_string();
    let candidate = if path.is_empty() || !path.contains('.') {
        "index.html".to_string()
    } else {
        path.clone()
    };
    const DIST: include_dir::Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../dist");
    if let Some(file) = DIST.get_file(candidate.as_str()) {
        let ext = candidate.rsplit('.').next().unwrap_or("");
        let ct = match ext {
            "html" => "text/html",
            "js" => "text/javascript",
            "mjs" => "text/javascript",
            "css" => "text/css",
            "json" => "application/json",
            "svg" => "image/svg+xml",
            "png" => "image/png",
            "ico" => "image/x-icon",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            "txt" => "text/plain",
            "map" => "application/json",
            _ => "application/octet-stream",
        };
        return (
            [(axum::http::header::CONTENT_TYPE, ct)],
            file.contents(),
        )
            .into_response();
    }
    if let Some(file) = DIST.get_file("index.html") {
        return (
            [(axum::http::header::CONTENT_TYPE, "text/html")],
            file.contents(),
        )
            .into_response();
    }
    (StatusCode::NOT_FOUND, "not found").into_response()
}
