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
                // the view into SQLite.
                let store = state.store.clone();
                let db = state.db.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    pollster::block_on(jjlab_git::sync::import_after_receive(&store, &db, &org, &repo))
                })
                .await;
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
