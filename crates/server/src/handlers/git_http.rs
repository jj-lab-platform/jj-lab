use crate::*;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

fn smart_http_error(status: StatusCode, msg: String) -> Response {
    (status, [("content-type", "text/plain")], msg).into_response()
}

pub async fn smart_http_info_refs(
    state: State<AppState>,
    org: String,
    repo: String,
    headers: axum::http::HeaderMap,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> Response {
    let Some(query) = query else {
        return smart_http_error(StatusCode::BAD_REQUEST, "missing service param".into());
    };
    let service_param = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("service="))
        .map(|s| s.replace("%20", " "));
    let Some(service) = jjlab_git::http::GitService::from_service_param(service_param.as_deref())
    else {
        return smart_http_error(StatusCode::BAD_REQUEST, "unknown service".into());
    };

    // Push advertisement requires write token (git sends basic auth).
    if service == jjlab_git::http::GitService::ReceivePack {
        if let Err(resp) = require_auth(&state, &headers, Level::Write) {
            return resp;
        }
    }

    let git_dir = match jjlab_git::http::git_dir_of(&state.store, &org, &repo) {
        Ok(d) => d,
        Err(e) => return smart_http_error(StatusCode::NOT_FOUND, e.to_string()),
    };
    match jjlab_git::http::advertise_refs(service, &git_dir).await {
        Ok(pkt) => (
            StatusCode::OK,
            [
                ("content-type", service.content_type("advertisement").as_str()),
                ("cache-control", "no-cache"),
            ],
            pkt,
        )
            .into_response(),
        Err(e) => smart_http_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
/// Per-repo import serialization: git's HTTP push protocol may run two
/// receive-pack RPCs back-to-back (empirically observed); concurrent jj imports
/// on the same repo panic ("Descendants have not been rebased") and leave the
/// view stale. A keyed mutex makes the second import wait, then no-op cleanly.
fn import_locks() -> &'static std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>> {
    static LOCKS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>> = std::sync::OnceLock::new();
    LOCKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

pub async fn smart_http_rpc(
    state: State<AppState>,
    org: String,
    repo: String,
    svc: String,
    headers: axum::http::HeaderMap,
    body: axum::body::Body,
) -> Response {
    let service = match svc.as_str() {
        "git-upload-pack" => jjlab_git::http::GitService::UploadPack,
        "git-receive-pack" => jjlab_git::http::GitService::ReceivePack,
        _ => return smart_http_error(StatusCode::NOT_FOUND, format!("unknown service {svc}")),
    };

    if service == jjlab_git::http::GitService::ReceivePack {
        if let Err(resp) = require_auth(&state, &headers, Level::Write) {
            return resp;
        }
    }

    let git_dir = match jjlab_git::http::git_dir_of(&state.store, &org, &repo) {
        Ok(d) => d,
        Err(e) => return smart_http_error(StatusCode::NOT_FOUND, e.to_string()),
    };

    let req_headers = headers.clone();
    let content_type = service.content_type("request");
    if !req_headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v == content_type)
        .unwrap_or(false)
    {
        return smart_http_error(
            StatusCode::BAD_REQUEST,
            format!("content-type must be {content_type}"),
        );
    }

    let body_bytes = match axum::body::to_bytes(body, 512 * 1024 * 1024).await {
        Ok(b) => b.to_vec(),
        Err(e) => return smart_http_error(StatusCode::BAD_REQUEST, format!("read body: {e}")),
    };
    match jjlab_git::http::run_rpc(service, &git_dir, body_bytes).await {
        Ok((out, _err, ok)) => {
            if service == jjlab_git::http::GitService::ReceivePack && ok {
                // Received a pack: import refs so the pushed commits become
                // native jj changes (change-id header preserved), then project
                // the view into SQLite. Serialized per repo (see import_locks);
                // failures are logged (never silently swallowed) — a stale jj
                // view after a successful push is a real bug and must be
                // visible.
                let store = state.store.clone();
                let db = state.db.clone();
                let (org2, repo2) = (org.clone(), repo.clone());
                let org = org.clone();
                let repo = repo.clone();
                let key = format!("{org}/{repo}");
                let lock = import_locks()
                    .lock()
                    .unwrap()
                    .entry(key)
                    .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
                    .clone();
                let _guard = lock.lock().await;
                let imported = run_jj(move || {
                    pollster::block_on(jjlab_git::sync::import_after_receive(
                        &store,
                        &db,
                        &org,
                        &repo,
                    ))
                })
                .await;
                if let Err(resp) = imported {
                    tracing::error!(org = %org2, repo = %repo2, "receive-pack: jj import failed: {}", pretty_resp(resp).await);
                } else {
                    tracing::info!(org = %org2, repo = %repo2, "receive-pack: jj import ok");
                }
            }
            (
                StatusCode::OK,
                [
                    ("content-type", service.content_type("result").as_str()),
                    ("cache-control", "no-cache"),
                ],
                out,
            )
                .into_response()
        }
        Err(e) => smart_http_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn smart_http_get(
    state: State<AppState>,
    Path((org, git_repo_path)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    query: axum::extract::RawQuery,
) -> Response {
    let Some((rest, tail)) = git_repo_path.split_once('/') else {
        return smart_http_error(StatusCode::NOT_FOUND, "not a git endpoint".into());
    };
    if tail != "info/refs" {
        return smart_http_error(StatusCode::NOT_FOUND, "not a git endpoint".into());
    }
    let Some(repo) = rest.strip_suffix(".git") else {
        return smart_http_error(StatusCode::NOT_FOUND, "not a git endpoint".into());
    };
    smart_http_info_refs(state, org, repo.to_string(), headers, query).await
}

pub async fn smart_http_post(
    state: State<AppState>,
    Path((org, git_repo_path)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    body: axum::body::Body,
) -> Response {
    let Some((rest, tail)) = git_repo_path.split_once('/') else {
        return smart_http_error(StatusCode::NOT_FOUND, "not a git endpoint".into());
    };
    let Some(repo) = rest.strip_suffix(".git") else {
        return smart_http_error(StatusCode::NOT_FOUND, "not a git endpoint".into());
    };
    let svc = match tail {
        t if t == "git-upload-pack" || t == "upload-pack" => "git-upload-pack".to_string(),
        t if t == "git-receive-pack" || t == "receive-pack" => "git-receive-pack".to_string(),
        _ => return smart_http_error(StatusCode::NOT_FOUND, "not a git endpoint".into()),
    };
    smart_http_rpc(state, org, repo.to_string(), svc, headers, body).await
}

/// Render an axum `Response` for logging: status + JSON message field when
/// present (run_jj errors arrive as Gitea-style `{"message": ...}`).
async fn pretty_resp(resp: axum::response::Response) -> String {
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap_or_default();
    let msg = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
        .unwrap_or_else(|| String::from_utf8_lossy(&body).to_string());
    format!("HTTP {status}: {msg}")
}
