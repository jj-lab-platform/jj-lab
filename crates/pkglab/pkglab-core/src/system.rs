//! The cross-protocol admin API (mounted by the embedder at
//! `/pkgs/system`): runtime upstream overrides, per-key proxy policy, and
//! package enumeration/deletion. Lives in core because it is inherently
//! cross-protocol.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use pkglab_common::auth::HeaderMap;
use pkglab_common::Registry as CommonRegistry;
use std::sync::Arc;

pub struct SystemState {
    pub registry: Arc<pkglab_common::Registry>,
    pub auth: Option<Arc<dyn pkglab_common::Auth>>,
}

pub fn router(state: Arc<SystemState>) -> Router {
    Router::new()
        .route("/upstreams", get(list_upstreams))
        .route("/upstreams/{key}", get(get_upstream).put(set_upstream).delete(reset_upstream))
        .route("/proxy", get(list_proxy))
        .route("/proxy/{key}", get(get_proxy).put(set_proxy).delete(reset_proxy))
        .route("/packages", get(list_packages).delete(delete_package))
        .with_state(state)
}

async fn authorize_write(
    auth: &Option<Arc<dyn pkglab_common::Auth>>,
    headers: &HeaderMap,
) -> Result<(), axum::response::Response> {
    if let Some(a) = auth {
        if a.authorize_write(headers).await.is_none() {
            let resp = (
                StatusCode::UNAUTHORIZED,
                [("WWW-Authenticate", "Basic realm=\"registry\"")],
                Json(serde_json::json!({"error": "authentication required"})),
            )
                .into_response();
            return Err(resp);
        }
    }
    Ok(())
}

fn lookup(reg: &CommonRegistry, key: &str) -> Option<String> {
    if let Some((format, sub)) = key.split_once('.') {
        if !format.is_empty() {
            return reg.upstreams.sub(format, sub);
        }
    }
    reg.upstreams.get(key)
}

async fn list_upstreams(State(st): State<Arc<SystemState>>) -> impl IntoResponse {
    Json(serde_json::json!({ "upstreams": st.registry.upstreams.all() }))
}

async fn get_upstream(
    State(st): State<Arc<SystemState>>,
    Path(key): Path<String>,
) -> impl IntoResponse {
    match lookup(&st.registry, &key) {
        Some(v) => Json(serde_json::json!({
            "key": key, "url": v, "override": st.registry.upstreams.is_override(&key)
        }))
        .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("unknown upstream key: {key}")})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct UrlBody {
    #[serde(default)]
    url: String,
}

async fn set_upstream(
    State(st): State<Arc<SystemState>>,
    Path(key): Path<String>,
    headers: HeaderMap,
    body: Option<Json<UrlBody>>,
) -> Result<impl IntoResponse, axum::response::Response> {
    authorize_write(&st.auth, &headers).await?;
    let url = body.map(|Json(b)| b.url).unwrap_or_default();
    if url.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "missing url"})))
            .into_response());
    }
    if lookup(&st.registry, &key).is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("unknown upstream key: {key}")})),
        )
            .into_response());
    }
    st.registry.upstreams.set(&key, &url);
    Ok(Json(serde_json::json!({"key": key, "url": url})))
}

async fn reset_upstream(
    State(st): State<Arc<SystemState>>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, axum::response::Response> {
    authorize_write(&st.auth, &headers).await?;
    st.registry.upstreams.reset(&key);
    Ok(Json(serde_json::json!({"key": key, "reset": true})))
}

async fn list_proxy(State(st): State<Arc<SystemState>>) -> impl IntoResponse {
    Json(serde_json::json!({ "proxy": st.registry.upstreams.all_proxy_states() }))
}

async fn get_proxy(
    State(st): State<Arc<SystemState>>,
    Path(key): Path<String>,
) -> impl IntoResponse {
    if lookup(&st.registry, &key).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("unknown upstream key: {key}")})),
        )
            .into_response();
    }
    Json(serde_json::json!({
        "key": key,
        "proxy": st.registry.upstreams.proxy_url(&key).unwrap_or_default()
    }))
    .into_response()
}

#[derive(serde::Deserialize, Default)]
struct ProxyBody {
    #[serde(default)]
    proxy: String,
}

async fn set_proxy(
    State(st): State<Arc<SystemState>>,
    Path(key): Path<String>,
    headers: HeaderMap,
    body: Option<Json<ProxyBody>>,
) -> Result<impl IntoResponse, axum::response::Response> {
    authorize_write(&st.auth, &headers).await?;
    if lookup(&st.registry, &key).is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("unknown upstream key: {key}")})),
        )
            .into_response());
    }
    let proxy = body.map(|Json(b)| b.proxy).unwrap_or_default();
    st.registry.upstreams.set_proxy(&key, &proxy);
    Ok(Json(serde_json::json!({
        "key": key,
        "proxy": st.registry.upstreams.proxy_url(&key).unwrap_or_default()
    })))
}

async fn reset_proxy(
    State(st): State<Arc<SystemState>>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, axum::response::Response> {
    authorize_write(&st.auth, &headers).await?;
    st.registry.upstreams.set_proxy(&key, "");
    Ok(Json(serde_json::json!({"key": key, "reset": true})))
}

async fn list_packages(State(st): State<Arc<SystemState>>) -> impl IntoResponse {
    match st.registry.meta.list_packages().await {
        Ok(pkgs) => Json(serde_json::json!({ "packages": pkgs })).into_response(),
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()})))
                .into_response()
        }
    }
}

#[derive(serde::Deserialize)]
struct RepoQuery {
    #[serde(default)]
    repo: Option<String>,
}

async fn delete_package(
    State(st): State<Arc<SystemState>>,
    Query(q): Query<RepoQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, axum::response::Response> {
    authorize_write(&st.auth, &headers).await?;
    let Some(repo) = q.repo.filter(|r| !r.trim().is_empty()) else {
        return Err((StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "missing repo"})))
            .into_response());
    };
    match st.registry.meta.delete_repo(&repo).await {
        Ok(n) => Ok(Json(serde_json::json!({"repo": repo, "deleted": n}))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response()),
    }
}
