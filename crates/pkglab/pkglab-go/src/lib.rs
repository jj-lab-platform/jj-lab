//! Go module proxy protocol (GOPROXY): `@v/list`, `@latest`, `.info`,
//! `.mod`, `.zip` endpoints plus a local `PUT /upload`.
//!
//! `.mod` is served by extracting go.mod from the locally stored source zip
//! (entries named `{module}@v{version}/go.mod`). Misses fall through to the
//! configured upstream (proxy.golang.org).

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pkglab_common::httphelpers::{blob_response, error, text};
use pkglab_common::remote::Remote;
use pkglab_common::{Artifact, Descriptor};
use std::io::Cursor;
use std::sync::Arc;

pub struct GoState {
    pub registry: Arc<pkglab_common::Registry>,
    pub auth: Option<Arc<dyn pkglab_common::Auth>>,
}

fn json_ok(v: serde_json::Value) -> Response {
    (StatusCode::OK, [(header::CONTENT_TYPE, "application/json".to_string())], v.to_string())
        .into_response()
}

/// Encode a module path per the Go module proxy spec: uppercase letters
/// become `!<lower>`.
pub fn encode_module_path(m: &str) -> String {
    let mut out = String::with_capacity(m.len());
    for c in m.chars() {
        if c.is_ascii_uppercase() {
            out.push('!');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn now_rfc3339() -> String {
    // Informational timestamp only.
    "2024-01-01T00:00:00Z".to_string()
}

pub fn router(state: Arc<GoState>) -> axum::Router {
    let st0 = state.clone();
    axum::Router::new()
        .route("/upload", axum::routing::put(upload_with_query))
        .fallback(move |req: axum::http::Request<Body>| {
            let st = st0.clone();
            async move {
                let method = req.method().clone();
                let path = req.uri().path().to_string();
                let headers = req.headers().clone();
                let body = req.into_body();
                Ok::<_, std::convert::Infallible>(dispatch(st, method, &path, headers, body).await)
            }
        })
        .with_state(state)
}

async fn dispatch(
    state: Arc<GoState>,
    method: Method,
    path: &str,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let p = path.trim_start_matches('/');
    // Strip the mount prefix segments ("/pkgs/go/") when nested.
    let p = p.strip_prefix("pkgs/go/").or_else(|| p.strip_prefix("go/")).unwrap_or(p);

    if method == Method::PUT && p == "upload" {
        let query = path.split_once('?').map(|(_, q)| q.to_string());
        return upload(State(state.clone()), axum::extract::RawQuery(query), headers, body).await;
    }
    if method != Method::GET {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    if let Some(module) = p.strip_suffix("/@latest") {
        return latest(&state, module).await;
    }
    if let Some(module) = p.strip_suffix("/@v/list") {
        return versions(&state, module).await;
    }
    for ext in [".info", ".mod", ".zip"] {
        if let Some(rest) = p.strip_suffix(ext) {
            let (module, version) = split_module_version(rest);
            if version.is_empty() {
                break;
            }
            return match ext {
                ".info" => info(&state, &module, &version).await,
                ".mod" => go_mod(&state, &module, &version).await,
                ".zip" => module_zip(&state, &module, &version).await,
                _ => unreachable!(),
            };
        }
    }
    StatusCode::NOT_FOUND.into_response()
}

use axum::http::Method;

fn split_module_version(rest: &str) -> (String, String) {
    match rest.find("/@v/") {
        Some(i) => (rest[..i].to_string(), rest[i + "/@v/".len()..].to_string()),
        None => (rest.to_string(), String::new()),
    }
}

async fn latest(state: &GoState, module: &str) -> Response {
    let versions = state.registry.meta.list_versions("go", module).await.unwrap_or_default();
    if let Some(v) = pkglab_common::versioncmp::highest(versions.iter()) {
        return json_ok(serde_json::json!({"Version": v, "Time": now_rfc3339()}));
    }
    if let Some(v) = versions.last() {
        return json_ok(serde_json::json!({"Version": v, "Time": now_rfc3339()}));
    }
    proxy_one(state, module, "@latest", "application/json").await
}

async fn versions(state: &GoState, module: &str) -> Response {
    let mut versions = state.registry.meta.list_versions("go", module).await.unwrap_or_default();
    if !versions.is_empty() {
        pkglab_common::versioncmp::sort_vec(&mut versions);
        return text(StatusCode::OK, versions.join("\n"), "text/plain");
    }
    proxy_one(state, module, "@v/list", "text/plain").await
}

async fn info(state: &GoState, module: &str, version: &str) -> Response {
    if state.registry.meta.get("go", module, version).await.is_ok() {
        return json_ok(serde_json::json!({"Version": version, "Time": now_rfc3339()}));
    }
    proxy_one(state, module, &format!("@v/{version}.info"), "application/json").await
}

async fn go_mod(state: &GoState, module: &str, version: &str) -> Response {
    // Serve go.mod extracted from the locally stored source zip.
    if let Ok(art) = state.registry.meta.get("go", module, version).await {
        for b in &art.blobs {
            if let Ok(Some(mut r)) = state.registry.blobs.open(&b.digest).await {
                let mut data = Vec::new();
                if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                    if let Some(gm) = extract_go_mod(&data) {
                        return text(StatusCode::OK, gm, "text/plain; charset=utf-8");
                    }
                }
            }
        }
    }
    proxy_one(state, module, &format!("@v/{version}.mod"), "text/plain; charset=utf-8").await
}

/// Read go.mod out of a module source zip. Entries are named
/// `{module}@v{version}/go.mod`.
fn extract_go_mod(zip_data: &[u8]) -> Option<String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_data)).ok()?;
    for i in 0..archive.len() {
        let Ok(mut f) = archive.by_index(i) else { continue };
        let name = f.name().to_string();
        if name.ends_with("/go.mod") || name == "go.mod" {
            use std::io::Read as _;
            let mut out = String::new();
            if f.read_to_string(&mut out).is_ok() {
                return Some(out);
            }
        }
    }
    None
}

async fn module_zip(state: &GoState, module: &str, version: &str) -> Response {
    let filename = format!("{module}-{version}.zip");
    if let Ok(art) = state.registry.meta.get("go", module, version).await {
        for b in &art.blobs {
            if let Ok(Some(mut r)) = state.registry.blobs.open(&b.digest).await {
                let mut data = Vec::new();
                if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                    return blob_response(data, &filename);
                }
            }
        }
    }
    let ep = encode_module_path(module);
    match proxy_fetch(state, &format!("/{ep}/@v/{version}.zip")).await {
        Ok(data) => {
            store_version_src(state, module, version, data.clone(), "pull").await;
            blob_response(data, &filename)
        }
        Err(_) => error(StatusCode::NOT_FOUND, "not found"),
    }
}

async fn proxy_one(state: &GoState, module: &str, suffix: &str, ct: &str) -> Response {
    let ep = encode_module_path(module);
    match proxy_fetch(state, &format!("/{ep}/{suffix}")).await {
        Ok(body) => text(StatusCode::OK, String::from_utf8_lossy(&body).to_string(), ct),
        Err(e)
            if e.is_unknown()
                || matches!(
                    e,
                    pkglab_common::RegistryError::UpstreamStatus { status: 404 | 410, .. }
                ) =>
        {
            error(StatusCode::NOT_FOUND, "not found")
        }
        Err(e) if matches!(e, pkglab_common::RegistryError::UpstreamStatus { .. }) => error(
            StatusCode::from_u16(match e {
                pkglab_common::RegistryError::UpstreamStatus { status, .. } => status,
                _ => 502,
            })
            .unwrap_or(StatusCode::BAD_GATEWAY),
            "upstream",
        ),
        Err(_) => error(StatusCode::BAD_GATEWAY, "upstream"),
    }
}

async fn proxy_fetch(state: &GoState, path: &str) -> Result<Vec<u8>, pkglab_common::RegistryError> {
    let remote: Remote = state
        .registry
        .remote("go", None)
        .ok_or(pkglab_common::RegistryError::NoUpstream("go".into()))?;
    remote.get_bytes(path).await
}

async fn upload_with_query(
    State(state): State<Arc<GoState>>,
    raw: axum::extract::RawQuery,
    headers: HeaderMap,
    body: Body,
) -> Response {
    upload(State(state), raw, headers, body).await
}

async fn upload(
    st: State<Arc<GoState>>,
    raw: axum::extract::RawQuery,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let state = st.0;
    if let Err(resp) = pkglab_common::httphelpers::authorize_write(&state.auth, &headers).await {
        return resp;
    }
    // Query params carry name/module + version (the Go client flow); fall
    // back to a JSON body {name,module,version}.
    let mut name = String::new();
    let mut version = String::new();
    if let Some(q) = raw.0 {
        for pair in q.split('&') {
            let mut it = pair.splitn(2, '=');
            match it.next().unwrap_or("") {
                "name" | "module" => name = it.next().unwrap_or("").to_string(),
                "version" => version = it.next().unwrap_or("").to_string(),
                _ => {}
            }
        }
    }
    let mut body = body;
    let data = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(c) => c.to_bytes().to_vec(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
    };
    if name.is_empty() {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&data) {
            name = v
                .get("name")
                .or_else(|| v.get("module"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            version = v.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string();
        }
    }
    if name.is_empty() {
        name = "go/local".into();
    }
    if version.is_empty() {
        version = format!(
            "v{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        );
    }
    store_version(&state, &name, &version, data).await;
    (
        StatusCode::CREATED,
        [(header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({"ok": true}).to_string(),
    )
        .into_response()
}

async fn store_version(state: &GoState, module: &str, version: &str, data: Vec<u8>) {
    store_version_src(state, module, version, data, "push").await
}

pub async fn store_version_src(
    state: &GoState,
    module: &str,
    version: &str,
    data: Vec<u8>,
    source: &str,
) {
    let version = if version.is_empty() { "v0.0.0" } else { version };
    let mut art = Artifact {
        format: "go".into(),
        repository: module.to_string(),
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
                    name: format!("{module}-{version}.zip"),
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
    use std::io::Write;

    #[test]
    fn encode_module_paths() {
        assert_eq!(encode_module_path("github.com/User/Repo"), "github.com/!user/!repo");
        assert_eq!(encode_module_path("example.com/simple"), "example.com/simple");
        assert_eq!(encode_module_path(""), "");
    }

    #[test]
    fn split_module_version_paths() {
        assert_eq!(
            split_module_version("github.com/u/r/@v/v1.0.0"),
            ("github.com/u/r".to_string(), "v1.0.0".to_string())
        );
        assert_eq!(split_module_version("plain"), ("plain".to_string(), "".to_string()));
    }

    #[test]
    fn extract_go_mod_from_zip() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("example.com/mod@v1.0.0/go.mod", zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(b"module example.com/mod\n\ngo 1.21\n").unwrap();
            z.finish().unwrap();
        }
        let zip_bytes = buf.into_inner();
        let got = extract_go_mod(&zip_bytes).unwrap();
        assert_eq!(got, "module example.com/mod\n\ngo 1.21\n");
    }
}
