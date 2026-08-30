//! PyPI (Python Package Index) protocol: PEP 503 normalized `/simple/`
//! index (HTML + PEP 691 JSON), PEP 658 `.metadata` sidecars extracted from
//! wheel zips, multipart upload, and pull-through with href rewriting
//! (absolute and relative upstream hrefs, `#sha256=` fragments dropped).
use pkglab_common::httphelpers::urlencode;
use pkglab_common::httphelpers::{blob_response, error, text};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, post};
use pkglab_common::{Artifact, Descriptor};
use std::io::{Cursor, Read};
use std::sync::Arc;

pub struct PyPiState {
    pub registry: Arc<pkglab_common::Registry>,
    pub auth: Option<Arc<dyn pkglab_common::Auth>>,
    /// Public base URL of this registry (self), used to rewrite hrefs.
    pub self_base: String,
}

/// PEP 503 project-name normalization: lowercase and collapse runs of
/// `-_.` into a single `-`.
pub fn normalize_name(name: &str) -> String {
    let name = name.to_lowercase();
    let mut out = String::with_capacity(name.len());
    let mut last_was_sep = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            if !last_was_sep && !out.is_empty() {
                out.push('-');
            }
            last_was_sep = true;
        } else {
            out.push(c);
            last_was_sep = false;
        }
    }
    out.trim_matches('-').to_string()
}

fn wants_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|h| h.contains("application/vnd.pypi.simple.v1+json"))
        .unwrap_or(false)
}

async fn authorize_write(state: &PyPiState, headers: &HeaderMap) -> Result<(), Response> {
    pkglab_common::httphelpers::authorize_write(&state.auth, headers).await
}

pub fn router(state: Arc<PyPiState>) -> axum::Router {
    let s0 = state.clone();
    let s1 = state.clone();
    let s2 = state.clone();
    axum::Router::new()
        .route("/upload", post(upload))
        .route("/", post(upload))
        .route("/simple/", axum::routing::get(move |_st: State<Arc<PyPiState>>, h: HeaderMap| async move { simple_root(&s0, &h).await }))
        .route("/simple", axum::routing::get(move |_st: State<Arc<PyPiState>>, h: HeaderMap| async move { simple_root(&s2, &h).await }))
        .route("/simple/{*path}", axum::routing::get(move |_st: State<Arc<PyPiState>>, h: HeaderMap, p: axum::extract::Path<String>| async move { simple_dispatch(&s1, &h, p.0).await }))
        .route("/api/projects/{name}", axum::routing::delete(delete_project))
        .route("/api/projects/{name}/files", delete(delete_files).post(delete_files))
        .route("/api/projects/{name}/releases/{version}", axum::routing::delete(delete_release))
        .with_state(state)
}

async fn simple_dispatch(state: &PyPiState, headers: &HeaderMap, raw: String) -> Response {
    // raw is everything after /simple/
    let rel = raw.trim_matches('/');
    if rel.is_empty() {
        return simple_root(state, headers).await;
    }
    let parts: Vec<&str> = rel.split('/').collect();
    match parts.len() {
        1 => simple_project(state, headers, &normalize_name(parts[0])).await,
        2 => {
            let project = normalize_name(parts[0]);
            let fname = parts[1].to_string();
            if fname.ends_with(".metadata") {
                metadata_file(state, &project, fname.trim_end_matches(".metadata")).await
            } else {
                simple_file(state, &project, &fname).await
            }
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn simple_root(state: &PyPiState, headers: &HeaderMap) -> Response {
    let repos = state.registry.meta.list_repositories_by_format("pypi").await.unwrap_or_default();
    if wants_json(headers) {
        let projects: Vec<serde_json::Value> =
            repos.iter().map(|name| serde_json::json!({"name": name})).collect();
        return simple_json(
            serde_json::json!({"meta": {"api-version": "1.4"}, "projects": projects}),
        );
    }
    let base = &state.self_base;
    let mut sb = String::from("<!DOCTYPE html><html><body>\n");
    for name in &repos {
        sb.push_str(&format!("<a href=\"{base}/simple/{}/\">{name}</a>\n", urlencode(name)));
    }
    sb.push_str("</body></html>");
    text(StatusCode::OK, sb, "text/html")
}

async fn simple_project(state: &PyPiState, headers: &HeaderMap, name: &str) -> Response {
    let versions = state.registry.meta.list_versions("pypi", name).await.unwrap_or_default();
    if versions.is_empty() {
        return proxy_simple_project(state, name).await;
    }
    if wants_json(headers) {
        return simple_project_json(state, name, &versions).await;
    }
    let base = &state.self_base;
    let mut sb = String::from("<!DOCTYPE html><html><body>\n");
    for v in &versions {
        let Ok(art) = state.registry.meta.get("pypi", name, v).await else {
            continue;
        };
        for b in &art.blobs {
            if b.name.is_empty() {
                continue;
            }
            sb.push_str(&format!(
                "<a href=\"{base}/simple/{}/{}#sha256={}\">{}</a>\n",
                urlencode(name),
                urlencode(&b.name),
                b.hex(),
                b.name
            ));
        }
    }
    sb.push_str("</body></html>");
    text(StatusCode::OK, sb, "text/html")
}

fn simple_json(v: serde_json::Value) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/vnd.pypi.simple.v1+json".to_string())],
        v.to_string(),
    )
        .into_response()
}

async fn simple_project_json(state: &PyPiState, name: &str, versions: &[String]) -> Response {
    let base = &state.self_base;
    let mut files = Vec::new();
    for v in versions {
        let Ok(art) = state.registry.meta.get("pypi", name, v).await else {
            continue;
        };
        for b in &art.blobs {
            if b.name.is_empty() {
                continue;
            }
            let mut entry = serde_json::json!({
                "filename": b.name,
                "url": format!("{base}/simple/{}/{}", urlencode(name), urlencode(&b.name)),
                "hashes": {"sha256": b.hex()},
            });
            if b.size > 0 {
                entry["size"] = b.size.into();
            }
            let up = upload_time_of(&art).unwrap_or_else(|| "2024-01-01T00:00:00Z".into());
            entry["upload-time"] = up.into();
            files.push(entry);
        }
    }
    simple_json(serde_json::json!({
        "meta": {"api-version": "1.4"},
        "name": name,
        "versions": versions,
        "files": files,
    }))
}

fn upload_time_of(art: &Artifact) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(&art.proprietary).ok()?;
    v.get("upload_time")?.as_str().map(|s| s.to_string())
}

async fn proxy_simple_project(state: &PyPiState, name: &str) -> Response {
    let Some(remote) = state.registry.remote("pypi", None) else {
        return error(StatusCode::NOT_FOUND, "not found");
    };
    let Ok(body) =
        remote.get_cached(&index_cache(), &format!("/simple/{}/", urlencode(name))).await
    else {
        return error(StatusCode::NOT_FOUND, "not found");
    };
    let rewritten = rewrite_links(&body, &state.self_base, name);
    text(StatusCode::OK, rewritten, "text/html")
}

/// Rewrite hrefs in a /simple/ index page to self, keeping only the filename
/// part of each anchor. `project` is the PEP 503 normalized project name
/// owning the page.
fn rewrite_links(html: &str, self_base: &str, project: &str) -> String {
    let mut sb = String::new();
    let mut rest = html;
    loop {
        let Some(start) = rest.find("<a ") else {
            sb.push_str(rest);
            break;
        };
        sb.push_str(&rest[..start]);
        let after = &rest[start..];
        let Some(gt) = after.find('>') else {
            sb.push_str(after);
            break;
        };
        let Some(close) = after.find("</a>") else {
            sb.push_str(after);
            break;
        };
        let mut fname = after[gt + 1..close].trim().to_string();
        if let Some(h) = fname.find('#') {
            fname.truncate(h);
        }
        let href = format!("{}/simple/{}/{}", self_base, urlencode(project), urlencode(&fname));
        sb.push_str(&format!("<a href=\"{href}\">{fname}</a>"));
        rest = &after[close + "</a>".len()..];
    }
    sb
}

fn index_cache() -> pkglab_common::cache::MemCache {
    // The shared cache lives on the Upstreams layer in spirit; a fresh cache
    // per call would defeat TTL. Keep a process-global like the reference.
    static CACHE: std::sync::OnceLock<pkglab_common::cache::MemCache> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| pkglab_common::cache::MemCache::new(std::time::Duration::from_secs(3600)))
        .clone()
}

async fn simple_file(state: &PyPiState, project: &str, filename: &str) -> Response {
    // Locate the version containing this file.
    let versions = state.registry.meta.list_versions("pypi", project).await.unwrap_or_default();
    for v in versions {
        let Ok(art) = state.registry.meta.get("pypi", project, &v).await else {
            continue;
        };
        for b in &art.blobs {
            if b.name == filename {
                if let Ok(Some(mut r)) = state.registry.blobs.open(&b.digest).await {
                    let mut data = Vec::new();
                    if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                        return blob_response(data, filename);
                    }
                }
            }
        }
    }

    // Pull-through: resolve the file href from the upstream /simple/ page.
    let Some(up_base) = state.registry.upstream("pypi") else {
        return error(StatusCode::NOT_FOUND, "not found");
    };
    let page_path = format!("/simple/{}/", urlencode(project));
    let remote = state.registry.remote_at("pypi", &up_base);
    let Ok(html_bytes) = remote.get_bytes(&page_path).await else {
        return error(StatusCode::NOT_FOUND, "not found");
    };
    let page_url = format!("{up_base}{page_path}");
    let Some(href) = resolve_file_href(&String::from_utf8_lossy(&html_bytes), filename, &page_url)
    else {
        return error(StatusCode::NOT_FOUND, "not found");
    };
    match state.registry.fetch_absolute(&href).await {
        Ok(fetched) => {
            let version = version_from_filename(filename, project);
            store_version_src(state, project, &version, filename, fetched.data.clone(), "pull")
                .await;
            blob_response(fetched.data, filename)
        }
        Err(_) => error(StatusCode::NOT_FOUND, "not found"),
    }
}

/// Find the href for `filename` in a /simple/ page and resolve it against
/// `base_url`. Fragments (#sha256=...) are dropped.
fn resolve_file_href(html: &str, filename: &str, base_url: &str) -> Option<String> {
    let mut rest = html;
    loop {
        let start = rest.find("<a ")?;
        let after = &rest[start..];
        let gt = after.find('>')?;
        let close = after.find("</a>")?;
        let mut fname = after[gt + 1..close].trim().to_string();
        if let Some(h) = fname.find('#') {
            fname.truncate(h);
        }
        if fname == filename {
            let tag = &after[..gt];
            if let Some(i) = tag.find("href=\"") {
                let t = &tag[i + "href=\"".len()..];
                if let Some(j) = t.find('"') {
                    let raw = t[..j].trim().to_string();
                    return Some(resolve_against(&raw, base_url));
                }
            }
        }
        rest = &after[close + "</a>".len()..];
    }
}

fn resolve_against(raw: &str, base_url: &str) -> String {
    match url::Url::parse(raw) {
        Ok(u) => drop_fragment(u.as_str()),
        Err(_) => match base_url.parse::<url::Url>() {
            Ok(base) => match base.join(raw) {
                Ok(j) => drop_fragment(j.as_str()),
                Err(_) => raw.to_string(),
            },
            Err(_) => raw.to_string(),
        },
    }
}

fn drop_fragment(s: &str) -> String {
    match s.find('#') {
        Some(i) => s[..i].to_string(),
        None => s.to_string(),
    }
}

fn version_from_filename(filename: &str, project: &str) -> String {
    let base =
        filename.trim_end_matches(".tar.gz").trim_end_matches(".whl").trim_end_matches(".zip");
    match base.strip_prefix(&format!("{project}-")) {
        Some(v) => v.to_string(),
        None => base.to_string(),
    }
}

/// PEP 658: serve the `.metadata` sidecar extracted from the wheel zip, or a
/// minimal synthesized Core Metadata document.
async fn metadata_file(state: &PyPiState, project: &str, filename: &str) -> Response {
    let versions = state.registry.meta.list_versions("pypi", project).await.unwrap_or_default();
    for v in versions {
        let Ok(art) = state.registry.meta.get("pypi", project, &v).await else {
            continue;
        };
        for b in &art.blobs {
            if b.name != filename {
                continue;
            }
            if let Ok(Some(mut r)) = state.registry.blobs.open(&b.digest).await {
                let mut data = Vec::new();
                if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                    let meta = extract_wheel_metadata(&data).unwrap_or_else(|| {
                        format!("Metadata-Version: 2.1\nName: {project}\nVersion: {v}\n")
                    });
                    return text(StatusCode::OK, meta, "application/octet-stream");
                }
            }
        }
    }
    error(StatusCode::NOT_FOUND, "not found")
}

/// METADATA file content from a wheel (.whl) zip.
fn extract_wheel_metadata(data: &[u8]) -> Option<String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(data)).ok()?;
    for i in 0..archive.len() {
        let Ok(f) = archive.by_index(i) else { continue };
        if f.name().ends_with(".dist-info/METADATA") {
            let mut capped = f.take(1 << 20);
            let mut out = String::new();
            if capped.read_to_string(&mut out).is_ok() {
                return Some(out);
            }
        }
    }
    None
}

async fn upload(State(st): State<Arc<PyPiState>>, headers: HeaderMap, body: Body) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    let mut body = body;
    let raw = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(c) => c.to_bytes().to_vec(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
    };
    let ct = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");

    // PyPI upload is multipart/form-data with name/version/content fields.
    if ct.starts_with("multipart/form-data") {
        let name =
            pkglab_common::multipart::extract_text_field(&raw, ct, "name").unwrap_or_default();
        let version =
            pkglab_common::multipart::extract_text_field(&raw, ct, "version").unwrap_or_default();
        let (fname, data) =
            pkglab_common::multipart::extract_first_file(&raw, ct).unwrap_or((None, Vec::new()));
        if name.is_empty() {
            return error(StatusCode::BAD_REQUEST, "missing name");
        }
        let filename = fname.unwrap_or_else(|| "unknown.bin".into());
        store_version(&st, &name, &version, &filename, data).await;
        return (
            StatusCode::CREATED,
            [(header::CONTENT_TYPE, "application/json".to_string())],
            serde_json::json!({"ok": true}).to_string(),
        )
            .into_response();
    }

    // Fallback: accept a JSON document {name, version, filename, content}
    // carried as the raw body.
    #[derive(serde::Deserialize, Default)]
    struct Doc {
        #[serde(default)]
        name: String,
        #[serde(default)]
        version: String,
        #[serde(default)]
        filename: String,
    }
    let Ok(doc) = serde_json::from_slice::<Doc>(&raw) else {
        return error(StatusCode::BAD_REQUEST, "missing name");
    };
    if doc.name.is_empty() {
        return error(StatusCode::BAD_REQUEST, "missing name");
    }
    store_version(&st, &doc.name, &doc.version, &doc.filename, raw).await;
    (
        StatusCode::CREATED,
        [(header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({"ok": true}).to_string(),
    )
        .into_response()
}

async fn store_version(
    state: &PyPiState,
    name: &str,
    version: &str,
    filename: &str,
    data: Vec<u8>,
) {
    store_version_src(state, name, version, filename, data, "push").await
}

/// Warehouse-legacy mutation API: delete a single release, a whole project,
/// or specific files. Mirrors the endpoints `twine`/Warehouse historically
/// exposed so self-hosted mirrors can unpublish/yank.
async fn remove_version(state: &Arc<PyPiState>, project: &str, version: &str) {
    if let Ok(art) = state.registry.meta.get("pypi", project, version).await {
        for b in &art.blobs {
            let _ = state.registry.blobs.delete(&b.digest).await;
        }
    }
    let _ = state.registry.meta.delete("pypi", project, version).await;
}

async fn delete_release(
    State(st): State<Arc<PyPiState>>,
    axum::extract::Path((name, version)): axum::extract::Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    remove_version(&st, &name, &version).await;
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({"ok": true}).to_string(),
    )
        .into_response()
}

async fn delete_files(
    State(st): State<Arc<PyPiState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    #[derive(serde::Deserialize, Default)]
    struct Req {
        #[serde(default)]
        version: String,
        #[serde(default)]
        files: Vec<String>,
    }
    let mut body = body;
    let raw = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(c) => c.to_bytes().to_vec(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
    };
    let req = match serde_json::from_slice::<Req>(&raw) {
        Ok(r) => r,
        Err(_) => Req { version: String::new(), files: Vec::new() },
    };
    let name = normalize_name(&name);
    if req.files.is_empty() {
        // No file list: drop the whole release (or project when no version).
        if req.version.is_empty() {
            if let Ok(versions) = st.registry.meta.list_versions("pypi", &name).await {
                for v in versions {
                    remove_version(&st, &name, &v).await;
                }
            }
        } else {
            remove_version(&st, &name, &req.version).await;
        }
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json".to_string())],
            serde_json::json!({"ok": true}).to_string(),
        )
            .into_response();
    }
    // Remove the named files from the version's blob list.
    if !req.version.is_empty() {
        if let Ok(art) = st.registry.meta.get("pypi", &name, &req.version).await {
            let mut art = art;
            let keep: Vec<Descriptor> =
                art.blobs.iter().filter(|b| !req.files.contains(&b.name)).cloned().collect();
            let removed: Vec<Descriptor> =
                art.blobs.iter().filter(|b| req.files.contains(&b.name)).cloned().collect();
            art.blobs = keep;
            if art.blobs.is_empty() {
                let _ = st.registry.meta.delete("pypi", &name, &req.version).await;
            } else {
                let _ = st.registry.meta.put(art).await;
            }
            for b in &removed {
                let _ = st.registry.blobs.delete(&b.digest).await;
            }
        }
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({"ok": true}).to_string(),
    )
        .into_response()
}

async fn delete_project(
    State(st): State<Arc<PyPiState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    let name = normalize_name(&name);
    if let Ok(versions) = st.registry.meta.list_versions("pypi", &name).await {
        for v in versions {
            remove_version(&st, &name, &v).await;
        }
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({"ok": true}).to_string(),
    )
        .into_response()
}

pub async fn store_version_src(
    state: &PyPiState,
    name: &str,
    version: &str,
    filename: &str,
    data: Vec<u8>,
    source: &str,
) {
    let name = normalize_name(name);
    let version = if version.is_empty() { "0.1.0" } else { version };
    let mut art = Artifact {
        format: "pypi".into(),
        repository: name.clone(),
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
    // Record upload timestamp for PEP 700 JSON output.
    art.proprietary =
        serde_json::json!({"upload_time": "2024-01-01T00:00:00.000000Z"}).to_string().into_bytes();
    let _ = state.registry.meta.put(art).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pep503() {
        assert_eq!(normalize_name("Foo.Bar_baz--qux"), "foo-bar-baz-qux");
        assert_eq!(normalize_name("requests"), "requests");
        assert_eq!(normalize_name("A_B-C.D"), "a-b-c-d");
        assert_eq!(normalize_name("-lead-"), "lead");
    }

    #[test]
    fn version_parse() {
        assert_eq!(version_from_filename("requests-1.2.3.tar.gz", "requests"), "1.2.3");
        assert_eq!(version_from_filename("x-2.0.whl", "x"), "2.0");
        assert_eq!(version_from_filename("other-name.zip", "x"), "other-name");
    }

    #[test]
    fn link_rewrite() {
        let html = "<html><body>\n<a href=\"https://files.pythonhosted.org/packages/x/foo-1.0.tar.gz#sha256=abc\">foo-1.0.tar.gz</a>\n</body></html>";
        let out = rewrite_links(html, "http://reg:1", "foo");
        assert!(
            out.contains("<a href=\"http://reg:1/simple/foo/foo-1.0.tar.gz\">foo-1.0.tar.gz</a>")
        );
        assert!(!out.contains("pythonhosted"));
    }

    #[test]
    fn href_resolution() {
        let html = "<a href=\"../../packages/foo.tar.gz#sha256=x\">foo.tar.gz</a>";
        let u =
            resolve_file_href(html, "foo.tar.gz", "https://mirror.example/simple/foo/").unwrap();
        assert!(u.starts_with("https://mirror.example/packages/foo.tar.gz"), "{u}");
        assert!(!u.contains('#'));
    }
}

#[cfg(test)]
mod tests_extras {
    use super::*;
    use std::io::Write;

    #[test]
    fn wheel_metadata_extraction() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("pkg-1.0.0.dist-info/METADATA", zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(b"Metadata-Version: 2.1\nName: pkg\nVersion: 1.0.0\n").unwrap();
            z.finish().unwrap();
        }
        let meta = extract_wheel_metadata(&buf.into_inner()).unwrap();
        assert!(meta.contains("Name: pkg"));
        assert!(meta.contains("Version: 1.0.0"));
    }
}
