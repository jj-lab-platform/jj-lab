//! Swift Package Registry (SE-0321 / SE-0416): `/{scope}/{name}` releases,
//! version metadata, source-archive zip, Package.swift serving, publish via
//! multipart `source-archive`, SCM-to-registry via a git clone cache, and
//! pull-through from the upstream registry.
use pkglab_common::httphelpers::text;
use pkglab_common::httphelpers::urlencode;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pkglab_common::{Artifact, Descriptor};
use std::io::{Cursor, Read};
use std::sync::Arc;
use zip::ZipArchive;

pub struct SwiftState {
    pub registry: Arc<pkglab_common::Registry>,
    pub auth: Option<Arc<dyn pkglab_common::Auth>>,
    pub self_base: String,
}

fn swift_json(status: StatusCode, v: serde_json::Value) -> Response {
    let mut resp =
        (status, [(header::CONTENT_TYPE, "application/json".to_string())], v.to_string())
            .into_response();
    resp.headers_mut().insert(
        axum::http::HeaderName::from_static("content-version"),
        axum::http::HeaderValue::from_static("1"),
    );
    resp
}

fn swift_error(status: StatusCode, detail: &str) -> Response {
    let mut resp = (
        status,
        [(header::CONTENT_TYPE, "application/problem+json".to_string())],
        serde_json::json!({
            "type": "about:blank",
            "title": status.canonical_reason().unwrap_or("error"),
            "status": status.as_u16(),
            "detail": detail,
        })
        .to_string(),
    )
        .into_response();
    resp.headers_mut().insert(
        axum::http::HeaderName::from_static("content-version"),
        axum::http::HeaderValue::from_static("1"),
    );
    resp
}

async fn authorize_write(state: &SwiftState, headers: &HeaderMap) -> Result<(), Response> {
    pkglab_common::httphelpers::authorize_write(&state.auth, headers).await
}

pub fn router(state: Arc<SwiftState>) -> axum::Router {
    let s0 = state.clone();
    axum::Router::new()
        .route(
            "/login",
            axum::routing::any(login),
        )
        .route("/identifiers", get(identifiers))
        .fallback(move |req: axum::http::Request<Body>| {
            let st = s0.clone();
            async move { Ok::<_, std::convert::Infallible>(dispatch(st, req).await) }
        })
        .with_state(state)
}

/// SwiftPM `login` probes GET then POSTs the credential. Accept every method
/// (SwiftPM varies by version) and return the write token when auth is
/// enabled so it round-trips through SwiftPM's netrc store.
async fn login(State(st): State<Arc<SwiftState>>, headers: HeaderMap) -> Response {
    if let Some(auth) = &st.auth {
        let identity = auth.authenticate(&headers).await;
        if !identity.is_empty() {
            let token = auth.issue_token(&identity, &[], 3600).await;
            return swift_json(StatusCode::OK, serde_json::json!({"token": token}));
        }
    }
    swift_json(StatusCode::OK, serde_json::json!({"token": "valid"}))
}

async fn identifiers(
    State(st): State<Arc<SwiftState>>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
) -> Response {
    // ?url=<git-url>: derive identity + persist the git URL for SCM-to-registry.
    let mut url_param = String::new();
    if let Some(q) = q {
        for pair in q.split('&') {
            let mut it = pair.splitn(2, '=');
            if it.next() == Some("url") {
                url_param = it.next().unwrap_or("").replace("%3A", ":").replace("%2F", "/");
            }
        }
    }
    if !url_param.is_empty() {
        let (scope, name) = gitcache_identity(&url_param);
        let id = format!("{scope}.{name}");
        let _ = st.registry.meta.set_meta(&format!("swift-git:{id}"), url_param.as_bytes()).await;
        return swift_json(StatusCode::OK, serde_json::json!({"identifiers": [id]}));
    }
    let repos = st.registry.meta.list_repositories_by_format("swift").await.unwrap_or_default();
    swift_json(StatusCode::OK, serde_json::json!({"identifiers": repos}))
}

async fn git_url_for(state: &SwiftState, id: &str) -> String {
    match state.registry.meta.get_meta(&format!("swift-git:{id}")).await {
        Ok(Some(b)) => String::from_utf8_lossy(&b).to_string(),
        _ => String::new(),
    }
}

async fn dispatch(state: Arc<SwiftState>, req: axum::http::Request<Body>) -> Response {
    let method = req.method().clone();
    let headers = req.headers().clone();
    let path = req.uri().path().trim_start_matches('/').to_string();
    let path = path
        .strip_prefix("pkgs/swift/")
        .or_else(|| path.strip_prefix("swift/"))
        .unwrap_or(&path)
        .to_string();

    // SwiftPM `login` POSTs to the registry root (not /login). Route it to the
    // login handler so `--token` round-trips.
    if method == Method::POST && (path.is_empty() || path == "/") {
        if let Some(auth) = &state.auth {
            let identity = auth.authenticate(&headers).await;
            if !identity.is_empty() {
                let token = auth.issue_token(&identity, &[], 3600).await;
                return swift_json(StatusCode::OK, serde_json::json!({"token": token}));
            }
        }
        return swift_json(StatusCode::OK, serde_json::json!({"token": "valid"}));
    }

    if method == Method::OPTIONS {
        let mut resp = StatusCode::OK.into_response();
        let h = resp.headers_mut();
        h.insert(header::ALLOW, axum::http::HeaderValue::from_static("GET, HEAD, PUT, OPTIONS"));
        h.insert(
            axum::http::HeaderName::from_static("link"),
            axum::http::HeaderValue::from_static(
                "<https://github.com/swiftlang/swift-package-manager/blob/main/Documentation/PackageRegistry/Registry.md>; rel=\"service-doc\"",
            ),
        );
        h.insert(
            axum::http::HeaderName::from_static("content-version"),
            axum::http::HeaderValue::from_static("1"),
        );
        return resp;
    }

    if method == Method::PUT {
        if let Err(resp) = authorize_write(&state, &headers).await {
            return resp;
        }
        return put_path(state, &path, headers, req.into_body()).await;
    }
    if method != Method::GET && method != Method::HEAD {
        return swift_error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
    }

    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
        return swift_error(StatusCode::NOT_FOUND, "invalid path");
    }
    let scope = parts[0].to_string();
    let name = parts[1].to_string();
    let id = format!("{scope}.{name}");

    match parts.len() {
        2 => releases(&state, &scope, &name, &id).await,
        3 => {
            let ver = parts[2];
            if let Some(v) = ver.strip_suffix(".zip") {
                source_zip(&state, &id, v, &name).await
            } else {
                version_meta(&state, &id, ver, &name).await
            }
        }
        4 if parts[3] == "Package.swift" => package_swift(&state, &id, parts[2], &name).await,
        _ => swift_error(StatusCode::NOT_FOUND, "release not found"),
    }
}

async fn releases(state: &SwiftState, scope: &str, name: &str, full: &str) -> Response {
    let versions = state.registry.meta.list_versions("swift", full).await.unwrap_or_default();
    if versions.is_empty() {
        // SCM-to-registry: expose each version tag of the persisted clone.
        let gu = git_url_for(state, full).await;
        if !gu.is_empty() {
            if let Some(tags) = gitcache_tags(&gu) {
                if !tags.is_empty() {
                    let mut rels = serde_json::Map::new();
                    for t in tags {
                        rels.insert(
                            t.clone(),
                            serde_json::json!({
                                "url": format!("{}/{}/{}/{}.zip",
                                    state.self_base, urlencode(scope), urlencode(name), t),
                            }),
                        );
                    }
                    return swift_json(StatusCode::OK, serde_json::json!({"releases": rels}));
                }
            }
        }
        // Pull-through from upstream (cached index).
        if let Some(remote) = state.registry.remote("swift", None) {
            if let Ok(body) = remote
                .get_cached(&shared_cache(), &format!("/{}={}", urlencode(scope), urlencode(name)))
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
        return swift_json(StatusCode::OK, serde_json::json!({"releases": {}}));
    }
    let mut rels = serde_json::Map::new();
    for v in &versions {
        rels.insert(
            v.clone(),
            serde_json::json!({
                "url": format!("{}/{}/{}/{}.zip",
                    state.self_base, urlencode(scope), urlencode(name), v),
            }),
        );
    }
    swift_json(StatusCode::OK, serde_json::json!({"releases": rels}))
}

async fn version_meta(state: &SwiftState, full: &str, version: &str, name: &str) -> Response {
    let _ = name;
    let checksum = state
        .registry
        .meta
        .get("swift", full, version)
        .await
        .ok()
        .and_then(|art| art.blobs.first().map(|b| b.hex().to_string()))
        .unwrap_or_default();
    swift_json(
        StatusCode::OK,
        serde_json::json!({
            "id": full.replace('/', "."),
            "version": version,
            "resources": [{"name": "source-archive", "type": "application/zip", "checksum": checksum}],
            "metadata": {"description": ""},
            "publishedAt": "2024-01-01T00:00:00Z",
        }),
    )
}

async fn source_zip(state: &SwiftState, full: &str, version: &str, name: &str) -> Response {
    let filename = format!("{name}-{version}.zip");
    if let Ok(art) = state.registry.meta.get("swift", full, version).await {
        for b in &art.blobs {
            if let Ok(Some(mut r)) = state.registry.blobs.open(&b.digest).await {
                let mut data = Vec::new();
                if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                    return zip_response(data, &filename);
                }
            }
        }
    }
    // SCM-to-registry: archive from a cached git clone.
    let gu = git_url_for(state, full).await;
    if !gu.is_empty() {
        if let Some(data) = gitcache_archive(&gu, version) {
            if !data.is_empty() {
                store_version_src(state, full, version, &filename, data.clone(), "pull").await;
                return zip_response(data, &filename);
            }
        }
    }
    // Pull-through.
    match state.registry.fetch("swift", "", &format!("/{}/{version}.zip", urlencode(full))).await {
        Ok(fetched) => {
            store_version_src(state, full, version, &filename, fetched.data.clone(), "pull").await;
            zip_response(fetched.data, &filename)
        }
        Err(_) => swift_json(StatusCode::NOT_FOUND, serde_json::json!({"error": "not found"})),
    }
}

fn zip_response(data: Vec<u8>, filename: &str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_LENGTH, data.len())
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{filename}\""))
        .body(Body::from(data))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn package_swift(state: &SwiftState, full: &str, version: &str, name: &str) -> Response {
    // Prefer the real Package.swift from the stored source archive.
    if let Ok(art) = state.registry.meta.get("swift", full, version).await {
        for b in &art.blobs {
            if let Ok(Some(mut r)) = state.registry.blobs.open(&b.digest).await {
                let mut data = Vec::new();
                if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                    if let Some(manifest) = extract_package_swift(&data) {
                        let mut resp = text(StatusCode::OK, manifest, "text/x-swift");
                        resp.headers_mut().insert(
                            axum::http::HeaderName::from_static("content-version"),
                            axum::http::HeaderValue::from_static("1"),
                        );
                        return resp;
                    }
                }
            }
        }
    }
    let manifest = format!(
        "// swift-tools-version:5.9\nimport PackageDescription\n\nlet package = Package(\n    name: \"{name}\",\n    products: [.library(name: \"{name}\", targets: [\"{name}\"])],\n    targets: [.target(name: \"{name}\")]\n)\n"
    );
    let mut resp = text(StatusCode::OK, manifest, "text/x-swift");
    resp.headers_mut().insert(
        axum::http::HeaderName::from_static("content-version"),
        axum::http::HeaderValue::from_static("1"),
    );
    resp
}

/// Read Package.swift out of a source archive zip.
fn extract_package_swift(zip_data: &[u8]) -> Option<String> {
    let mut archive = ZipArchive::new(Cursor::new(zip_data)).ok()?;
    for i in 0..archive.len() {
        let Ok(mut f) = archive.by_index(i) else { continue };
        if f.name() == "Package.swift" || f.name().ends_with("/Package.swift") {
            let mut out = String::new();
            if f.read_to_string(&mut out).is_ok() {
                return Some(out);
            }
        }
    }
    None
}

async fn put_path(state: Arc<SwiftState>, path: &str, headers: HeaderMap, body: Body) -> Response {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() != 3 {
        return swift_json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "invalid upload path"}),
        );
    }
    let scope = parts[0].to_string();
    let name = parts[1].to_string();
    let ver = parts[2].to_string();
    let full = format!("{scope}.{name}");

    let mut body = body;
    let raw = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(c) => c.to_bytes().to_vec(),
        Err(_) => {
            return swift_json(StatusCode::BAD_REQUEST, serde_json::json!({"error": "read error"}))
        }
    };
    let ct = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    // SE-0321 publish is multipart/form-data whose "source-archive" part
    // carries the zip (Swift sends Content-Transfer-Encoding: binary).
    let zip_data = if ct.starts_with("multipart/form-data") {
        pkglab_common::multipart::extract_field(&raw, ct, "source-archive").unwrap_or_default()
    } else {
        raw.to_vec()
    };
    if zip_data.is_empty() {
        return swift_json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "missing source-archive"}),
        );
    }

    let Ok((hashes, _)) = pkglab_common::artifact::compute_hashes(&zip_data[..]) else {
        return swift_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"error": "hash failed"}),
        );
    };
    let filename = format!("{name}-{ver}.zip");
    store_version(&state, &full, &ver, &filename, zip_data).await;
    swift_json(
        StatusCode::CREATED,
        serde_json::json!({
            "id": format!("{scope}.{name}"),
            "version": ver,
            "resources": [{"name": "source-archive", "type": "application/zip", "checksum": hashes.sha256}],
        }),
    )
}

async fn store_version(
    state: &SwiftState,
    full: &str,
    version: &str,
    filename: &str,
    data: Vec<u8>,
) {
    store_version_src(state, full, version, filename, data, "push").await
}

pub async fn store_version_src(
    state: &SwiftState,
    full: &str,
    version: &str,
    filename: &str,
    data: Vec<u8>,
    source: &str,
) {
    let mut art = Artifact {
        format: "swift".into(),
        repository: full.to_string(),
        version: version.to_string(),
        source: source.to_string(),
        ..Default::default()
    };
    if !data.is_empty() {
        if let Ok((hashes, size)) = pkglab_common::artifact::compute_hashes(&data[..]) {
            let digest = format!("sha256:{}", hashes.sha256);
            let mut cursor = Cursor::new(&data);
            if state.registry.blobs.put_if_absent(&digest, &mut cursor).await.is_ok() {
                art.blobs.push(Descriptor {
                    digest,
                    size: size as i64,
                    name: filename.to_string(),
                    ..Default::default()
                });
            }
        }
    }
    let _ = state.registry.meta.put(art).await;
}

// --- git cache (SCM-to-registry) --------------------------------------------
//
// Shallow-clone per-URL under a temp cache dir and refresh tags; archive a
// tag as a zip. Mirrors the reference `gitcache` package (git CLI).

fn cache_dir(url: &str) -> std::path::PathBuf {
    let key = url.trim_end_matches(".git");
    let key: String = key
        .chars()
        .map(|c| match c {
            '/' | ':' | '?' | '&' => '_',
            other => other,
        })
        .collect();
    std::env::temp_dir().join("pkglab-spm-git-cache").join(key)
}

fn run(dir: Option<&std::path::Path>, cmd: &str, args: &[&str]) -> Option<Vec<u8>> {
    let mut c = std::process::Command::new(cmd);
    c.args(args);
    if let Some(d) = dir {
        c.current_dir(d);
    }
    c.stdout(std::process::Stdio::piped());
    c.stderr(std::process::Stdio::null());
    let out = c.output().ok()?;
    if out.status.success() {
        Some(out.stdout)
    } else {
        None
    }
}

fn gitcache_clone(url: &str) -> Option<std::path::PathBuf> {
    let dir = cache_dir(url);
    let _ = std::fs::create_dir_all(dir.parent()?);
    if dir.exists() {
        run(Some(&dir), "git", &["fetch", "--tags", "--depth", "1"]);
        return Some(dir);
    }
    run(None, "git", &["clone", "--depth", "1", url, &dir.display().to_string()])?;
    run(Some(&dir), "git", &["fetch", "--tags", "--depth", "1"]);
    Some(dir)
}

fn gitcache_tags(url: &str) -> Option<Vec<String>> {
    let dir = gitcache_clone(url)?;
    let out = run(Some(&dir), "git", &["tag"])?;
    let tags = String::from_utf8_lossy(&out)
        .lines()
        .map(|l| l.trim().trim_start_matches('v').to_string())
        .filter(|l| !l.is_empty())
        .collect();
    Some(tags)
}

fn gitcache_archive(url: &str, tag: &str) -> Option<Vec<u8>> {
    let dir = gitcache_clone(url)?;
    // Reconstruct the original ref: tags may carry the "v" prefix.
    let mut full_tag = tag.to_string();
    if !tag.starts_with('v') {
        let out = run(Some(&dir), "git", &["tag", "-l", &format!("v{tag}"), tag])?;
        if String::from_utf8_lossy(&out).split_whitespace().any(|l| l == format!("v{tag}")) {
            full_tag = format!("v{tag}");
        }
    }
    run(Some(&dir), "git", &["archive", "--format=zip", &full_tag])
}

/// Derive the registry identity (scope.name) from a git URL:
/// `https://github.com/apple/swift-crypto.git` -> `apple.swift-crypto`.
pub fn gitcache_identity(url: &str) -> (String, String) {
    let mut u = url.trim_end_matches(".git").to_string();
    if let Some(i) = u.find("://") {
        u = u[i + 3..].to_string();
    }
    if let Some(i) = u.find('@') {
        u = u[i + 1..].to_string();
    }
    let parts: Vec<&str> = u.trim_matches('/').split('/').collect();
    if parts.len() < 2 {
        let name = parts.last().copied().unwrap_or("package");
        return ("swift".to_string(), name.to_string());
    }
    let scope = parts[parts.len() - 2].to_string();
    let name = parts[parts.len() - 1].to_string();
    let scope = if scope.is_empty() { "swift".to_string() } else { scope };
    (scope, name)
}

fn shared_cache() -> pkglab_common::cache::MemCache {
    static CACHE: std::sync::OnceLock<pkglab_common::cache::MemCache> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| pkglab_common::cache::MemCache::new(std::time::Duration::from_secs(3600)))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity() {
        assert_eq!(
            gitcache_identity("https://github.com/apple/swift-crypto.git"),
            ("apple".into(), "swift-crypto".into())
        );
        assert_eq!(gitcache_identity("https://gitlab.com/foo/bar"), ("foo".into(), "bar".into()));
    }

    #[test]
    fn cache_key() {
        let d = cache_dir("https://github.com/a/b.git");
        assert!(d.to_string_lossy().contains("pkglab-spm-git-cache"));
    }
}

#[cfg(test)]
mod tests_extras {
    use super::*;
    use std::io::Write;

    #[test]
    fn package_swift_extraction() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("Package.swift", zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(b"// swift-tools-version:5.9\n").unwrap();
            z.finish().unwrap();
        }
        let got = extract_package_swift(&buf.into_inner()).unwrap();
        assert!(got.contains("swift-tools-version:5.9"));
    }
}
