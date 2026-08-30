//! RubyGems protocol (compact index + .gem): `/versions` compact index
//! merged with upstream, `/info/{name}`, `specs.4.8.gz` (Ruby Marshal
//! encoder), `quick/Marshal.4.8/*.gemspec.rz` (zlib), Marshal `dependencies`
//! API, gem push (metadata.gz YAML parse), yank, download pull-through,
//! names/versions/gems/owners/search/api_key endpoints.
use pkglab_common::httphelpers::urlencode;
use pkglab_common::httphelpers::{error, json, text};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use flate2::read::GzDecoder;
use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression;
use pkglab_common::{Artifact, Descriptor};
use std::io::{Cursor, Read, Write as _};
use std::sync::Arc;
use tar::Archive;

pub struct RubyGemsState {
    pub registry: Arc<pkglab_common::Registry>,
    pub auth: Option<Arc<dyn pkglab_common::Auth>>,
}

fn octet(status: StatusCode, data: Vec<u8>) -> Response {
    (status, [(header::CONTENT_TYPE, "application/octet-stream".to_string())], data).into_response()
}

fn latest_of(versions: &[String]) -> String {
    pkglab_common::versioncmp::highest(versions.iter())
        .map(str::to_string)
        .or_else(|| versions.last().cloned())
        .unwrap_or_default()
}

fn valid_gem_version(v: &str) -> bool {
    if v.is_empty() || v.starts_with('v') {
        return false;
    }
    v.split('.')
        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

async fn authorize_write(state: &RubyGemsState, headers: &HeaderMap) -> Result<(), Response> {
    pkglab_common::httphelpers::authorize_write(&state.auth, headers).await
}

pub fn router(state: Arc<RubyGemsState>) -> axum::Router {
    axum::Router::new()
        .route("/specs.4.8.gz", get(specs))
        .route("/latest_specs.4.8.gz", get(specs))
        .route("/prerelease_specs.4.8.gz", get(specs))
        .route("/names", get(names))
        .route("/versions", get(compact_versions))
        .route("/info/{name}", get(compact_info))
        .route("/api/v1/versions/{name}", get(versions_api))
        .route("/api/v1/versions/{name}/latest", get(latest))
        .route("/api/v1/versions/{name}/latest.json", get(latest))
        .route("/api/v1/gems/{name}", get(gems_info))
        .route("/api/v1/gems", get(gems_list))
        .route("/api/v1/gems/{name}/owners", get(owners))
        .route("/api/v1/gems/{name}/owners.json", get(owners))
        .route("/api/v1/dependencies", get(dependencies).post(dependencies))
        .route("/api/v1/search", get(search))
        .route("/api/v1/search.json", get(search))
        .route("/api/v1/search.yaml", get(search))
        .route("/api/v1/api_key", get(api_key).post(api_key))
        .route("/api/v1/gems", post(upload))
        .route("/api/v1/gems/yank", axum::routing::any(yank_no_path))
        .route("/api/v1/gems/{name}/yank", delete(yank))
        .route("/api/v2/rubygems/{name}/versions/{*version}", get(version_v2))
        .route("/gems/{*filename}", get(download))
        .route("/quick/{*rest}", get(quick_marshal))
        .with_state(state)
}

use axum::routing::delete;

async fn names(State(st): State<Arc<RubyGemsState>>) -> Response {
    let repos = st.registry.meta.list_repositories_by_format("rubygems").await.unwrap_or_default();
    text(StatusCode::OK, repos.join("\n"), "text/plain")
}

async fn versions_api(State(st): State<Arc<RubyGemsState>>, Path(name): Path<String>) -> Response {
    let name = name.trim_end_matches(".json");
    let versions = st.registry.meta.list_versions("rubygems", name).await.unwrap_or_default();
    if versions.is_empty() {
        // Pull-through the API versions JSON from rubygems.org.
        if let Some(remote) = st.registry.remote("rubygems", None) {
            if let Ok(body) =
                remote.get_bytes(&format!("/api/v1/versions/{}.json", urlencode(name))).await
            {
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json".to_string())],
                    String::from_utf8_lossy(&body).to_string(),
                )
                    .into_response();
            }
        }
    }
    let out: Vec<serde_json::Value> = versions
        .iter()
        .map(|v| {
            serde_json::json!({"number": v, "platform": "ruby", "created_at": "2024-01-01T00:00:00Z"})
        })
        .collect();
    json(StatusCode::OK, serde_json::Value::Array(out))
}

async fn latest(State(st): State<Arc<RubyGemsState>>, Path(name): Path<String>) -> Response {
    let name = name.trim_end_matches(".json");
    let versions = st.registry.meta.list_versions("rubygems", name).await.unwrap_or_default();
    json(StatusCode::OK, serde_json::json!({"version": latest_of(&versions)}))
}

async fn gems_info(State(st): State<Arc<RubyGemsState>>, Path(name): Path<String>) -> Response {
    let name = name.trim_end_matches(".json");
    let versions = st.registry.meta.list_versions("rubygems", name).await.unwrap_or_default();
    if versions.is_empty() {
        return error(StatusCode::NOT_FOUND, "not found");
    }
    let lines: Vec<String> = versions.iter().map(|v| format!("{v} |sha256:|")).collect();
    text(StatusCode::OK, format!("---\n{}", lines.join("\n")), "text/plain")
}

async fn gems_list(State(st): State<Arc<RubyGemsState>>) -> Response {
    let repos = st.registry.meta.list_repositories_by_format("rubygems").await.unwrap_or_default();
    let mut out = Vec::new();
    for name in repos {
        let versions = st.registry.meta.list_versions("rubygems", &name).await.unwrap_or_default();
        out.push(serde_json::json!({"name": name, "version": latest_of(&versions)}));
    }
    json(StatusCode::OK, serde_json::Value::Array(out))
}

async fn owners() -> Response {
    json(StatusCode::OK, serde_json::json!([{"id": 1, "handle": "anonymous", "role": "owner"}]))
}

async fn search(
    State(st): State<Arc<RubyGemsState>>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
) -> Response {
    let mut query = String::new();
    if let Some(q) = q {
        for pair in q.split('&') {
            let mut it = pair.splitn(2, '=');
            if it.next() == Some("query") {
                query = it.next().unwrap_or("").to_lowercase();
            }
        }
    }
    let repos = st.registry.meta.list_repositories_by_format("rubygems").await.unwrap_or_default();
    let mut out = Vec::new();
    for name in repos {
        if !query.is_empty() && !name.to_lowercase().contains(&query) {
            continue;
        }
        let versions = st.registry.meta.list_versions("rubygems", &name).await.unwrap_or_default();
        out.push(serde_json::json!({"name": name, "version": latest_of(&versions)}));
    }
    json(StatusCode::OK, serde_json::Value::Array(out))
}

async fn api_key() -> Response {
    json(StatusCode::OK, serde_json::json!({"rubygems_api_key": "test-token", "name": "test"}))
}

async fn version_v2(
    State(st): State<Arc<RubyGemsState>>,
    Path((name, version)): Path<(String, String)>,
) -> Response {
    let _ = st;
    let name = name.trim_end_matches(".json");
    let version = version.trim_end_matches(".json");
    json(
        StatusCode::OK,
        serde_json::json!({
            "name": name, "version": version, "platform": "ruby",
            "number": version, "created_at": "2024-01-01T00:00:00Z"
        }),
    )
}

/// Compact index /versions: local lines merged with the upstream compact
/// index (header stripped) so Bundler resolves un-cached gems.
async fn compact_versions(State(st): State<Arc<RubyGemsState>>) -> Response {
    let repos = st.registry.meta.list_repositories_by_format("rubygems").await.unwrap_or_default();
    let mut sb = String::from("created_at: 2026-01-01T00:00:00Z\n---\n");
    for name in &repos {
        let versions = st.registry.meta.list_versions("rubygems", name).await.unwrap_or_default();
        if versions.is_empty() {
            continue;
        }
        // Third field is the sha256 of the /info/{name} entry; a stable
        // placeholder keeps Bundler moving to /info/{name}.
        sb.push_str(&format!("{name} {} {}\n", versions.join(","), "0".repeat(64)));
    }
    // Merge upstream /versions (large; cached).
    if let Some(remote) = st.registry.remote_sub("rubygems", "index") {
        if let Ok(upstream) = remote.get_cached(&shared_cache(), "/versions").await {
            if let Some(i) = upstream.find("---\n") {
                sb.push_str(&upstream[i + "---\n".len()..]);
            }
        }
    }
    text(StatusCode::OK, sb, "text/plain; version=1")
}

/// Compact index /info/{name}; pull-through on local miss.
async fn compact_info(State(st): State<Arc<RubyGemsState>>, Path(name): Path<String>) -> Response {
    let versions = st.registry.meta.list_versions("rubygems", &name).await.unwrap_or_default();
    if versions.is_empty() {
        if let Some(remote) = st.registry.remote_sub("rubygems", "index") {
            if let Ok(body) =
                remote.get_cached(&shared_cache(), &format!("/info/{}", urlencode(&name))).await
            {
                return text(StatusCode::OK, body, "text/plain");
            }
        }
        return text(StatusCode::OK, "---\n".into(), "text/plain");
    }
    let mut sb = String::from("---\n");
    for v in &versions {
        let mut checksum = String::new();
        if let Ok(art) = st.registry.meta.get("rubygems", &name, v).await {
            if let Some(b) = art.blobs.first() {
                checksum = b.hex().to_string();
            }
        }
        if checksum.is_empty() {
            checksum = "0".repeat(64);
        }
        sb.push_str(&format!("{v} |checksum:{checksum}\n"));
    }
    text(StatusCode::OK, sb, "text/plain")
}

/// specs.4.8.gz: Ruby-Marshalled array of [name, Gem::Version, platform].
async fn specs(State(st): State<Arc<RubyGemsState>>) -> Response {
    let repos = st.registry.meta.list_repositories_by_format("rubygems").await.unwrap_or_default();
    let mut tuples: Vec<(String, String)> = Vec::new();
    for n in &repos {
        let versions = st.registry.meta.list_versions("rubygems", n).await.unwrap_or_default();
        for v in versions {
            if valid_gem_version(&v) {
                tuples.push((n.clone(), v));
            }
        }
    }
    let data = marshal_specs(&tuples);
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    let _ = gz.write_all(&data);
    octet(StatusCode::OK, gz.finish().unwrap_or_default())
}

async fn quick_marshal(State(st): State<Arc<RubyGemsState>>, Path(rest): Path<String>) -> Response {
    let rel = rest.trim_start_matches("Marshal.4.8/");
    let stem = rel.trim_end_matches(".gemspec.rz");
    let (name, version) = name_version_from_stem(stem);
    if st.registry.meta.get("rubygems", &name, &version).await.is_ok() {
        let spec = marshal_specification(&name, &version);
        let mut zlib = ZlibEncoder::new(Vec::new(), Compression::default());
        let _ = zlib.write_all(&spec);
        return octet(StatusCode::OK, zlib.finish().unwrap_or_default());
    }
    // Pull-through: rubygems.org serves the same Marshal 4.8 gemspec.rz.
    if let Some(remote) = st.registry.remote("rubygems", None) {
        if let Ok(data) = remote.get_bytes(&format!("/quick/Marshal.4.8/{rel}")).await {
            return octet(StatusCode::OK, data);
        }
    }
    StatusCode::NOT_FOUND.into_response()
}

async fn dependencies(
    State(st): State<Arc<RubyGemsState>>,
    method: axum::http::Method,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
    body: Body,
) -> Response {
    let mut gems_param = q
        .as_deref()
        .and_then(|q| {
            q.split('&').find_map(|pair| {
                let mut it = pair.splitn(2, '=');
                if it.next() == Some("gems") {
                    it.next().map(|v| v.to_string())
                } else {
                    None
                }
            })
        })
        .unwrap_or_default();
    if gems_param.is_empty() && method == axum::http::Method::POST {
        let mut b = body;
        if let Ok(raw) = http_body_util::BodyExt::collect(&mut b).await {
            let body_str = String::from_utf8_lossy(&raw.to_bytes()).to_string();
            // Parse application/x-www-form-urlencoded "gems=a,b,c".
            for pair in body_str.split('&') {
                let mut it = pair.splitn(2, '=');
                if it.next() == Some("gems") {
                    gems_param = it.next().unwrap_or("").to_string();
                }
            }
        }
    }
    let mut b: Vec<u8> = vec![0x04, 0x08]; // Marshal 4.8
    if gems_param.is_empty() {
        b = marshal_array_len(&b, 0);
        return octet(StatusCode::OK, b);
    }
    let gems: Vec<&str> = gems_param.split(',').collect();
    b = marshal_array_len(&b, gems.len());
    for g in gems {
        if g.is_empty() {
            continue;
        }
        let mut name = g.to_string();
        let mut ver = String::new();
        if let Some(i) = g.rfind('-') {
            let cand = &g[i + 1..];
            if cand.as_bytes().first().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                ver = cand.to_string();
                name = g[..i].to_string();
            }
        }
        if ver.is_empty() {
            let versions =
                st.registry.meta.list_versions("rubygems", &name).await.unwrap_or_default();
            ver = latest_of(&versions);
        }
        // entry = [[name, Gem::Requirement(">= 0"), platform], []]
        b = marshal_array_len(&b, 2);
        b = marshal_array_len(&b, 3);
        b = marshal_istring(&b, &name);
        b = marshal_requirement(&b);
        b = marshal_istring(&b, "ruby");
        b = marshal_array_len(&b, 0);
    }
    octet(StatusCode::OK, b)
}

async fn upload(State(st): State<Arc<RubyGemsState>>, headers: HeaderMap, body: Body) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    let mut body = body;
    let data = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(c) => c.to_bytes().to_vec(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
    };
    let (name, version) = extract_name_version_gem(&data);
    let name = if name.is_empty() { "unknown".to_string() } else { name };
    let version = if version.is_empty() { "0.1.0".to_string() } else { version };
    let filename = format!("{name}-{version}.gem");
    store_version(&st, &name, &version, &filename, data).await;
    (
        StatusCode::CREATED,
        [(header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({"ok": true}).to_string(),
    )
        .into_response()
}

/// DELETE /api/v1/gems/yank (no {name} path segment).
async fn yank_no_path(
    st: State<Arc<RubyGemsState>>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
    headers: HeaderMap,
    body: Body,
) -> Response {
    yank_impl(st.0, String::new(), q, headers, body).await
}

async fn yank(
    State(st): State<Arc<RubyGemsState>>,
    Path(name_path): Path<String>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
    headers: HeaderMap,
    body: Body,
) -> Response {
    yank_impl(st, name_path, q, headers, body).await
}

async fn yank_impl(
    st: Arc<RubyGemsState>,
    name_path: String,
    q: Option<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    // Form params arrive in the query string or the DELETE body.
    let mut params: Vec<(String, String)> = Vec::new();
    if let Some(q) = q {
        for pair in q.split('&') {
            let mut it = pair.splitn(2, '=');
            params.push((it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string()));
        }
    }
    let mut b = body;
    if let Ok(raw) = http_body_util::BodyExt::collect(&mut b).await {
        let body_str = String::from_utf8_lossy(&raw.to_bytes()).to_string();
        for pair in body_str.split('&') {
            let mut it = pair.splitn(2, '=');
            params.push((it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string()));
        }
    }
    let get = |k: &str| {
        params.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone()).unwrap_or_default()
    };
    let name =
        if name_path.is_empty() || name_path == "yank" { get("gem_name") } else { name_path };
    if name.is_empty() {
        // Deleting a nonexistent package: not found rather than an error.
        return error(StatusCode::NOT_FOUND, "missing gem_name");
    }
    let version = get("version");
    if version.is_empty() {
        let versions = st.registry.meta.list_versions("rubygems", &name).await.unwrap_or_default();
        for v in versions {
            remove_version(&st, &name, &v).await;
        }
    } else {
        remove_version(&st, &name, &version).await;
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({"ok": true}).to_string(),
    )
        .into_response()
}

async fn remove_version(st: &RubyGemsState, name: &str, version: &str) {
    if let Ok(art) = st.registry.meta.get("rubygems", name, version).await {
        for b in &art.blobs {
            let _ = st.registry.blobs.delete(&b.digest).await;
        }
    }
    let _ = st.registry.meta.delete("rubygems", name, version).await;
}

async fn download(State(st): State<Arc<RubyGemsState>>, Path(filename): Path<String>) -> Response {
    let filename = filename.rsplit('/').next().unwrap_or(&filename).to_string();
    let stem = filename.trim_end_matches(".gem");
    let (name, version) = name_version_from_stem(stem);
    if let Ok(art) = st.registry.meta.get("rubygems", &name, &version).await {
        for b in &art.blobs {
            if let Ok(Some(mut r)) = st.registry.blobs.open(&b.digest).await {
                let mut data = Vec::new();
                if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                    return octet(StatusCode::OK, data);
                }
            }
        }
    }
    // Pull-through: fetch the .gem from rubygems.org/gems.
    if let Some(remote) = st.registry.remote_sub("rubygems", "gems") {
        if let Ok(data) = remote.get_bytes(&format!("/{filename}")).await {
            store_version_src(&st, &name, &version, &filename, data.clone(), "pull").await;
            return octet(StatusCode::OK, data);
        }
    }
    error(StatusCode::NOT_FOUND, "not found")
}

fn name_version_from_stem(stem: &str) -> (String, String) {
    match stem.rfind('-') {
        Some(i) if i > 0 => (stem[..i].to_string(), stem[i + 1..].to_string()),
        _ => (stem.to_string(), "0.0.0".to_string()),
    }
}

/// Parse name/version from a .gem tar archive's metadata.gz (YAML gemspec).
fn extract_name_version_gem(data: &[u8]) -> (String, String) {
    let mut archive = Archive::new(Cursor::new(data));
    for entry in archive.entries().into_iter().flatten() {
        let Ok(mut entry) = entry else { continue };
        let name = entry.path().map(|p| p.display().to_string()).unwrap_or_default();
        if name != "metadata.gz" {
            continue;
        }
        let mut raw = Vec::new();
        if entry.read_to_end(&mut raw).is_err() {
            continue;
        }
        let mut gz = GzDecoder::new(&raw[..]);
        let mut ym = String::new();
        if gz.read_to_string(&mut ym).is_err() {
            continue;
        }
        let name = yaml_field(&ym, "name");
        let version = yaml_version(&ym);
        return (name, version);
    }
    (String::new(), String::new())
}

fn yaml_field(s: &str, field: &str) -> String {
    // Match `^name: value$` at column 0 (the gemspec YAML nests under
    // "rubygems:", so indented matches are wrong).
    let prefix = format!("{field}:");
    for line in s.lines() {
        if line.starts_with(&prefix) {
            return line[prefix.len()..].trim().to_string();
        }
    }
    // Fallback for flat nested forms (indented once).
    for line in s.lines() {
        if let Some(rest) = line.trim_start().strip_prefix(&prefix) {
            return rest.trim().to_string();
        }
    }
    String::new()
}

fn yaml_version(s: &str) -> String {
    for line in s.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("version:") {
            let v = rest.trim();
            // numeric version only (skip object forms)
            if v.as_bytes().first().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                return v.to_string();
            }
        }
    }
    String::new()
}

async fn store_version(
    st: &RubyGemsState,
    name: &str,
    version: &str,
    filename: &str,
    data: Vec<u8>,
) {
    store_version_src(st, name, version, filename, data, "push").await
}

pub async fn store_version_src(
    st: &RubyGemsState,
    name: &str,
    version: &str,
    filename: &str,
    data: Vec<u8>,
    source: &str,
) {
    let mut art = Artifact {
        format: "rubygems".into(),
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
                    name: filename.to_string(),
                    ..Default::default()
                });
            }
        }
    }
    let _ = st.registry.meta.put(art).await;
}

fn shared_cache() -> pkglab_common::cache::MemCache {
    static CACHE: std::sync::OnceLock<pkglab_common::cache::MemCache> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| pkglab_common::cache::MemCache::new(std::time::Duration::from_secs(3600)))
        .clone()
}

// ---------------------------------------------------------------------------
// Ruby Marshal 4.8 encoding (byte-level port of the reference encoders).
// ---------------------------------------------------------------------------

fn marshal_len(mut b: Vec<u8>, n: usize) -> Vec<u8> {
    if n <= 122 {
        b.push((n + 5) as u8);
        return b;
    }
    let mut mag = Vec::new();
    let mut v = n;
    while v > 0 {
        mag.push((v & 0xff) as u8);
        v >>= 8;
    }
    b.push(mag.len() as u8);
    b.extend_from_slice(&mag);
    b
}

fn marshal_array_len(b: &[u8], n: usize) -> Vec<u8> {
    let mut out = b.to_vec();
    out.push(0x5b);
    marshal_len(out, n)
}

/// IVar string with the :E encoding ivar. The FIRST string emits :E inline;
/// subsequent strings emit a symbol link (index 0) — mirroring Ruby's symbol
/// dedup.
fn marshal_istring_first(b: &[u8], s: &str) -> Vec<u8> {
    let mut out = b.to_vec();
    out.extend_from_slice(&[0x49, 0x22]);
    out = marshal_len(out, s.len());
    out.extend_from_slice(s.as_bytes());
    out.extend_from_slice(&[0x06, 0x3a, 0x06, 0x45, 0x54]); // :E inline
    out
}

fn marshal_istring(b: &[u8], s: &str) -> Vec<u8> {
    let mut out = b.to_vec();
    out.extend_from_slice(&[0x49, 0x22]);
    out = marshal_len(out, s.len());
    out.extend_from_slice(s.as_bytes());
    out.extend_from_slice(&[0x06, 0x3b, 0x00, 0x54]); // :E symbol link 0
    out
}

/// `U` :Gem::Version [ ivar-string ] (user-marshal).
fn marshal_version(b: &[u8], version: &str) -> Vec<u8> {
    let mut out = b.to_vec();
    out.extend_from_slice(&[0x55, 0x3a, 0x11]);
    out.extend_from_slice(b"Gem::Version");
    out.extend_from_slice(&[0x5b, 0x06]); // array of 1
    out = marshal_istring(&out, version);
    out
}

/// Gem::Requirement user-marshal: 'U' :Gem::Requirement [ [">=", Gem::Version("0")] ]
/// (16 chars -> 0x15 = 16+5 small-len). marshal_load validates each
/// requirement is [String, Gem::Version].
fn marshal_requirement(b: &[u8]) -> Vec<u8> {
    let mut out = b.to_vec();
    out.extend_from_slice(&[0x55, 0x3a, 0x15]);
    out.extend_from_slice(b"Gem::Requirement");
    out.extend_from_slice(&[0x5b, 0x06]); // usrmarshal payload: 1-element array [@requirements]
    out.extend_from_slice(&[0x5b, 0x06]); // @requirements: 1-element array
    out.extend_from_slice(&[0x5b, 0x07]); // a single requirement: 2-element array
    out = marshal_istring(&out, ">=");
    out = marshal_version(&out, "0");
    out
}

/// Gem::Specification 'u' user-marshal, byte-exact port of the reference
/// `marshalSpecification`: the _dump payload is Marshal.dump of a 19-element
/// array (spec format 4), wrapped as `u :Gem::Specification <w_long-len>`.
fn marshal_specification(name: &str, version: &str) -> Vec<u8> {
    let payload = marshal_spec_array(name, version);

    let mut b: Vec<u8> = vec![0x04, 0x08];
    b.push(0x75); // 'u' user-marshal
    b.push(0x3a);
    // class symbol uses the len+5 scheme: "Gem::Specification" is 18 chars
    // -> 18+5 = 23 = 0x17.
    b.push(0x17);
    b.extend_from_slice(b"Gem::Specification");
    // 'u' length uses Ruby's w_long format: byte count then LE magnitude.
    // (0..255 -> 0x01 + 1 byte; 256..65535 -> 0x02 + 2 bytes; ...)
    let mut mag = Vec::new();
    let mut v = payload.len();
    if v == 0 {
        mag.push(0u8);
    }
    while v > 0 {
        mag.push((v & 0xff) as u8);
        v >>= 8;
    }
    b.push(mag.len() as u8);
    b.extend_from_slice(&mag);
    b.extend_from_slice(&payload);
    b
}

/// 19-element spec array (element layout per specification.rb).
fn marshal_spec_array(name: &str, version: &str) -> Vec<u8> {
    let mut b: Vec<u8> = vec![0x04, 0x08];
    b = marshal_array_len(&b, 19);

    // The FIRST string must emit the :E encoding ivar inline; subsequent
    // strings use a symbol link (index 0). Mirrors Ruby's Marshal symbol
    // dedup (and the reference appendMarshalIStringBytes vs ...IStrLink).
    b = marshal_istring_first(&b, "4.0.6"); // 0 rubygems_version
    b = marshal_fixnum(b.clone(), 4); // 1 specification_version
    b = marshal_istring(&b, name); // 2 name
    b = marshal_version(&b, version); // 3 version
    b = marshal_time(&b); // 4 date
    b = marshal_istring(&b, ""); // 5 summary
    b = marshal_requirement(&b); // 6 required_ruby_version
    b = marshal_requirement(&b); // 7 required_rubygems_version
    b.push(0x30); // 8 original_platform ("ruby" symbol ref)
    b = marshal_array_len(&b, 0); // 9 dependencies []
    b.push(0x30); // 10 rubyforge_project (symbol ref)
    b.push(0x30); // 11 email (symbol ref)
    b = marshal_authors(&b); // 12 authors
    b.push(0x30); // 13 description
    b.push(0x30); // 14 homepage
    b.push(0x54); // 15 has_rdoc true
    b = marshal_istring(&b, "ruby"); // 16 new_platform
    b = marshal_licenses(&b); // 17 licenses
    b.extend_from_slice(&[0x7b, 0x00]); // 18 metadata: empty hash
    b
}

fn marshal_fixnum(mut b: Vec<u8>, n: i64) -> Vec<u8> {
    // Ruby Marshal fixnum: 0x69 marker, then small-form (n+5) for 0..122 or
    // length-prefixed LE magnitude.
    b.push(0x69);
    if (0..=122).contains(&n) {
        b.push((n + 5) as u8);
        return b;
    }
    let mut mag = Vec::new();
    let mut v = n;
    while v > 0 {
        mag.push((v & 0xff) as u8);
        v >>= 8;
    }
    b.push(mag.len() as u8);
    b.extend_from_slice(&mag);
    b
}

/// Ruby Marshal for a UTC epoch Time ('I' 'u' :Time payload ivar :zone UTC).
fn marshal_time(b: &[u8]) -> Vec<u8> {
    let mut out = b.to_vec();
    out.extend_from_slice(&[0x49, 0x75, 0x3a, 0x09]);
    out.extend_from_slice(b"Time");
    out.extend_from_slice(&[0x0d, 0x40, 0x00, 0x14, 0xc0, 0x00, 0x00, 0x00, 0x00]);
    out.extend_from_slice(&[0x06, 0x3a, 0x09]);
    out.extend_from_slice(b"zone");
    marshal_istring(&out, "UTC")
}

fn marshal_authors(b: &[u8]) -> Vec<u8> {
    let mut out = b.to_vec();
    out.extend_from_slice(&[0x5b, 0x06]);
    marshal_istring(&out, "")
}

fn marshal_licenses(b: &[u8]) -> Vec<u8> {
    let mut out = b.to_vec();
    out.extend_from_slice(&[0x5b, 0x06]);
    marshal_istring(&out, "MIT")
}

fn marshal_specs(tuples: &[(String, String)]) -> Vec<u8> {
    let mut b: Vec<u8> = vec![0x04, 0x08];
    b = marshal_array_len(&b, tuples.len());
    for (i, (name, version)) in tuples.iter().enumerate() {
        b = marshal_array_len(&b, 3);
        b = if i == 0 { marshal_istring_first(&b, name) } else { marshal_istring(&b, name) };
        b = marshal_version(&b, version);
        b = if i == 0 { marshal_istring_first(&b, "ruby") } else { marshal_istring(&b, "ruby") };
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems() {
        assert_eq!(name_version_from_stem("rake-13.0.6"), ("rake".into(), "13.0.6".into()));
        assert_eq!(name_version_from_stem("lonely"), ("lonely".into(), "0.0.0".into()));
    }

    #[test]
    fn gem_versions() {
        assert!(valid_gem_version("1.2.3"));
        assert!(valid_gem_version("2.0.0.pre"));
        assert!(!valid_gem_version("v1.0.0"));
        assert!(!valid_gem_version(""));
    }

    #[test]
    fn marshal_header() {
        let specs = marshal_specs(&[("a".into(), "1.0".into())]);
        assert_eq!(&specs[..2], &[0x04, 0x08]);
        assert_eq!(specs[2], 0x5b); // array
        assert_eq!(specs[3], 6); // len 1 => 1+5
    }

    #[test]
    fn marshal_len_golden() {
        // Short form: 0..122 -> n+5.
        assert_eq!(marshal_len(vec![], 0), vec![5]);
        assert_eq!(marshal_len(vec![], 122), vec![127]);
        // Long form: 123 -> 1 magnitude byte 0x7b.
        assert_eq!(marshal_len(vec![], 123), vec![1, 0x7b]);
        // 256 -> 2 magnitude bytes, little-endian (0x00, 0x01).
        assert_eq!(marshal_len(vec![], 256), vec![2, 0x00, 0x01]);
    }

    #[test]
    fn marshal_version_golden() {
        let out = marshal_version(&[], "");
        // 'U' :Gem::Version [ ivar-string ] — assert the stable prefix and the
        // user-marshal wrapper bytes exactly.
        // bytes: 'U' (x55), ':' (0x3a), sym-len 0x11 (17 => "Gem::Version"=12),
        // "Gem::Version", then '[' (0x5b) + small-len 6 (1 element).
        assert_eq!(out[0], b'U');
        assert_eq!(out[1], b':');
        assert_eq!(out[2], 0x11);
        assert_eq!(&out[3..15], b"Gem::Version");
        assert_eq!(out[15], 0x5b); // array
        assert_eq!(out[16], 0x06); // len 1
    }

    #[test]
    fn marshal_spec_roundtrip_bytes() {
        // The full specification is deterministically self-consistent: the
        // 19-element spec array always begins with the marshal header and an
        // array tag, then len-19.
        let b = marshal_spec_array("demo", "1.0.0");
        assert_eq!(&b[..2], &[0x04, 0x08]);
        assert_eq!(b[2], 0x5b); // array
        assert_eq!(b[3], 24); // 19 + 5
    }

    #[test]
    fn yaml_parse() {
        let flat = "--- \nname: rake\nversion: 13.0.6\n";
        assert_eq!(yaml_field(flat, "name"), "rake");
        assert_eq!(yaml_version(flat), "13.0.6");
        let nested = "--- \nrubygems: \n  name: rake\n  version: 13.0.6\n";
        assert_eq!(yaml_field(nested, "name"), "rake");
        assert_eq!(yaml_version(nested), "13.0.6");
    }

    #[test]
    fn marshal_specification_wraps_user_marshal() {
        // 'u' :Gem::Specification <len> <payload> — the wrapper must be a
        // user-marshal object referencing Gem::Specification.
        let b = marshal_specification("demo", "1.0.0");
        assert_eq!(&b[..2], &[0x04, 0x08]);
        assert_eq!(b[2], 0x75); // 'u'
        assert_eq!(b[3], 0x3a); // ':'
        assert_eq!(b[4], 0x17); // 18+5 = 23
        assert_eq!(&b[5..23], b"Gem::Specification");
        // w_long: 1-byte magnitude count, then little-endian payload length.
        let nlen = b[23] as usize;
        assert!((1..=4).contains(&nlen), "payload length byte count {nlen}");
        let mut plen = 0usize;
        for (k, byte) in b[24..24 + nlen].iter().enumerate() {
            plen |= (*byte as usize) << (8 * k);
        }
        assert_eq!(b.len(), 24 + nlen + plen);
    }
}
