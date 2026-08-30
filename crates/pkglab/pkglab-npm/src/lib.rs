//! npm registry protocol: packument assembly (dist.shasum sha1 + dist
//! integrity sha512, tarball URL rewrite), tarball serving, publish
//! (CouchDB `_attachments` base64 or raw tarball), dist-tags (persisted under
//! the version-"" root artifact), deprecate, -rev unpublish, search/all,
//! audit/profile stubs, plus pull-through with packument URL rewriting.

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use base64::Engine as _;
use flate2::read::GzDecoder;
use pkglab_common::httphelpers::{error, json, octet_response as blob_response};
use pkglab_common::{Artifact, Descriptor};
use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Arc;
use tar::Archive;

pub struct NpmState {
    pub registry: Arc<pkglab_common::Registry>,
    pub auth: Option<Arc<dyn pkglab_common::Auth>>,
    pub self_base: String,
}

/// npm tarball filename: the last path segment of the package name + version.
pub fn tarball_filename(name: &str, version: &str) -> String {
    let short = name.rsplit('/').next().unwrap_or(name);
    format!("{short}-{version}.tgz")
}

/// Scoped names arrive as `@scope%2Fname` or `@scope/name`.
pub fn unescape_name(name: &str) -> String {
    percent_encoding::percent_decode_str(name)
        .decode_utf8()
        .map(|c| c.to_string())
        .unwrap_or_else(|_| name.to_string())
}

pub fn encode_name(name: &str) -> String {
    name.replace('/', "%2F")
}

async fn authorize_write(state: &NpmState, headers: &HeaderMap) -> Result<(), Response> {
    pkglab_common::httphelpers::authorize_write(&state.auth, headers).await
}

pub fn router(state: Arc<NpmState>) -> axum::Router {
    let s0 = state.clone();
    axum::Router::new()
        .fallback(any(move |req: axum::http::Request<Body>| {
            let st = s0.clone();
            async move {
                let method = req.method().clone();
                let headers = req.headers().clone();
                let path = req.uri().path().to_string();
                let raw_query = req.uri().query().map(|q| q.to_string());
                let body = req.into_body();
                Ok::<_, std::convert::Infallible>(
                    dispatch(st, method, &path, raw_query, headers, body).await,
                )
            }
        }))
        .with_state(state)
}

async fn dispatch(
    state: Arc<NpmState>,
    method: Method,
    path: &str,
    raw_query: Option<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let rel = path.trim_start_matches('/');
    let rel = rel.strip_prefix("pkgs/npm/").or_else(|| rel.strip_prefix("npm/")).unwrap_or(rel);

    match rel {
        "" => {
            return json(
                StatusCode::OK,
                serde_json::json!({"db_name": "registry", "doc_count": "0"}),
            )
        }
        "-/ping" => return json(StatusCode::OK, serde_json::json!({})),
        "-/whoami" => return json(StatusCode::OK, serde_json::json!({"username": "anonymous"})),
        "-/all" => return all_packages(&state).await,
        "-/v1/login" => {
            return json(StatusCode::OK, serde_json::json!({"ok": true, "token": "npm-anonymous"}))
        }
        _ => {}
    }

    if rel.starts_with("-/npm/v1/security") {
        return security_stub(rel);
    }
    if rel == "-/npm/v1/user" {
        return json(
            StatusCode::OK,
            serde_json::json!({"name": "anonymous", "email": "", "email_verified": false, "tfa": null}),
        );
    }
    if rel.starts_with("-/npm/v1/tokens") {
        return tokens(rel, method);
    }
    if let Some(rest) = rel.strip_prefix("-/user/") {
        return couch_user(rest, method);
    }
    if rel.starts_with("-/v1/search") {
        return search(&state, &raw_query.unwrap_or_default()).await;
    }
    if rel.starts_with("-/package/") {
        return dist_tags(&state, rel, method, headers, body).await;
    }
    if rel.contains("/-rev/") {
        return rev(&state, rel, method, body).await;
    }

    // PUT deprecate (path form).
    if method == Method::PUT && rel.ends_with("/deprecate") {
        return deprecate(&state, rel.trim_end_matches("/deprecate"), body).await;
    }

    if method == Method::PUT {
        if let Err(resp) = authorize_write(&state, &headers).await {
            return resp;
        }
        return publish(&state, &unescape_name(rel), body).await;
    }
    if method == Method::DELETE {
        if let Err(resp) = authorize_write(&state, &headers).await {
            return resp;
        }
        return delete_version(&state, &unescape_name(rel)).await;
    }

    // Tarball: {pkg}/-/{file} (check after dist-tags to allow /-/package/...)
    if let Some(idx) = rel.find("/-/") {
        let pkg = &rel[..idx];
        let file = &rel[idx + "/-/".len()..];
        if file == "package" || file.starts_with("package/") {
            return dist_tags(&state, rel, method, headers, body).await;
        }
        if method == Method::GET {
            return tarball(&state, &unescape_name(pkg), file).await;
        }
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    // Packument.
    if method == Method::GET {
        return metadata(&state, &unescape_name(rel)).await;
    }
    StatusCode::NOT_FOUND.into_response()
}

async fn all_packages(state: &NpmState) -> Response {
    let repos = state.registry.meta.list_repositories_by_format("npm").await.unwrap_or_default();
    let mut entries = serde_json::Map::new();
    for repo in &repos {
        let versions = state.registry.meta.list_versions("npm", repo).await.unwrap_or_default();
        let latest = pkglab_common::versioncmp::highest(versions.iter())
            .map(str::to_string)
            .or_else(|| versions.last().cloned())
            .unwrap_or_default();
        entries.insert(repo.clone(), serde_json::json!({"name": repo, "latest": latest}));
    }
    json(
        StatusCode::OK,
        serde_json::json!({"db_name": "registry", "_updated": 0, "entries": entries}),
    )
}

fn security_stub(rel: &str) -> Response {
    if rel.contains("/quick") {
        return json(StatusCode::OK, serde_json::json!({"secret": "audits-quick"}));
    }
    // FLAT map: npm@11's arborist iterates Object.entries(body) directly.
    json(StatusCode::OK, serde_json::json!({}))
}

fn tokens(rel: &str, method: Method) -> Response {
    let _ = rel;
    match method {
        Method::DELETE => json(StatusCode::OK, serde_json::json!({"ok": true})),
        Method::POST => json(
            StatusCode::CREATED,
            serde_json::json!({"token": "npm-anonymous", "key": "npm-anonymous"}),
        ),
        _ => json(StatusCode::OK, serde_json::json!({"objects": [], "total": 0})),
    }
}

fn couch_user(rel: &str, method: Method) -> Response {
    match method {
        Method::PUT | Method::POST => {
            let name = rel.trim_start_matches("org.couchdb.user:");
            json(
                StatusCode::CREATED,
                serde_json::json!({
                    "ok": true, "name": name, "token": "npm-anonymous",
                    "id": format!("org.couchdb.user:{name}"), "rev": "1"
                }),
            )
        }
        Method::DELETE => json(StatusCode::OK, serde_json::json!({"ok": true})),
        _ => json(StatusCode::OK, serde_json::json!({"_id": rel, "name": rel})),
    }
}

async fn search(state: &NpmState, raw_query: &str) -> Response {
    let mut q = String::new();
    for pair in raw_query.split('&') {
        let mut it = pair.splitn(2, '=');
        if it.next() == Some("text") {
            q = it.next().unwrap_or("").replace("%20", " ").replace('+', " ").to_lowercase();
        }
    }
    let repos = state.registry.meta.list_repositories_by_format("npm").await.unwrap_or_default();
    let mut objects = Vec::new();
    for repo in repos {
        if !q.is_empty() && !repo.to_lowercase().contains(&q) {
            continue;
        }
        let versions = state.registry.meta.list_versions("npm", &repo).await.unwrap_or_default();
        let latest = pkglab_common::versioncmp::highest(versions.iter())
            .map(str::to_string)
            .or_else(|| versions.last().cloned())
            .unwrap_or_default();
        objects.push(serde_json::json!({"package": {"name": repo, "version": latest}}));
    }
    let total = objects.len();
    json(StatusCode::OK, serde_json::json!({"objects": objects, "total": total, "time": ""}))
}

async fn dist_tags(
    state: &NpmState,
    rel: &str,
    method: Method,
    headers: HeaderMap,
    body: Body,
) -> Response {
    // forms: "-/package/{pkg}/dist-tags" or "-/package/{pkg}/dist-tags/{tag}"
    let rest = rel.trim_end_matches('/');
    let Some(rest) = rest.strip_prefix("-/package/") else {
        return error(StatusCode::NOT_FOUND, "not found");
    };
    let Some(idx) = rest.find("/dist-tags") else {
        return error(StatusCode::NOT_FOUND, "not found");
    };
    let pkg = unescape_name(&rest[..idx]);
    let tag = rest[idx + "/dist-tags".len()..].trim_matches('/').to_string();

    match method {
        Method::GET => {
            if !tag.is_empty() {
                let dt = dist_tags_map(state, &pkg).await;
                return match dt.get(&tag) {
                    Some(v) => json(StatusCode::OK, v.clone()),
                    None => error(StatusCode::NOT_FOUND, "not found"),
                };
            }
            let dt = dist_tags_map(state, &pkg).await;
            json(StatusCode::OK, serde_json::Value::Object(dt.into_iter().collect()))
        }
        Method::PUT => {
            if let Err(resp) = authorize_write(state, &headers).await {
                return resp;
            }
            let mut body = body;
            let raw = match http_body_util::BodyExt::collect(&mut body).await {
                Ok(c) => c.to_bytes().to_vec(),
                Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
            };
            let version = String::from_utf8_lossy(&raw).trim().trim_matches('"').to_string();
            if version.is_empty() {
                return error(StatusCode::NOT_FOUND, "missing version");
            }
            let mut dt = load_dist_tags(state, &pkg).await;
            if state.registry.meta.get("npm", &pkg, &version).await.is_err() {
                return error(StatusCode::NOT_FOUND, &format!("version {version} not found"));
            }
            dt.insert(tag, serde_json::Value::String(version));
            save_dist_tags(state, &pkg, dt).await;
            json(StatusCode::OK, serde_json::json!({"ok": true}))
        }
        Method::DELETE => {
            if let Err(resp) = authorize_write(state, &headers).await {
                return resp;
            }
            let mut dt = load_dist_tags(state, &pkg).await;
            dt.remove(&tag);
            save_dist_tags(state, &pkg, dt).await;
            json(StatusCode::OK, serde_json::json!({"ok": true}))
        }
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

/// dist-tags live under the package's root artifact (version == ""), stored
/// in proprietary as {"dist-tags": {...}}; fall back to `latest`.
async fn dist_tags_map(state: &NpmState, pkg: &str) -> BTreeMap<String, serde_json::Value> {
    let mut def = BTreeMap::new();
    let versions = state.registry.meta.list_versions("npm", pkg).await.unwrap_or_default();
    let latest = pkglab_common::versioncmp::highest(versions.iter())
        .map(str::to_string)
        .or_else(|| versions.last().cloned());
    if let Some(v) = latest {
        def.insert("latest".to_string(), serde_json::Value::String(v));
    }
    if let Ok(art) = state.registry.meta.get("npm", pkg, "").await {
        if let Ok(m) = serde_json::from_slice::<serde_json::Value>(&art.proprietary) {
            if let Some(dt) = m.get("dist-tags").and_then(|d| d.as_object()) {
                if !dt.is_empty() {
                    return dt.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                }
            }
        }
    }
    def
}

async fn load_dist_tags(state: &NpmState, pkg: &str) -> BTreeMap<String, serde_json::Value> {
    dist_tags_map(state, pkg).await
}

async fn save_dist_tags(state: &NpmState, pkg: &str, dt: BTreeMap<String, serde_json::Value>) {
    let stored =
        serde_json::json!({"dist-tags": serde_json::Value::Object(dt.into_iter().collect())});
    let _ = state
        .registry
        .meta
        .put(Artifact {
            format: "npm".into(),
            repository: pkg.to_string(),
            version: String::new(),
            proprietary: stored.to_string().into_bytes(),
            ..Default::default()
        })
        .await;
}

async fn deprecate(state: &NpmState, name: &str, body: Body) -> Response {
    let mut body = body;
    let raw = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(c) => c.to_bytes().to_vec(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
    };
    if let Ok(doc) = serde_json::from_slice::<serde_json::Value>(&raw) {
        if let Some(versions) = doc.get("versions").and_then(|v| v.as_object()) {
            for (ver, msg) in versions {
                let Ok(mut art) = state.registry.meta.get("npm", name, ver).await else {
                    continue;
                };
                let mut pj: serde_json::Map<String, serde_json::Value> =
                    serde_json::from_slice(&art.proprietary).unwrap_or_default();
                match msg.as_str() {
                    Some("") | None => {
                        pj.remove("deprecated");
                    }
                    Some(m) => {
                        pj.insert("deprecated".into(), serde_json::Value::String(m.to_string()));
                    }
                }
                art.proprietary = serde_json::Value::Object(pj).to_string().into_bytes();
                let _ = state.registry.meta.put(art).await;
            }
        }
    }
    json(StatusCode::OK, serde_json::json!({"ok": true}))
}

/// CouchDB rev endpoints:
/// - DELETE {pkg}/-rev/{rev}   -> unpublish whole package
/// - PUT    {pkg}/-rev/{rev}   -> replace packument (unpublish versions)
async fn rev(state: &NpmState, rel: &str, method: Method, body: Body) -> Response {
    match method {
        Method::DELETE => {
            if let Some(i) = rel.find("/-rev/") {
                let pkg = unescape_name(&rel[..i]);
                if let Ok(versions) = state.registry.meta.list_versions("npm", &pkg).await {
                    for v in versions {
                        remove_version(state, &pkg, &v).await;
                    }
                }
                let _ = state.registry.meta.delete("npm", &pkg, "").await;
                return json(StatusCode::OK, serde_json::json!({"ok": true}));
            }
            error(StatusCode::NOT_FOUND, "not found")
        }
        Method::PUT => {
            // Version removal: body is the packument without removed versions.
            let mut body = body;
            let raw = match http_body_util::BodyExt::collect(&mut body).await {
                Ok(c) => c.to_bytes().to_vec(),
                Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
            };
            if let Ok(doc) = serde_json::from_slice::<serde_json::Value>(&raw) {
                if let Some(pkg) = doc.get("name").and_then(|n| n.as_str()) {
                    reconcile_versions(state, pkg, &doc).await;
                }
            }
            json(StatusCode::OK, serde_json::json!({"ok": true}))
        }
        _ => error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
    }
}

async fn reconcile_versions(state: &NpmState, pkg: &str, doc: &serde_json::Value) {
    let mut keep = std::collections::HashSet::new();
    if let Some(vs) = doc.get("versions").and_then(|v| v.as_object()) {
        for k in vs.keys() {
            keep.insert(k.clone());
        }
    }
    let Ok(current) = state.registry.meta.list_versions("npm", pkg).await else {
        return;
    };
    for v in current {
        if !keep.contains(&v) {
            remove_version(state, pkg, &v).await;
        }
    }
}

async fn delete_version(state: &NpmState, name: &str) -> Response {
    // DELETE {pkg}/{version}
    if let Some(i) = name.rfind('/') {
        if i > 0 {
            let (pkg, ver) = (name[..i].to_string(), name[i + 1..].to_string());
            remove_version(state, &pkg, &ver).await;
            return json(StatusCode::OK, serde_json::json!({"ok": true}));
        }
    }
    // DELETE {pkg}: remove the whole package.
    if let Ok(versions) = state.registry.meta.list_versions("npm", name).await {
        for v in versions {
            remove_version(state, name, &v).await;
        }
    }
    let _ = state.registry.meta.delete("npm", name, "").await;
    json(StatusCode::OK, serde_json::json!({"ok": true}))
}

async fn remove_version(state: &NpmState, pkg: &str, version: &str) {
    if let Ok(art) = state.registry.meta.get("npm", pkg, version).await {
        for b in &art.blobs {
            let _ = state.registry.blobs.delete(&b.digest).await;
        }
    }
    let _ = state.registry.meta.delete("npm", pkg, version).await;
}

async fn metadata(state: &NpmState, name: &str) -> Response {
    match aggregate_metadata(state, name).await {
        Some(body) => {
            (StatusCode::OK, [(header::CONTENT_TYPE, "application/json".to_string())], body)
                .into_response()
        }
        None => {
            // Pull-through.
            let Some(remote) = state.registry.remote("npm", None) else {
                return error(StatusCode::NOT_FOUND, "package not found");
            };
            match remote.get_cached(&shared_cache(), &format!("/{}", encode_name(name))).await {
                Ok(body) => {
                    let rewritten = rewrite_tarball_urls(&body, &state.self_base, name);
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json".to_string())],
                        rewritten,
                    )
                        .into_response()
                }
                Err(_) => error(StatusCode::NOT_FOUND, "package not found"),
            }
        }
    }
}

fn shared_cache() -> pkglab_common::cache::MemCache {
    static CACHE: std::sync::OnceLock<pkglab_common::cache::MemCache> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| pkglab_common::cache::MemCache::new(std::time::Duration::from_secs(3600)))
        .clone()
}

/// Assemble a packument from locally stored versions: dist.tarball rewritten
/// to self, shasum (sha1 hex), integrity (sha512 base64), size.
async fn aggregate_metadata(state: &NpmState, name: &str) -> Option<String> {
    let versions = state.registry.meta.list_versions("npm", name).await.ok()?;
    if versions.is_empty() {
        return None;
    }
    let mut vmap = serde_json::Map::new();
    for v in &versions {
        let Ok(art) = state.registry.meta.get("npm", name, v).await else {
            continue;
        };
        let mut pkg_json: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&art.proprietary).unwrap_or_default();
        pkg_json
            .entry("name".to_string())
            .or_insert_with(|| serde_json::Value::String(name.to_string()));
        pkg_json
            .entry("version".to_string())
            .or_insert_with(|| serde_json::Value::String(v.clone()));

        let mut dist = serde_json::Map::new();
        dist.insert(
            "tarball".to_string(),
            serde_json::Value::String(tarball_url(&state.self_base, name, v)),
        );
        for b in &art.blobs {
            if b.digest.is_empty() {
                continue;
            }
            if let Ok(h) = state.registry.blobs.hashes_for(&b.digest).await {
                dist.insert("shasum".into(), serde_json::Value::String(h.sha1.clone()));
                let integrity = base64::engine::general_purpose::STANDARD
                    .encode(hex::decode(&h.sha512).unwrap_or_default());
                dist.insert(
                    "integrity".into(),
                    serde_json::Value::String(format!("sha512-{integrity}")),
                );
            }
            if b.size > 0 {
                dist.insert("size".into(), b.size.into());
            }
        }
        pkg_json.insert("dist".into(), serde_json::Value::Object(dist));
        vmap.insert(v.clone(), serde_json::Value::Object(pkg_json));
    }

    let latest = pkglab_common::versioncmp::highest(versions.iter())
        .map(str::to_string)
        .or_else(|| versions.last().cloned())
        .unwrap_or_default();

    let mut dist_tags = dist_tags_map(state, name).await;
    dist_tags.entry("latest".to_string()).or_insert_with(|| serde_json::Value::String(latest));

    let doc = serde_json::json!({
        "_id": name,
        "_rev": "1",
        "name": name,
        "dist-tags": serde_json::Value::Object(dist_tags.into_iter().collect()),
        "versions": serde_json::Value::Object(vmap),
    });
    Some(doc.to_string())
}

fn tarball_url(self_base: &str, name: &str, version: &str) -> String {
    format!(
        "{}/{}/-/{}",
        self_base.trim_end_matches('/'),
        encode_name(name),
        tarball_filename(name, version)
    )
}

/// Rewrite upstream dist.tarball URLs to self URLs in a proxied packument.
fn rewrite_tarball_urls(packument: &str, self_base: &str, name: &str) -> String {
    let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(packument) else {
        return packument.to_string();
    };
    if let Some(versions) = doc.get_mut("versions").and_then(|v| v.as_object_mut()) {
        for (ver, info) in versions.iter_mut() {
            if let Some(dist) = info.get_mut("dist").and_then(|d| d.as_object_mut()) {
                dist.insert(
                    "tarball".into(),
                    serde_json::Value::String(tarball_url(self_base, name, ver)),
                );
            }
        }
    }
    doc.to_string()
}

async fn tarball(state: &NpmState, name: &str, file: &str) -> Response {
    let versions = state.registry.meta.list_versions("npm", name).await.unwrap_or_default();
    for v in &versions {
        if tarball_filename(name, v) == file {
            if let Ok(art) = state.registry.meta.get("npm", name, v).await {
                for b in &art.blobs {
                    if let Ok(Some(mut r)) = state.registry.blobs.open(&b.digest).await {
                        let mut data = Vec::new();
                        if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                            return blob_response(data);
                        }
                    }
                }
            }
        }
    }
    // Pull-through tarball.
    let Some(remote) = state.registry.remote("npm", None) else {
        return error(StatusCode::NOT_FOUND, "tarball not found");
    };
    match remote.get_bytes(&format!("/{}/-/{}", encode_name(name), file)).await {
        Ok(data) => {
            // Cache the version with the package.json extracted from the
            // tarball (this is what `npm install pkg@ver` hits directly).
            let (ver, pkg_json) = parse_tarball_package_json(&data);
            let ver = if ver.is_empty() { version_from_tarball_name(file, name) } else { ver };
            store_version_src(state, name, &ver, data.clone(), pkg_json, "pull").await;
            blob_response(data)
        }
        Err(_) => error(StatusCode::NOT_FOUND, "tarball not found"),
    }
}

fn version_from_tarball_name(file: &str, name: &str) -> String {
    let short = name.rsplit('/').next().unwrap_or(name);
    let prefix = format!("{short}-");
    if let Some(v) = file.strip_prefix(&prefix) {
        if let Some(v) = v.strip_suffix(".tgz") {
            return v.to_string();
        }
    }
    String::new()
}

/// Read package.json out of a gzipped tarball.
fn parse_tarball_package_json(data: &[u8]) -> (String, serde_json::Value) {
    let gz = GzDecoder::new(data);
    let mut archive = Archive::new(gz);
    for entry in archive.entries().into_iter().flatten() {
        let Ok(mut entry) = entry else { continue };
        let name = entry.path().map(|p| p.display().to_string()).unwrap_or_default();
        if name.ends_with("package.json") {
            let mut buf = Vec::new();
            if entry.read_to_end(&mut buf).is_ok() {
                if let Ok(m) = serde_json::from_slice::<serde_json::Value>(&buf) {
                    let ver = m.get("version").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    return (ver, m);
                }
            }
        }
    }
    (String::new(), serde_json::Value::Null)
}

async fn publish(state: &NpmState, name: &str, body: Body) -> Response {
    let mut body = body;
    let data = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(c) => c.to_bytes().to_vec(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
    };

    // Raw tarball publish (gzipped: 0x1f 0x8b).
    if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
        let (ver, pkg_json) = parse_tarball_package_json(&data);
        let pkg_name = pkg_json
            .get("name")
            .and_then(|n| n.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(name)
            .to_string();
        store_version_src(state, &pkg_name, &ver, data, pkg_json, "push").await;
        return json(StatusCode::CREATED, serde_json::json!({"ok": true}));
    }

    // CouchDB-style publish document.
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&data) else {
        return error(StatusCode::BAD_REQUEST, "invalid payload");
    };
    let pkg_name = payload
        .get("name")
        .and_then(|n| n.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(name)
        .to_string();

    let mut version = String::new();
    let mut pkg_json = serde_json::Value::Null;
    if let Some(vs) = payload.get("versions").and_then(|v| v.as_object()) {
        if let Some(first) = vs.keys().next() {
            version = first.clone();
            pkg_json = vs[first].clone();
        }
    }
    if version.is_empty() {
        version = payload
            .get("dist-tags")
            .and_then(|dt| dt.get("latest"))
            .and_then(|l| l.as_str())
            .unwrap_or("")
            .to_string();
    }
    if version.is_empty() {
        return error(StatusCode::BAD_REQUEST, "cannot determine version");
    }

    let mut tarball_bytes: Vec<u8> = Vec::new();
    if let Some(atts) = payload.get("_attachments").and_then(|a| a.as_object()) {
        for (_, v) in atts {
            if let Some(b64) = v.get("data").and_then(|d| d.as_str()) {
                if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(b64) {
                    tarball_bytes = decoded;
                    break;
                }
            }
        }
    }

    store_version_src(state, &pkg_name, &version, tarball_bytes, pkg_json, "push").await;
    json(StatusCode::CREATED, serde_json::json!({"ok": true}))
}

pub async fn store_version_src(
    state: &NpmState,
    name: &str,
    version: &str,
    tarball: Vec<u8>,
    pkg_json: serde_json::Value,
    source: &str,
) {
    let name = if name.is_empty() { "unknown" } else { name };
    let version = if version.is_empty() { "0.0.0" } else { version };
    let mut art = Artifact {
        format: "npm".into(),
        repository: name.to_string(),
        version: version.to_string(),
        media_type: "application/json".into(),
        proprietary: pkg_json.to_string().into_bytes(),
        source: source.to_string(),
        ..Default::default()
    };
    if !tarball.is_empty() {
        if let Ok((hashes, size)) = pkglab_common::artifact::compute_hashes(&tarball[..]) {
            let digest = format!("sha256:{}", hashes.sha256);
            let mut cursor = std::io::Cursor::new(&tarball);
            if state.registry.blobs.put_if_absent(&digest, &mut cursor).await.is_ok() {
                art.blobs.push(Descriptor {
                    digest,
                    size: size as i64,
                    name: tarball_filename(name, version),
                    ..Default::default()
                });
            }
        }
    }
    let _ = state.registry.meta.put(art).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tarball_names() {
        assert_eq!(tarball_filename("lodash", "4.17.21"), "lodash-4.17.21.tgz");
        assert_eq!(tarball_filename("@scope/pkg", "1.0.0"), "pkg-1.0.0.tgz");
    }

    #[test]
    fn names() {
        assert_eq!(unescape_name("@scope%2Fname"), "@scope/name");
        assert_eq!(encode_name("@scope/name"), "@scope%2Fname");
    }

    #[test]
    fn version_from_name() {
        assert_eq!(version_from_tarball_name("lodash-4.0.0.tgz", "lodash"), "4.0.0");
        assert_eq!(version_from_tarball_name("other.tgz", "lodash"), "");
    }

    #[test]
    fn packument_rewrite() {
        let doc = r#"{"versions":{"1.0.0":{"name":"x","dist":{"tarball":"https://registry.npmjs.org/x/-/x-1.0.0.tgz"}}}}"#;
        let out = rewrite_tarball_urls(doc, "http://reg/pkgs/npm", "x");
        assert!(out.contains("http://reg/pkgs/npm/x/-/x-1.0.0.tgz"));
        assert!(!out.contains("npmjs.org"));
    }

    #[test]
    fn packument_rewrite_scoped() {
        let doc = r#"{"versions":{"2.0.0":{"name":"@s/p","dist":{"tarball":"https://registry.npmjs.org/@s/p/-/p-2.0.0.tgz"}}}}"#;
        let out = rewrite_tarball_urls(doc, "http://reg/pkgs/npm", "@s/p");
        assert!(out.contains("http://reg/pkgs/npm/@s%2Fp/-/p-2.0.0.tgz"));
        assert!(!out.contains("npmjs.org"));
    }

    #[test]
    fn tarball_url_shape() {
        assert_eq!(
            tarball_url("http://reg/pkgs/npm/", "@scope/pkg", "1.0.0"),
            "http://reg/pkgs/npm/@scope%2Fpkg/-/pkg-1.0.0.tgz"
        );
    }
}
