//! Sandbox file-sync: push a repo snapshot into a long-running service's
//! worker (worker-go protocol). The sandbox tools need the workspace at a
//! given rev; this endpoint materializes the tarball in-process (no HTTP
//! self-call) and streams it into the worker's `/api/v1/sync/files`.
//!
//! Synced-rev tracking is an in-process cache (name → rev): a repeated sync
//! for the same rev is skipped (`?force=1` overrides). Losing the cache on
//! restart is harmless — the worker-side overlay extract is idempotent.

use std::collections::HashMap;
use tokio::sync::Mutex;

use crate::*;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use jjlab_git::service::ServiceClient;

use super::ops::{k8s_client, resolve_ns};

/// name → last synced rev (in-process; see module docs).
#[derive(Default)]
pub struct SyncCache(Mutex<HashMap<String, String>>);

impl SyncCache {
    pub fn new() -> Self {
        Self(Mutex::new(HashMap::new()))
    }
}

/// `POST /ops/services/{name}/sync` — body `{org, repo, rev}`.
#[axum::debug_handler]
pub async fn ops_service_sync(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    Json(body): Json<ServiceSyncBody>,
) -> Response {
    let ns = match resolve_ns(&state, body.namespace.as_deref()) {
        Ok(n) => n,
        Err(e) => return e,
    };
    if body.org.is_empty() || body.repo.is_empty() || body.rev.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "org/repo/rev required".into());
    }

    // Resolve the rev to its commit sha FIRST: the cache is keyed on the
    // resolved sha (not the literal rev string), so a bookmark move ("main"
    // now points elsewhere) correctly invalidates the cached sync.
    let store2 = state.store.clone();
    let (o2, r2, rev2) = (body.org.clone(), body.repo.clone(), body.rev.clone());
    let sha = match run_jj(move || {
        pollster::block_on(jjlab_git::read::resolve_rev(&store2, &o2, &r2, &rev2)).map(|(_, sha)| sha)
    })
    .await
    {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    // Skip when the worker is already at this rev (unless forced).
    let force = q.get("force").map(|v| v == "1" || v == "true").unwrap_or(false);
    let cache_key = format!("{ns}/{name}");
    if !force {
        let cached = state.sync_cache.0.lock().await.get(&cache_key).cloned();
        if cached.as_deref() == Some(sha.as_str()) {
            return Json(json!({ "ok": true, "skipped": true, "rev": sha })).into_response();
        }
    }

    let client = match k8s_client().await {
        Ok(c) => c,
        Err(e) => return e,
    };
    let sc = ServiceClient::new(client.clone());

    // The service must exist, be ready, and expose its pod IP.
    let status = match sc.status(&name, &ns).await {
        Ok(s) => s,
        Err(e) => return json_err(StatusCode::NOT_FOUND, e.to_string()),
    };
    if !status.ready {
        return json_err(
            StatusCode::CONFLICT,
            format!("service {name} not ready (phase {})", status.phase),
        );
    }
    let port = jjlab_git::service::first_container_port_of_pod(&status, &client, &ns, &name).await;
    let worker_url = status.worker_url_on(port);
    if worker_url.is_empty() {
        return json_err(StatusCode::CONFLICT, format!("service {name} has no pod IP yet"));
    }

    // Materialize the repo tarball (blocking jj → spawn_blocking), pinned to
    // the resolved sha.
    let store = state.store.clone();
    let (org, repo, rev) = (body.org.clone(), body.repo.clone(), sha.clone());
    let ball = match run_jj(move || {
        pollster::block_on(jjlab_git::read::archive_tarball(&store, &org, &repo, &rev))
    })
    .await
    {
        Ok(b) => b,
        Err(resp) => return resp,
    };

    // Stream it into the worker's sync/files endpoint (pure overlay extract).
    let sync_url = format!(
        "{}/api/v1/sync/files?rev={}",
        worker_url,
        url::form_urlencoded::byte_serialize(body.rev.as_bytes()).collect::<String>()
    );
    let reply = reqwest::Client::new()
        .post(sync_url)
        .header("content-type", "application/gzip")
        .body(ball)
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await;
    match reply {
        Ok(resp) if resp.status().is_success() => {
            state.sync_cache.0.lock().await.insert(cache_key, sha.clone());
            let text = resp.text().await.unwrap_or_default();
            let files = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("files").and_then(|f| f.as_i64()))
                .unwrap_or(0);
            Json(json!({ "ok": true, "skipped": false, "files": files, "rev": sha }))
                .into_response()
        }
        Ok(resp) => {
            let code = resp.status();
            let text = resp.text().await.unwrap_or_default();
            json_err(
                StatusCode::BAD_GATEWAY,
                format!("worker sync {code}: {text}"),
            )
        }
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("worker sync: {e}")),
    }
}
