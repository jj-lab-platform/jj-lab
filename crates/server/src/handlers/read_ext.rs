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
    // Pagination uses offset+limit (skip) rather than page, so callers can
    // page deterministically; `page` is still accepted for backward compat.
    let limit: usize = q.get("limit").and_then(|v| v.parse().ok()).unwrap_or(20);
    let offset: usize = q.get("offset").and_then(|v| v.parse().ok()).unwrap_or_else(|| {
        q.get("page")
            .and_then(|v| v.parse::<usize>().ok())
            .map(|p| p.saturating_sub(1).saturating_mul(limit))
            .unwrap_or(0)
    });
    let rev = q.get("sha").cloned().or_else(|| q.get("rev").cloned());
    let since = match q.get("since") {
        Some(s) => match jjlab_git::read::parse_time_bound(s) {
            Ok(ms) => Some(ms),
            Err(e) => return json_err(StatusCode::BAD_REQUEST, e.to_string()),
        },
        None => None,
    };
    let until = match q.get("until") {
        Some(s) => match jjlab_git::read::parse_time_bound(s) {
            Ok(ms) => Some(ms),
            Err(e) => return json_err(StatusCode::BAD_REQUEST, e.to_string()),
        },
        None => None,
    };
    let store = state.store.clone();
    let (items, total) = match run_jj(move || {
        pollster::block_on(jjlab_git::read::commit_log(
            &store,
            &org,
            &repo,
            rev.as_deref(),
            since,
            until,
            offset,
            limit,
        ))
    })
    .await
    {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    Json(json!({ "total_count": total, "commits": items })).into_response()
}

pub async fn tags_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let store = state.store.clone();
    let tags = match run_jj(move || pollster::block_on(jjlab_git::read::tags(&store, &org, &repo))).await {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    Json(json!({ "tags": tags })).into_response()
}

pub async fn refs_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let store = state.store.clone();
    let refs = match run_jj(move || pollster::block_on(jjlab_git::read::all_refs(&store, &org, &repo))).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let items: Vec<Value> = refs
        .into_iter()
        .map(|(name, sha)| json!({ "ref": name, "sha": sha }))
        .collect();
    Json(json!({ "refs": items })).into_response()
}

pub async fn contents_list_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let sha = q.get("ref").cloned().unwrap_or_default();
    let store = state.store.clone();
    match run_jj(move || {
        pollster::block_on(jjlab_git::read::contents_dir_at(
            &store, &org, &repo, &sha, "",
        ))
        .map(|entries| Json(json!({ "entries": entries })).into_response())
    })
    .await
    {
        Ok(resp) => resp,
        Err(resp) => resp,
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
    match run_jj(move || {
        if is_dir {
            pollster::block_on(jjlab_git::read::contents_dir_at(
                &store, &org, &repo, &sha, &path,
            ))
            .map(|entries| Json(json!({ "entries": entries })).into_response())
        } else {
            pollster::block_on(jjlab_git::read::contents_entry(&store, &org, &repo, &sha, &path))
                .map(|v| Json(v).into_response())
        }
    })
    .await
    {
        Ok(resp) => resp,
        Err(resp) => resp,
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
    let patch = match run_jj(move || {
        pollster::block_on(jjlab_git::read::compare_patch(&store, &org, &repo, &base, &head))
    })
    .await
    {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    Json(json!({ "diff": patch })).into_response()
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
    let bytes = match run_jj(move || {
        pollster::block_on(jjlab_git::read::archive_tarball(&store, &o2, &r2, &s2))
    })
    .await
    {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    (
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
        .into_response()
}