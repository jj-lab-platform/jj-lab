//! Generic raw-artifact store: `/{name}/{version}/{filename}`.
//!
//! - `PUT  /{name}/{version}/{filename}`  upload (appends the file to the
//!   version's blob set)
//! - `GET  /{name}/{version}/{filename}`  download
//! - `DELETE /{name}.version`             remove every version of a package
//!
//! No upstream: generic artifacts are local-only by definition.

use axum::body::Body;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pkglab_common::httphelpers::blob_response;
use pkglab_common::Descriptor;
use std::io::Cursor;
use std::sync::Arc;

pub struct GenericState {
    pub registry: Arc<pkglab_common::Registry>,
    pub auth: Option<Arc<dyn pkglab_common::Auth>>,
}

fn json_error(status: StatusCode, msg: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({"ok": false, "error": msg}).to_string(),
    )
        .into_response()
}

async fn authorize_write(state: &GenericState, headers: &HeaderMap) -> Result<(), Response> {
    pkglab_common::httphelpers::authorize_write(&state.auth, headers).await
}

pub fn router(state: Arc<GenericState>) -> axum::Router {
    let s0 = state.clone();
    axum::Router::new()
        .route(
            "/{*path}",
            axum::routing::any(move |req: axum::http::Request<Body>| {
                let st = s0.clone();
                async move {
                    let method = req.method().clone();
                    let headers = req.headers().clone();
                    let path = req.uri().path().trim_start_matches('/').to_string();
                    let path = path
                        .strip_prefix("pkgs/generic/")
                        .or_else(|| path.strip_prefix("generic/"))
                        .unwrap_or(&path)
                        .to_string();
                    let body = req.into_body();
                    Ok::<_, std::convert::Infallible>(
                        dispatch(st, method, &path, headers, body).await,
                    )
                }
            }),
        )
        .with_state(state)
}

async fn dispatch(
    state: Arc<GenericState>,
    method: axum::http::Method,
    path: &str,
    headers: HeaderMap,
    body: Body,
) -> Response {
    // DELETE {name}.version -> remove every version of the package.
    if let Some(name) = path.strip_suffix(".version") {
        if method == axum::http::Method::DELETE {
            return delete_version(state, name.to_string(), headers).await;
        }
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return json_error(StatusCode::NOT_FOUND, "not found");
    }
    let (name, version, filename) =
        (parts[0].to_string(), parts[1].to_string(), parts[2].to_string());
    match method {
        axum::http::Method::GET => download(state, name, version, filename).await,
        axum::http::Method::HEAD => {
            // HEAD: same status/headers, empty body.
            let resp = download(state, name, version, filename).await;
            let (parts, _) = resp.into_parts();
            axum::http::Response::from_parts(parts, Body::empty())
        }
        axum::http::Method::PUT => upload(state, name, version, filename, headers, body).await,
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

async fn download(
    st: Arc<GenericState>,
    name: String,
    version: String,
    filename: String,
) -> Response {
    let art = match st.registry.meta.get("generic", &name, &version).await {
        Ok(a) => a,
        Err(_) => return json_error(StatusCode::NOT_FOUND, "not found"),
    };
    for b in &art.blobs {
        if b.name != filename {
            continue;
        }
        if let Ok(Some(mut reader)) = st.registry.blobs.open(&b.digest).await {
            let mut data = Vec::new();
            if std::io::Read::read_to_end(&mut reader, &mut data).is_ok() {
                return blob_response(data, &filename);
            }
        }
    }
    json_error(StatusCode::NOT_FOUND, "not found")
}

async fn upload(
    st: Arc<GenericState>,
    name: String,
    version: String,
    filename: String,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    let mut body = body;
    let data = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(c) => c.to_bytes().to_vec(),
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "read error"),
    };
    store(&st.registry, &name, &version, &filename, data).await;
    (
        StatusCode::CREATED,
        [(header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({"ok": true}).to_string(),
    )
        .into_response()
}

async fn delete_version(st: Arc<GenericState>, name: String, headers: HeaderMap) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    if let Ok(versions) = st.registry.meta.list_versions("generic", &name).await {
        for v in versions {
            let _ = st.registry.meta.delete("generic", &name, &v).await;
        }
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({"ok": true}).to_string(),
    )
        .into_response()
}

/// Store a file into the version's blob set (append semantics, like the
/// reference implementation).
pub async fn store(
    reg: &pkglab_common::Registry,
    name: &str,
    version: &str,
    filename: &str,
    data: Vec<u8>,
) {
    let mut art = reg.meta.get("generic", name, version).await.unwrap_or_default();
    art.format = "generic".into();
    art.repository = name.to_string();
    art.version = version.to_string();
    if !data.is_empty() {
        if let Ok((hashes, size)) = pkglab_common::artifact::compute_hashes(Cursor::new(&data)) {
            let digest = format!("sha256:{}", hashes.sha256);
            // Replace (not append) any same-named descriptor so a re-publish
            // is an overwrite, never a second dangling entry.
            let removed: Vec<Descriptor> =
                art.blobs.iter().filter(|b| b.name == filename).cloned().collect();
            art.blobs.retain(|b| b.name != filename);
            let mut cursor = Cursor::new(data);
            if reg.blobs.put_if_absent(&digest, &mut cursor).await.is_ok() {
                art.blobs.push(Descriptor {
                    digest: digest.clone(),
                    size: size as i64,
                    name: filename.to_string(),
                    ..Default::default()
                });
            }
            for b in removed {
                if b.digest != digest {
                    let _ = reg.blobs.delete(&b.digest).await;
                }
            }
        }
    }
    let _ = reg.meta.put(art).await;
}
