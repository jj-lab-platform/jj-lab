//! Cargo (crates.io) protocol: sparse index (upstream page rewritten + local
//! overlay), config.json, JSON API search, publish (binary framing:
//! u32-le JSON len + metadata + u32-le crate len), download with pull-through
//! from static.crates.io, yank/unyank, owners.
use pkglab_common::httphelpers::urlencode;
use pkglab_common::httphelpers::{blob_response, error, json};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::put;
use pkglab_common::{Artifact, Descriptor};
use std::io::Cursor;
use std::sync::Arc;

pub struct CargoState {
    pub registry: Arc<pkglab_common::Registry>,
    pub auth: Option<Arc<dyn pkglab_common::Auth>>,
    pub self_base: String,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct CargoMeta {
    #[serde(default)]
    yanked: std::collections::HashMap<String, bool>,
    #[serde(default)]
    owners: Vec<String>,
}

fn text(status: StatusCode, body: String) -> Response {
    (status, [(header::CONTENT_TYPE, "text/plain".to_string())], body).into_response()
}

async fn authorize_write(state: &CargoState, headers: &HeaderMap) -> Result<(), Response> {
    pkglab_common::httphelpers::authorize_write(&state.auth, headers).await
}

async fn load_meta(state: &CargoState, name: &str) -> CargoMeta {
    let mut m = CargoMeta::default();
    if let Ok(art) = state.registry.meta.get("cargo", name, "").await {
        if !art.proprietary.is_empty() {
            m = serde_json::from_slice(&art.proprietary).unwrap_or_default();
        }
    }
    m
}

async fn save_meta(state: &CargoState, name: &str, m: &CargoMeta) {
    let _ = state
        .registry
        .meta
        .put(Artifact {
            format: "cargo".into(),
            repository: name.to_string(),
            version: String::new(),
            proprietary: serde_json::to_vec(m).unwrap_or_default(),
            ..Default::default()
        })
        .await;
}

fn shared_cache() -> pkglab_common::cache::MemCache {
    static CACHE: std::sync::OnceLock<pkglab_common::cache::MemCache> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| pkglab_common::cache::MemCache::new(std::time::Duration::from_secs(3600)))
        .clone()
}

pub fn router(state: Arc<CargoState>) -> axum::Router {
    let s0 = state.clone();
    axum::Router::new()
        .route("/config.json", axum::routing::get(config))
        .route("/index/config.json", axum::routing::get(config))
        .route("/api/v1/crates", axum::routing::get(search))
        .route("/api/v1/crates/new", put(publish))
        .route(
            "/api/v1/crates/{name}/owners",
            axum::routing::get(list_owners).put(add_owners).delete(remove_owners),
        )
        .route("/api/v1/crates/{name}/{version}/yank", axum::routing::get(yank_get).delete(yank))
        .route("/api/v1/crates/{name}/{version}/unyank", put(unyank))
        .route("/api/v1/crates/{name}/{version}/download", axum::routing::get(download))
        .route("/me", axum::routing::get(me))
        .route("/api/v1/crates/{name}", axum::routing::get(api_fallback))
        .fallback(move |req: axum::http::Request<Body>| {
            let st = s0.clone();
            async move { Ok::<_, std::convert::Infallible>(sparse_index(st, req).await) }
        })
        .with_state(state)
}

async fn config(State(st): State<Arc<CargoState>>) -> Response {
    let base = &st.self_base;
    json(
        StatusCode::OK,
        serde_json::json!({
            "dl": format!("{base}/api/v1/crates"),
            "api": format!("{base}"),
        }),
    )
}

async fn search(
    State(st): State<Arc<CargoState>>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
) -> Response {
    let query = q
        .as_deref()
        .and_then(|q| {
            q.split('&').find_map(|pair| {
                let mut it = pair.splitn(2, '=');
                if it.next() == Some("q") {
                    it.next().map(|v| v.to_lowercase())
                } else {
                    None
                }
            })
        })
        .unwrap_or_default();
    let repos = st.registry.meta.list_repositories_by_format("cargo").await.unwrap_or_default();
    let mut crates = Vec::new();
    for name in repos {
        if !query.is_empty() && !name.to_lowercase().contains(&query) {
            continue;
        }
        let versions = st.registry.meta.list_versions("cargo", &name).await.unwrap_or_default();
        let latest = pkglab_common::versioncmp::highest(versions.iter())
            .map(str::to_string)
            .or_else(|| versions.last().cloned())
            .unwrap_or_default();
        crates.push(serde_json::json!({"name": name, "max_version": latest}));
    }
    let total = crates.len();
    json(StatusCode::OK, serde_json::json!({"crates": crates, "meta": {"total": total}}))
}

async fn api_fallback(State(st): State<Arc<CargoState>>, Path(rest): Path<String>) -> Response {
    // /api/v1/crates/{name} metadata
    let name = rest.trim_end_matches('/');
    let versions = st.registry.meta.list_versions("cargo", name).await.unwrap_or_default();
    if versions.is_empty() {
        // Pull-through from crates.io API.
        if let Some(remote) = st.registry.remote("cargo", None) {
            if let Ok(body) = remote.get_bytes(&format!("/api/v1/crates/{}", urlencode(name))).await
            {
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json".to_string())],
                    String::from_utf8_lossy(&body).to_string(),
                )
                    .into_response();
            }
        }
        return error(StatusCode::NOT_FOUND, "crate not found");
    }
    let m = load_meta(&st, name).await;
    let vs: Vec<serde_json::Value> = versions
        .iter()
        .map(|v| {
            serde_json::json!({
                "crate_size": 0,
                "num": v,
                "dl_path": format!("/api/v1/crates/{}/download", urlencode(name)),
                "yanked": m.yanked.get(v).copied().unwrap_or(false),
            })
        })
        .collect();
    json(StatusCode::OK, serde_json::json!({"versions": vs}))
}

/// Sparse index: upstream page (rewritten dl) overlaid with local-only
/// versions.
async fn sparse_index(state: Arc<CargoState>, req: axum::http::Request<Body>) -> Response {
    if req.method() != axum::http::Method::GET {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = req.uri().path().trim_start_matches('/').to_string();
    let rel = path
        .trim_start_matches("pkgs/cargo/")
        .trim_start_matches("cargo/")
        .trim_start_matches("index/")
        .trim_matches('/');
    if rel.is_empty() {
        return text(StatusCode::OK, String::new());
    }
    let name = rel.rsplit('/').next().unwrap_or(rel).to_string();
    let mut versions = state.registry.meta.list_versions("cargo", &name).await.unwrap_or_default();
    pkglab_common::versioncmp::sort_vec(&mut versions);

    // Full upstream index first (cached per crate path).
    let upstream_body = fetch_sparse_index(&state, rel).await;

    if versions.is_empty() {
        if !upstream_body.is_empty() {
            return text(StatusCode::OK, upstream_body);
        }
        return StatusCode::NOT_FOUND.into_response();
    }

    // Local versions always overrides upstream entries for the same version
    // (a re-publish must be authoritative). Collect upstream lines first,
    // dropping any whose `vers` is stored locally.
    let mut upstream_lines: Vec<String> = Vec::new();
    for line in upstream_body.lines() {
        if let Ok(m) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(v) = m.get("vers").and_then(|v| v.as_str()) {
                if versions.iter().any(|lv| lv == v) {
                    continue;
                }
            }
        }
        upstream_lines.push(line.to_string());
    }

    let mut local_lines: Vec<String> = Vec::new();
    for v in &versions {
        let cksum = state
            .registry
            .meta
            .get("cargo", &name, v)
            .await
            .ok()
            .and_then(|art| art.blobs.first().map(|b| b.hex().to_string()))
            .unwrap_or_default();
        let m = load_meta(&state, &name).await;
        let entry = serde_json::json!({
            "name": name, "vers": v, "deps": [], "cksum": cksum,
            "features": {}, "yanked": m.yanked.get(v).copied().unwrap_or(false), "links": null,
        });
        local_lines.push(entry.to_string());
    }

    let mut out = upstream_lines.join("\n");
    if !local_lines.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&local_lines.join("\n"));
    }
    text(StatusCode::OK, out + "\n")
}

async fn fetch_sparse_index(state: &CargoState, rel: &str) -> String {
    let Some(remote) = state.registry.remote_sub("cargo", "index") else {
        return String::new();
    };
    match remote.get_cached(&shared_cache(), &format!("/{}", rel.trim_matches('/'))).await {
        Ok(body) => rewrite_sparse_index(state, &body),
        Err(_) => String::new(),
    }
}

/// Rewrite the `dl` field of upstream sparse index entries to self.
fn rewrite_sparse_index(state: &CargoState, body: &str) -> String {
    let base = &state.self_base;
    let mut sb = String::new();
    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        if let Ok(mut m) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(name) = m.get("name").and_then(|n| n.as_str()) {
                if !name.is_empty() {
                    m["dl"] = serde_json::Value::String(format!("{base}/api/v1/crates"));
                    sb.push_str(&m.to_string());
                    sb.push('\n');
                    continue;
                }
            }
        }
        sb.push_str(line);
        sb.push('\n');
    }
    sb
}

/// Cargo publish framing: [u32-le json-len][metadata JSON][u32-le
/// crate-len][.crate bytes].
fn parse_publish_body(data: &[u8]) -> (String, String, Option<Vec<u8>>) {
    if data.len() < 8 {
        return (String::new(), String::new(), None);
    }
    let json_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    if json_len == 0 || 4 + json_len > data.len() {
        return (String::new(), String::new(), None);
    }
    let Ok(meta) = serde_json::from_slice::<serde_json::Value>(&data[4..4 + json_len]) else {
        return (String::new(), String::new(), None);
    };
    let name = meta.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let vers = meta.get("vers").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let crate_pos = 4 + json_len;
    if crate_pos + 4 > data.len() {
        return (name, vers, None);
    }
    let crate_len = u32::from_le_bytes([
        data[crate_pos],
        data[crate_pos + 1],
        data[crate_pos + 2],
        data[crate_pos + 3],
    ]) as usize;
    let end = (crate_pos + 4 + crate_len).min(data.len());
    (name, vers, Some(data[crate_pos + 4..end].to_vec()))
}

async fn publish(
    State(st): State<Arc<CargoState>>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    let mut body = body;
    let data = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(c) => c.to_bytes().to_vec(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
    };
    let (mut name, mut version, crate_data) = parse_publish_body(&data);
    // Fallback: query params / raw body.
    if name.is_empty() {
        if let Some(q) = q {
            for pair in q.split('&') {
                let mut it = pair.splitn(2, '=');
                if it.next() == Some("name") {
                    name = it.next().unwrap_or("").to_string();
                }
            }
        }
    }
    if version.is_empty() {
        version = "0.1.0".to_string();
    }
    if name.is_empty() {
        return error(StatusCode::BAD_REQUEST, "missing name");
    }
    let crate_data = crate_data.unwrap_or_else(|| data.to_vec());
    store_version(&st, &name, &version, crate_data).await;
    json(StatusCode::CREATED, serde_json::json!({"warnings": {}}))
}

async fn download(
    State(st): State<Arc<CargoState>>,
    Path((name, version)): Path<(String, String)>,
) -> Response {
    let filename = format!("{name}-{version}.crate");
    if let Ok(art) = st.registry.meta.get("cargo", &name, &version).await {
        for b in &art.blobs {
            if let Ok(Some(mut r)) = st.registry.blobs.open(&b.digest).await {
                let mut data = Vec::new();
                if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                    return blob_response(data, &filename);
                }
            }
        }
    }
    // Pull-through. Prefer the configured `cargo.static` upstream using the
    // crates.io-style `/api/v1/crates/{name}/{version}/download` shape (an
    // upstream registry such as the old artifact exposes that); fall back to
    // the static.crates.io flat `/crates/{name}-{version}.crate` form when a
    // bare path is given.
    if let Some(remote) = st.registry.remote_sub("cargo", "static") {
        let name_enc = urlencode(&name);
        let crate_path = format!("/{}/{name}-{version}.crate", name_enc);
        let api_path = format!("/api/v1/crates/{}/{version}/download", name_enc);
        tracing::debug!(crate_name = %name, crate_path = %crate_path, api_path = %api_path, base = %remote.base(), "cargo pull-through candidate paths");
        let data = match remote.get_bytes(&crate_path).await {
            Ok(d) => Some(d),
            Err(e) => {
                tracing::debug!(err = %e, path = %crate_path, "cargo crate_path miss; trying api_path");
                remote.get_bytes(&api_path).await.ok()
            }
        };
        if let Some(data) = data {
            store_version_src(&st, &name, &version, data.clone(), "pull").await;
            return blob_response(data, &filename);
        }
    }
    error(StatusCode::NOT_FOUND, "not found")
}

async fn store_version(st: &CargoState, name: &str, version: &str, data: Vec<u8>) {
    store_version_src(st, name, version, data, "push").await
}

pub async fn store_version_src(
    st: &CargoState,
    name: &str,
    version: &str,
    data: Vec<u8>,
    source: &str,
) {
    let version = if version.is_empty() { "0.1.0" } else { version };
    let mut art = Artifact {
        format: "cargo".into(),
        repository: name.to_string(),
        version: version.to_string(),
        source: source.to_string(),
        ..Default::default()
    };
    if !data.is_empty() {
        if let Ok((hashes, size)) = pkglab_common::artifact::compute_hashes(&data[..]) {
            let digest = format!("sha256:{}", hashes.sha256);
            let mut cursor = Cursor::new(&data);
            if st.registry.blobs.put_if_absent(&digest, &mut cursor).await.is_ok() {
                art.blobs.push(Descriptor {
                    digest,
                    size: size as i64,
                    name: format!("{name}-{version}.crate"),
                    ..Default::default()
                });
            }
        }
    }
    let _ = st.registry.meta.put(art).await;
}

async fn yank_get() -> Response {
    json(StatusCode::OK, serde_json::json!({"ok": true}))
}

async fn yank(
    State(st): State<Arc<CargoState>>,
    Path((name, version)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    set_yanked(&st, &name, &version, true).await;
    json(StatusCode::OK, serde_json::json!({"ok": true}))
}

async fn unyank(
    State(st): State<Arc<CargoState>>,
    Path((name, version)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    set_yanked(&st, &name, &version, false).await;
    json(StatusCode::OK, serde_json::json!({"ok": true}))
}

async fn set_yanked(st: &CargoState, name: &str, version: &str, yanked: bool) {
    if st.registry.meta.get("cargo", name, version).await.is_err() {
        return;
    }
    let mut m = load_meta(st, name).await;
    m.yanked.insert(version.to_string(), yanked);
    save_meta(st, name, &m).await;
}

async fn list_owners(State(st): State<Arc<CargoState>>, Path(name): Path<String>) -> Response {
    let m = load_meta(&st, &name).await;
    let users: Vec<serde_json::Value> =
        m.owners.iter().map(|o| serde_json::json!({"login": o, "name": o})).collect();
    json(StatusCode::OK, serde_json::json!({"users": users}))
}

async fn add_owners(
    State(st): State<Arc<CargoState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    let mut body = body;
    let raw = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(c) => c.to_bytes().to_vec(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
    };
    let mut m = load_meta(&st, &name).await;
    if let Ok(req) = serde_json::from_slice::<serde_json::Value>(&raw) {
        if let Some(users) = req.get("users").and_then(|u| u.as_array()) {
            for u in users {
                if let Some(s) = u.as_str() {
                    if !m.owners.contains(&s.to_string()) {
                        m.owners.push(s.to_string());
                    }
                }
            }
        }
    }
    save_meta(&st, &name, &m).await;
    json(StatusCode::OK, serde_json::json!({"ok": true, "msg": "owners added"}))
}

async fn remove_owners(
    State(st): State<Arc<CargoState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    let mut body = body;
    let raw = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(c) => c.to_bytes().to_vec(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
    };
    let mut remove = std::collections::HashSet::new();
    if let Ok(req) = serde_json::from_slice::<serde_json::Value>(&raw) {
        if let Some(users) = req.get("users").and_then(|u| u.as_array()) {
            for u in users {
                if let Some(s) = u.as_str() {
                    remove.insert(s.to_string());
                }
            }
        }
    }
    let mut m = load_meta(&st, &name).await;
    m.owners.retain(|o| !remove.contains(o));
    save_meta(&st, &name, &m).await;
    json(StatusCode::OK, serde_json::json!({"ok": true, "msg": "owners removed"}))
}

async fn me() -> Response {
    json(
        StatusCode::OK,
        serde_json::json!({"user": {"id": 1, "login": "anonymous", "name": "anonymous"}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_framing() {
        let meta = br#"{"name":"anyhow","vers":"1.0.0"}"#;
        let crate_bytes = b"CRATE-BYTES";
        let mut body = Vec::new();
        body.extend_from_slice(&(meta.len() as u32).to_le_bytes());
        body.extend_from_slice(meta);
        body.extend_from_slice(&(crate_bytes.len() as u32).to_le_bytes());
        body.extend_from_slice(crate_bytes);
        let (name, vers, data) = parse_publish_body(&body);
        assert_eq!(name, "anyhow");
        assert_eq!(vers, "1.0.0");
        assert_eq!(data.unwrap(), crate_bytes.to_vec());
    }

    #[test]
    fn index_rewrite() {
        let st_state = test_state();
        let body = r#"{"name":"anyhow","vers":"1.0.0","deps":[],"cksum":"aa","features":{},"yanked":false}"#;
        let out = rewrite_sparse_index(&st_state, body);
        assert!(out.contains("anyhow"), "out={out}");
        assert!(out.contains("http://reg/pkgs/cargo/api/v1/crates"), "dl rewritten: {out}");
    }

    fn test_state() -> CargoState {
        CargoState {
            registry: Arc::new(pkglab_common::Registry::new(
                Arc::new(NullBlobs),
                Arc::new(NullStore),
                pkglab_common::upstreams::Upstreams::new(None),
            )),
            auth: None,
            self_base: "http://reg/pkgs/cargo".into(),
        }
    }

    struct NullBlobs;
    struct NullStore;

    #[async_trait::async_trait]
    impl pkglab_common::BlobStore for NullBlobs {
        async fn stat(&self, _: &str) -> pkglab_common::blob::Result<Option<u64>> {
            Ok(None)
        }
        async fn open(
            &self,
            _: &str,
        ) -> pkglab_common::blob::Result<Option<Box<dyn pkglab_common::blob::BlobReader>>> {
            Ok(None)
        }
        async fn put_if_absent(
            &self,
            _: &str,
            _: &mut (dyn std::io::Read + Send + Unpin),
        ) -> pkglab_common::blob::Result<bool> {
            Ok(true)
        }
        async fn hashes_for(&self, _: &str) -> pkglab_common::blob::Result<pkglab_common::Hashes> {
            Ok(Default::default())
        }
        async fn delete(&self, _: &str) -> pkglab_common::blob::Result<()> {
            Ok(())
        }
        async fn list(&self) -> pkglab_common::blob::Result<Vec<String>> {
            Ok(vec![])
        }
    }

    #[async_trait::async_trait]
    impl pkglab_common::ArtifactStore for NullStore {
        async fn put(&self, _: pkglab_common::Artifact) -> pkglab_common::registry::Result<()> {
            Ok(())
        }
        async fn get(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> pkglab_common::registry::Result<pkglab_common::Artifact> {
            Err(pkglab_common::RegistryError::ArtifactUnknown)
        }
        async fn delete(&self, _: &str, _: &str, _: &str) -> pkglab_common::registry::Result<()> {
            Ok(())
        }
        async fn delete_repo(&self, _: &str) -> pkglab_common::registry::Result<u64> {
            Ok(0)
        }
        async fn list_versions(
            &self,
            _: &str,
            _: &str,
        ) -> pkglab_common::registry::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn list_repositories_by_format(
            &self,
            _: &str,
        ) -> pkglab_common::registry::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn list_repositories(&self) -> pkglab_common::registry::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn list_packages(
            &self,
        ) -> pkglab_common::registry::Result<Vec<pkglab_common::store::PackageSummary>> {
            Ok(vec![])
        }
        async fn list_oci_images(
            &self,
            _: &str,
            _: &str,
        ) -> pkglab_common::registry::Result<Vec<pkglab_common::store::PackageSummary>> {
            Ok(vec![])
        }
        async fn list_artifacts(
            &self,
            _: &str,
            _: &str,
        ) -> pkglab_common::registry::Result<Vec<pkglab_common::Artifact>> {
            Ok(vec![])
        }
        async fn save_upload(
            &self,
            _: pkglab_common::blob::UploadRecord,
        ) -> pkglab_common::registry::Result<()> {
            Ok(())
        }
        async fn get_upload(
            &self,
            _: &str,
        ) -> pkglab_common::registry::Result<pkglab_common::blob::UploadRecord> {
            Err(pkglab_common::RegistryError::UploadUnknown)
        }
        async fn delete_upload(&self, _: &str) -> pkglab_common::registry::Result<()> {
            Ok(())
        }
        async fn list_uploads(&self) -> pkglab_common::registry::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn get_meta(&self, _: &str) -> pkglab_common::registry::Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn set_meta(&self, _: &str, _: &[u8]) -> pkglab_common::registry::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests_extras {
    use super::*;

    #[test]
    fn parse_publish_body_edge_cases() {
        // Too short.
        let (n, v, d) = parse_publish_body(&[1, 2, 3]);
        assert_eq!((n.as_str(), v.as_str(), d.is_none()), ("", "", true));
        // Zero metadata length.
        let (n, v, d) = parse_publish_body(&[0u8, 0, 0, 0, 0xff]);
        assert_eq!((n.as_str(), v.as_str(), d.is_none()), ("", "", true));
    }
}
