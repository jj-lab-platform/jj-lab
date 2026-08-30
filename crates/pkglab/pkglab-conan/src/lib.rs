//! Conan 2.x (and minimal 1.x) repository protocol: v1+v2 route sets, ping
//! capabilities, authenticate/check_credentials, search with glob matching,
//! latest/revisions/files, upload_urls indirection + direct file PUTs,
//! package-level (PREV) endpoints, deletes, and pull-through proxying to
//! ConanCenter (JSON verbatim / raw streams).
use pkglab_common::httphelpers::urlencode;
use pkglab_common::httphelpers::{blob_response, error, json, text};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use pkglab_common::{Artifact, Descriptor};
use std::io::Cursor;
use std::sync::Arc;

pub struct ConanState {
    pub registry: Arc<pkglab_common::Registry>,
    pub auth: Option<Arc<dyn pkglab_common::Auth>>,
    pub self_base: String,
}

/// Recipe coordinate + package id (8 path segments).
type Pkg8 = (String, String, String, String, String, String, String, String);
/// Recipe coordinate + pid + prev + filename (9 path segments).
type Pkg9 = (String, String, String, String, String, String, String, String, String);

pub const CAPABILITIES: &str =
    "revisions,revision_mode,server_hashes,server_caps_check_ignore_missing";

async fn authorize_write(state: &ConanState, headers: &HeaderMap) -> Result<(), Response> {
    pkglab_common::httphelpers::authorize_write(&state.auth, headers).await
}

fn shared_cache() -> pkglab_common::cache::MemCache {
    static CACHE: std::sync::OnceLock<pkglab_common::cache::MemCache> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| pkglab_common::cache::MemCache::new(std::time::Duration::from_secs(3600)))
        .clone()
}

pub fn router(state: Arc<ConanState>) -> axum::Router {
    let _s0 = state.clone();
    let s1 = state.clone();
    axum::Router::new()
        .route(
            "/{version}/ping",
            get(move |st: State<Arc<ConanState>>| async move {
                let _ = st;
                let mut r = StatusCode::OK.into_response();
                r.headers_mut().insert(
                    axum::http::HeaderName::from_static("x-conan-server-capabilities"),
                    axum::http::HeaderValue::from_static(CAPABILITIES),
                );
                r
            }),
        )
        .route("/{version}/users/authenticate", get(authenticate).post(authenticate))
        .route("/{version}/users/check_credentials", get(check_credentials))
        .route("/{version}/conans/search", get(search))
        .route("/{version}/conans/{name}/{ver}/{user}/{channel}/latest", get(latest))
        .route("/{version}/conans/{name}/{ver}/{user}/{channel}/revisions", get(revisions))
        .route("/{version}/conans/{name}/{ver}/{user}/{channel}/revisions/{rev}/files", get(files))
        .route("/{version}/conans/{name}/{ver}/{user}/{channel}/revisions/{rev}/files/{*filename}", get(file_get).put(file_put))
        .route("/{version}/conans/{name}/{ver}/{user}/{channel}/revisions/{rev}/packages/{pid}/latest", get(pkg_latest))
        .route("/{version}/conans/{name}/{ver}/{user}/{channel}/revisions/{rev}/packages/{pid}/revisions", get(pkg_revisions))
        .route("/{version}/conans/{name}/{ver}/{user}/{channel}/revisions/{rev}/packages/{pid}/revisions/{prev}/files", get(pkg_files_list))
        .route("/{version}/conans/{name}/{ver}/{user}/{channel}/revisions/{rev}/packages/{pid}/revisions/{prev}/files/{*filename}", get(pkg_file_get).put(pkg_file_put))
        .route("/{version}/conans/{name}/{ver}/{user}/{channel}/revisions/{rev}/upload_urls", post(recipe_upload_urls))
        .route("/{version}/conans/{name}/{ver}/{user}/{channel}/revisions/{rev}/packages/{pid}/revisions/{prev}/upload_urls", post(pkg_upload_urls))
        .route("/{version}/conans/{name}/{ver}/{user}/{channel}/revisions/{rev}/packages/{pid}", axum::routing::delete(delete_package))
        .route("/{version}/conans/{name}/{ver}/{user}/{channel}/revisions/{rev}", axum::routing::delete(delete_recipe_rev).get(proxy_recipe))
        .route("/{version}/conans/{name}/{ver}/{user}/{channel}", axum::routing::delete(delete_recipe))
        .route("/{version}/conans/{name}/{ver}/{user}/{channel}/revisions/{rev}/search", get(proxy_recipe))
        .route("/{version}/conans/{name}/{ver}/{user}/{channel}/revisions/{rev}/download_urls", get(proxy_recipe))
        .route("/{version}/files/{name}/{ver}/{user}/{channel}/{rev}/recipe/{*filename}", put(file_upload_recipe))
        .route("/{version}/files/{name}/{ver}/{user}/{channel}/{rev}/package/{pid}/{prev}/{*filename}", put(file_upload_package))
        .with_state(state)
        .fallback(move |req: axum::http::Request<Body>| {
            let st = s1.clone();
            async move {
                Ok::<_, std::convert::Infallible>(proxy_recipe_catchall(st, req).await)
            }
        })
}

#[allow(dead_code)]
fn strip_version(_st: &ConanState, path: &str) -> String {
    path.trim_start_matches('/')
        .trim_start_matches("pkgs/conan/")
        .trim_start_matches("conan/")
        .to_string()
}

async fn authenticate(State(st): State<Arc<ConanState>>, headers: HeaderMap) -> Response {
    // Conan 2.x login flow: the client authenticates (Basic), then uses the
    // returned raw token for subsequent uploads. Return the write token when
    // auth is enabled so it round-trips through the caller's token store.
    if let Some(auth) = &st.auth {
        let identity = auth.authenticate(&headers).await;
        if !identity.is_empty() {
            let token = auth.issue_token(&identity, &[], 3600).await;
            return text(StatusCode::OK, token, "text/plain");
        }
    }
    text(StatusCode::OK, "anonymous-token".into(), "text/plain")
}

async fn check_credentials(State(_st): State<Arc<ConanState>>) -> Response {
    json(StatusCode::OK, serde_json::json!({"ok": true}))
}

/// Conan search query glob ("pkg/*", "pkg/1.*", "*") over name/version.
fn matches_conan_query(name: &str, version: &str, q: &str) -> bool {
    if q.is_empty() || q == "*" {
        return true;
    }
    match q.split_once('/') {
        Some((qn, qv)) => {
            pkglab_common::globutil::glob_match(name, qn)
                && pkglab_common::globutil::glob_match(version, qv)
        }
        None => pkglab_common::globutil::glob_match(name, q),
    }
}

async fn search(
    State(st): State<Arc<ConanState>>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
) -> Response {
    let mut query = String::new();
    if let Some(q) = q {
        for pair in q.split('&') {
            let mut it = pair.splitn(2, '=');
            if it.next() == Some("q") {
                query = it.next().unwrap_or("").to_string();
            }
        }
    }
    let repos = st.registry.meta.list_repositories_by_format("conan").await.unwrap_or_default();
    let mut results: Vec<String> = Vec::new();
    for name in repos {
        let versions = st.registry.meta.list_versions("conan", &name).await.unwrap_or_default();
        if versions.is_empty() {
            if matches_conan_query(&name, "", &query) {
                results.push(name);
            }
            continue;
        }
        for v in versions {
            if matches_conan_query(&name, &v, &query) {
                results.push(format!("{name}/{v}"));
            }
        }
    }
    // Pull-through search from ConanCenter when nothing local matches.
    if results.is_empty() && !query.is_empty() {
        if let Some(remote) = st.registry.remote_sub("conan", "center") {
            if let Ok(body) = remote
                .get_cached(&shared_cache(), &format!("/v2/conans/search?q={}", urlencode(&query)))
                .await
            {
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json".to_string())],
                    body,
                )
                    .into_response();
            }
        }
        return error(StatusCode::BAD_GATEWAY, "upstream");
    }
    json(StatusCode::OK, serde_json::json!({"results": results}))
}

/// Persisted recipe revision: stored in the root artifact's proprietary as
/// {"revision": "..."} under the recipe's "" version.
async fn load_recipe_rev(st: &ConanState, name: &str, version: &str) -> String {
    if let Ok(art) = st.registry.meta.get("conan", name, "").await {
        if let Ok(m) = serde_json::from_slice::<serde_json::Value>(&art.proprietary) {
            if let Some(r) = m.get("revision").and_then(|r| r.as_str()) {
                return r.to_string();
            }
        }
    }
    let _ = (st, name, version);
    "0".to_string()
}

pub async fn save_recipe_rev(st: &ConanState, name: &str, version: &str, rev: &str) {
    // The recipe revision rides on the "" version artifact (format-level).
    let _ = st
        .registry
        .meta
        .put(Artifact {
            format: "conan".into(),
            repository: name.to_string(),
            version: String::new(),
            proprietary: serde_json::json!({"revision": rev, "recipe_version": version})
                .to_string()
                .into_bytes(),
            ..Default::default()
        })
        .await;
}

async fn latest(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, _user, _channel)): Path<(String, String, String, String, String)>,
) -> Response {
    if st.registry.meta.get("conan", &name, &ver).await.is_ok() {
        let rev = load_recipe_rev(&st, &name, &ver).await;
        return json(
            StatusCode::OK,
            serde_json::json!({"revision": rev, "time": "2024-01-01T00:00:00Z"}),
        );
    }
    proxy_json_path(&st, &format!("/v2/conans/{}/{}/_/_/latest", urlencode(&name), urlencode(&ver)))
        .await
}

async fn revisions(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, _user, _channel)): Path<(String, String, String, String, String)>,
) -> Response {
    if st.registry.meta.get("conan", &name, &ver).await.is_ok() {
        let rev = load_recipe_rev(&st, &name, &ver).await;
        return json(
            StatusCode::OK,
            serde_json::json!({"revisions": [{"revision": rev, "time": "2024-01-01T00:00:00Z"}]}),
        );
    }
    proxy_json_path(
        &st,
        &format!("/v2/conans/{}/{}/_/_/revisions", urlencode(&name), urlencode(&ver)),
    )
    .await
}

async fn files(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, _user, _channel, rev)): Path<(
        String,
        String,
        String,
        String,
        String,
        String,
    )>,
) -> Response {
    let mut files = serde_json::Map::new();
    if let Ok(art) = st.registry.meta.get("conan", &name, &ver).await {
        for b in &art.blobs {
            files.insert(b.name.clone(), serde_json::json!({"size": b.size, "hash": b.hex()}));
        }
    }
    if files.is_empty() {
        return proxy_json_path(
            &st,
            &format!(
                "/v2/conans/{}/{}/_/_/revisions/{}/files",
                urlencode(&name),
                urlencode(&ver),
                rev
            ),
        )
        .await;
    }
    json(StatusCode::OK, serde_json::json!({"files": files}))
}

async fn file_get(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, _user, _channel, rev, filename)): Path<(
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    )>,
) -> Response {
    if let Ok(art) = st.registry.meta.get("conan", &name, &ver).await {
        for b in &art.blobs {
            if b.name != filename {
                continue;
            }
            if let Ok(Some(mut r)) = st.registry.blobs.open(&b.digest).await {
                let mut data = Vec::new();
                if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                    return blob_response(data, &filename);
                }
            }
        }
    }
    proxy_raw_path(
        &st,
        &format!(
            "/v2/conans/{}/{}/_/_/revisions/{}/files/{}",
            urlencode(&name),
            urlencode(&ver),
            urlencode(&rev),
            urlencode(&filename)
        ),
    )
    .await
}

async fn file_put(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, _user, _channel, rev, filename)): Path<(
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    )>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    save_recipe_rev(&st, &name, &ver, &rev).await;
    let mut body = body;
    let data = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(c) => c.to_bytes().to_vec(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
    };
    store_file(&st, &name, &ver, &filename, data).await;
    let url = format!(
        "{}/v2/conans/{}/{}/_/_/revisions/{}/files/{}",
        st.self_base,
        urlencode(&name),
        urlencode(&ver),
        rev,
        urlencode(&filename)
    );
    json(StatusCode::OK, serde_json::json!({"files": {filename: url}}))
}

/// Store a recipe-level file (overwrite same-named file on re-publish).
pub async fn store_file(st: &ConanState, name: &str, version: &str, filename: &str, data: Vec<u8>) {
    let mut art = st.registry.meta.get("conan", name, version).await.unwrap_or_default();
    art.format = "conan".into();
    art.repository = name.to_string();
    art.version = version.to_string();
    if !data.is_empty() {
        if let Ok((hashes, size)) = pkglab_common::artifact::compute_hashes(&data[..]) {
            let digest = format!("sha256:{}", hashes.sha256);
            // Replace (not append) any same-named descriptor.
            let removed: Vec<Descriptor> =
                art.blobs.iter().filter(|b| b.name == filename).cloned().collect();
            art.blobs.retain(|b| b.name != filename);
            let mut cursor = Cursor::new(&data);
            if st.registry.blobs.put_if_absent(&digest, &mut cursor).await.is_ok() {
                art.blobs.push(Descriptor {
                    digest: digest.clone(),
                    size: size as i64,
                    name: filename.to_string(),
                    ..Default::default()
                });
            }
            for b in removed {
                if b.digest != digest {
                    let _ = st.registry.blobs.delete(&b.digest).await;
                }
            }
        }
    }
    let _ = st.registry.meta.put(art).await;
}

pub fn package_file_name(pid: &str, prev: &str, filename: &str) -> String {
    format!("package/{pid}/{prev}/{filename}")
}

async fn pkg_latest(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, _user, _channel, rev, pid)): Path<(
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    )>,
) -> Response {
    if let Ok(art) = st.registry.meta.get("conan", &name, &ver).await {
        let prefix = format!("package/{pid}/");
        for b in &art.blobs {
            if let Some(rest) = b.name.strip_prefix(&prefix) {
                let prev = rest.split('/').next().unwrap_or(rest);
                return json(
                    StatusCode::OK,
                    serde_json::json!({"revision": prev, "time": "2024-01-01T00:00:00Z"}),
                );
            }
        }
    }
    proxy_json_path(
        &st,
        &format!(
            "/v2/conans/{}/{}/_/_/revisions/{}/packages/{pid}/latest",
            urlencode(&name),
            urlencode(&ver),
            urlencode(&rev)
        ),
    )
    .await
}

async fn pkg_revisions(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, _user, _channel, rev, pid)): Path<(
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    )>,
) -> Response {
    if let Ok(art) = st.registry.meta.get("conan", &name, &ver).await {
        let prefix = format!("package/{pid}/");
        for b in &art.blobs {
            if let Some(rest) = b.name.strip_prefix(&prefix) {
                let prev = rest.split('/').next().unwrap_or(rest);
                return json(
                    StatusCode::OK,
                    serde_json::json!({"revisions": [{"revision": prev, "time": "2024-01-01T00:00:00Z"}]}),
                );
            }
        }
    }
    proxy_json_path(
        &st,
        &format!(
            "/v2/conans/{}/{}/_/_/revisions/{}/packages/{pid}/revisions",
            urlencode(&name),
            urlencode(&ver),
            urlencode(&rev)
        ),
    )
    .await
}

async fn pkg_files_list(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, _user, _channel, rev, pid, prev)): Path<Pkg8>,
) -> Response {
    let prefix = format!("package/{pid}/{prev}/");
    let mut out = serde_json::Map::new();
    if let Ok(art) = st.registry.meta.get("conan", &name, &ver).await {
        for b in &art.blobs {
            if let Some(rest) = b.name.strip_prefix(&prefix) {
                out.insert(rest.to_string(), serde_json::json!({"size": b.size, "hash": b.hex()}));
            }
        }
    }
    if out.is_empty() {
        return proxy_json_path(
            &st,
            &format!(
                "/v2/conans/{}/{}/_/_/revisions/{}/packages/{pid}/{prev}/files",
                urlencode(&name),
                urlencode(&ver),
                urlencode(&rev)
            ),
        )
        .await;
    }
    json(StatusCode::OK, serde_json::json!({"files": out}))
}

async fn pkg_file_get(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, _user, _channel, rev, pid, prev, filename)): Path<Pkg9>,
) -> Response {
    let full = package_file_name(&pid, &prev, &filename);
    if let Ok(art) = st.registry.meta.get("conan", &name, &ver).await {
        for b in &art.blobs {
            if b.name == full || b.name == filename {
                if let Ok(Some(mut r)) = st.registry.blobs.open(&b.digest).await {
                    let mut data = Vec::new();
                    if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                        return blob_response(data, &filename);
                    }
                }
            }
        }
    }
    proxy_raw_path(
        &st,
        &format!(
            "/v2/conans/{}/{}/_/_/revisions/{}/packages/{pid}/{prev}/files/{filename}",
            urlencode(&name),
            urlencode(&ver),
            urlencode(&rev)
        ),
    )
    .await
}

async fn pkg_file_put(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, _user, _channel, _rev, pid, prev, filename)): Path<Pkg9>,
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
    let full = package_file_name(&pid, &prev, &filename);
    store_file(&st, &name, &ver, &full, data).await;
    json(StatusCode::OK, serde_json::json!({"files": {filename: ""}}))
}

async fn recipe_upload_urls(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, user, channel, rev)): Path<(
        String,
        String,
        String,
        String,
        String,
        String,
    )>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    save_recipe_rev(&st, &name, &ver, &rev).await;
    let base = format!(
        "{}/v2/files/{}/{}/{}/{}/{}/recipe/",
        st.self_base,
        urlencode(&name),
        urlencode(&ver),
        urlencode(&user),
        urlencode(&channel),
        rev
    );
    let mut files = serde_json::Map::new();
    for f in ["conanfile.py", "conanmanifest.txt"] {
        files.insert(f.to_string(), serde_json::json!([format!("{base}{f}")]));
    }
    // The client may POST the set of files it intends to upload.
    let mut b = body;
    if let Ok(raw) = http_body_util::BodyExt::collect(&mut b).await {
        if let Ok(req) = serde_json::from_slice::<serde_json::Value>(raw.to_bytes().as_ref()) {
            if let Some(list) = req.get("files").and_then(|f| f.as_array()) {
                if !list.is_empty() {
                    files.clear();
                    for f in list {
                        if let Some(s) = f.as_str() {
                            files.insert(s.to_string(), serde_json::json!([format!("{base}{s}")]));
                        }
                    }
                }
            }
        }
    }
    json(StatusCode::OK, serde_json::json!({"files": files}))
}

async fn pkg_upload_urls(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, user, channel, rev, pid, prev)): Path<Pkg8>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    let base = format!(
        "{}/v2/files/{}/{}/{}/{}/{}/package/{}/{}/",
        st.self_base,
        urlencode(&name),
        urlencode(&ver),
        urlencode(&user),
        urlencode(&channel),
        rev,
        urlencode(&pid),
        urlencode(&prev)
    );
    let mut files = serde_json::Map::new();
    let mut b = body;
    if let Ok(raw) = http_body_util::BodyExt::collect(&mut b).await {
        if let Ok(req) = serde_json::from_slice::<serde_json::Value>(raw.to_bytes().as_ref()) {
            if let Some(list) = req.get("files").and_then(|f| f.as_array()) {
                for f in list {
                    if let Some(s) = f.as_str() {
                        files.insert(s.to_string(), serde_json::json!([format!("{base}{s}")]));
                    }
                }
            }
        }
    }
    json(StatusCode::OK, serde_json::json!({"files": files}))
}

async fn file_upload_recipe(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, _user, _channel, _rev, filename)): Path<(
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    )>,
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
    store_file(&st, &name, &ver, &filename, data).await;
    json(StatusCode::OK, serde_json::json!({"files": {filename: ""}}))
}

async fn file_upload_package(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, _user, _channel, _rev, pid, prev, filename)): Path<Pkg9>,
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
    let full = package_file_name(&pid, &prev, &filename);
    store_file(&st, &name, &ver, &full, data).await;
    json(StatusCode::OK, serde_json::json!({"files": {filename: ""}}))
}

async fn delete_recipe(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, _user, _channel)): Path<(String, String, String, String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    drop_artifact(&st, &name, &ver).await;
    StatusCode::OK.into_response()
}

async fn delete_recipe_rev(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, _user, _channel, _rev)): Path<(
        String,
        String,
        String,
        String,
        String,
        String,
    )>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    drop_artifact(&st, &name, &ver).await;
    StatusCode::OK.into_response()
}

async fn delete_package(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, _user, _channel, _rev, _pid)): Path<(
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    )>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    drop_artifact(&st, &name, &ver).await;
    StatusCode::OK.into_response()
}

pub async fn drop_artifact(st: &ConanState, name: &str, version: &str) {
    if let Ok(art) = st.registry.meta.get("conan", name, version).await {
        for b in &art.blobs {
            let _ = st.registry.blobs.delete(&b.digest).await;
        }
    }
    let _ = st.registry.meta.delete("conan", name, version).await;
    let _ = st.registry.meta.delete("conan", name, "").await;
}

/// Metadata-proxy catchall: if the recipe is stored locally answer from local
/// metadata, else forward to ConanCenter.
async fn proxy_recipe(
    State(st): State<Arc<ConanState>>,
    Path((_version, name, ver, _user, _channel, rev)): Path<(
        String,
        String,
        String,
        String,
        String,
        String,
    )>,
    req: axum::http::Request<Body>,
) -> Response {
    if let Ok(art) = st.registry.meta.get("conan", &name, &ver).await {
        if !art.blobs.is_empty() {
            let rel = req.uri().path();
            if rel.ends_with("/search") {
                // Binary package search: pid -> revisions map.
                let mut pids = serde_json::Map::new();
                for b in &art.blobs {
                    if let Some(rest) = b.name.strip_prefix("package/") {
                        if let Some((pid, rem)) = rest.split_once('/') {
                            let prev = rem.split('/').next().unwrap_or("");
                            pids.insert(
                                pid.to_string(),
                                serde_json::json!({
                                    "revisions": {prev: {"time": "2024-01-01T00:00:00Z"}},
                                    "settings": {},
                                    "options": {},
                                }),
                            );
                        }
                    }
                }
                return json(StatusCode::OK, serde_json::Value::Object(pids));
            }
            if rel.ends_with("/download_urls") {
                return json(StatusCode::OK, serde_json::json!({}));
            }
            return json(
                StatusCode::OK,
                serde_json::json!({"revision": rev, "time": "2024-01-01T00:00:00Z"}),
            );
        }
    }
    let up_path = req
        .uri()
        .path()
        .trim_start_matches('/')
        .trim_start_matches("pkgs/conan/")
        .trim_start_matches("conan/")
        .to_string();
    proxy_json_path(&st, &up_path).await
}

async fn proxy_recipe_catchall(state: Arc<ConanState>, req: axum::http::Request<Body>) -> Response {
    let path = req.uri().path().to_string();
    let up_path = path
        .trim_start_matches('/')
        .trim_start_matches("pkgs/conan/")
        .trim_start_matches("conan/")
        .to_string();
    if up_path.is_empty() || up_path == "v1" || up_path == "v2" {
        return StatusCode::NOT_FOUND.into_response();
    }
    proxy_json_path(&state, &up_path).await
}

async fn proxy_json_path(st: &ConanState, up_path: &str) -> Response {
    let Some(remote) = st.registry.remote_sub("conan", "center") else {
        return error(StatusCode::BAD_GATEWAY, "no upstream");
    };
    match remote.get_cached(&shared_cache(), up_path).await {
        Ok(body) => {
            (StatusCode::OK, [(header::CONTENT_TYPE, "application/json".to_string())], body)
                .into_response()
        }
        Err(pkglab_common::RegistryError::UpstreamStatus { status, .. })
            if status == 404 || status == 403 || status == 401 =>
        {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(_) => error(StatusCode::BAD_GATEWAY, "upstream"),
    }
}

async fn proxy_raw_path(st: &ConanState, up_path: &str) -> Response {
    let Some(remote) = st.registry.remote_sub("conan", "center") else {
        return error(StatusCode::BAD_GATEWAY, "no upstream");
    };
    match remote.get(up_path).await {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                return error(
                    StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                    "upstream",
                );
            }
            match resp.bytes().await {
                Ok(bytes) => Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/octet-stream")
                    .header(header::CONTENT_LENGTH, bytes.len())
                    .body(Body::from(bytes.to_vec()))
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
                Err(_) => error(StatusCode::BAD_GATEWAY, "upstream read"),
            }
        }
        Err(_) => error(StatusCode::BAD_GATEWAY, "upstream"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries() {
        assert!(matches_conan_query("pkg/1.0", "", "*"));
        assert!(matches_conan_query("pkg", "1.0", "pkg/*"));
        assert!(matches_conan_query("pkg", "1.0", "pkg/1.*"));
        assert!(!matches_conan_query("other", "1.0", "pkg/*"));
    }

    #[test]
    fn file_names() {
        assert_eq!(package_file_name("9a4b", "0", "conanfile.py"), "package/9a4b/0/conanfile.py");
    }
}

#[cfg(test)]
mod tests_revision {
    use super::*;

    /// `save_recipe_rev` + `load_recipe_rev` round-trip: the revision is
    /// persisted under the recipe's `""`-version artifact `proprietary`, keyed
    /// `{"revision": rev, "recipe_version": version}`.
    #[tokio::test]
    async fn recipe_rev_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let core = pkglab_core::Registry::open(dir.path()).unwrap();
        let reg = Arc::new(pkglab_common::Registry::new(
            core.blobs.clone(),
            core.meta.clone(),
            core.upstreams.clone(),
        ));
        let state = ConanState { registry: reg, auth: None, self_base: String::new() };

        save_recipe_rev(&state, "zlib", "1.3.1", "r1").await;
        assert_eq!(load_recipe_rev(&state, "zlib", "1.3.1").await, "r1");
    }

    #[test]
    fn package_file_names() {
        assert_eq!(package_file_name("p", "prev", "f"), "package/p/prev/f");
        assert_eq!(package_file_name("a", "b", "c/d"), "package/a/b/c/d");
    }
}
