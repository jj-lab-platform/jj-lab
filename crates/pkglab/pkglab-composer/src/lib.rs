//! Composer (Packagist) protocol: `packages.json` metadata map, `p2/{vendor}/{package}`
//! metadata, dist download, `list.json` (wildcard filter), `search.json`,
//! upload with autoload extraction, notify-batch.
use pkglab_common::httphelpers::urlencode;
use pkglab_common::httphelpers::{blob_response, error, json as json_ok};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, post, put};
use pkglab_common::{Artifact, Descriptor};
use std::io::{Cursor, Read};
use std::sync::Arc;

pub struct ComposerState {
    pub registry: Arc<pkglab_common::Registry>,
    pub auth: Option<Arc<dyn pkglab_common::Auth>>,
    pub self_base: String,
}

async fn authorize_write(state: &ComposerState, headers: &HeaderMap) -> Result<(), Response> {
    pkglab_common::httphelpers::authorize_write(&state.auth, headers).await
}

pub fn router(state: Arc<ComposerState>) -> axum::Router {
    axum::Router::new()
        .route("/packages.json", axum::routing::get(packages_json))
        .route("/list.json", axum::routing::get(list_json))
        .route("/search.json", axum::routing::get(search))
        .route("/p2/{vendor}/{package}", axum::routing::get(p2))
        .route("/p/{vendor}/{package}", axum::routing::get(provider))
        .route("/providers/{vendor}/{package}", axum::routing::get(providers_api))
        .route("/dist/{*rest}", axum::routing::get(dist))
        .route("/downloads/", post(notify_batch))
        .route("/downloads", post(notify_batch))
        .route("/api/packages", put(upload).post(upload))
        .route("/api/packages/{vendor}/{package}", delete(delete_package))
        .with_state(state)
}

async fn packages_json(State(st): State<Arc<ComposerState>>) -> Response {
    let repos = st.registry.meta.list_repositories_by_format("composer").await.unwrap_or_default();
    let base = &st.self_base;
    let mut providers = serde_json::Map::new();
    for name in &repos {
        providers.insert(name.clone(), serde_json::json!({"sha256": null}));
    }
    json_ok(
        StatusCode::OK,
        serde_json::json!({
            "packages": [],
            "metadata-url": format!("{base}/p2/%package%.json"),
            "available-packages": repos,
            "providers-url": format!("{base}/p/%package%$%hash%.json"),
            "providers": providers,
            "providers-api": format!("{base}/providers/%package%.json"),
            "list": format!("{base}/list.json"),
            "search": format!("{base}/search.json?q=%query%&type=%type%"),
            "provider-includes": {},
            "notify-batch": format!("{base}/downloads/"),
        }),
    )
}

async fn list_json(
    State(st): State<Arc<ComposerState>>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
) -> Response {
    let repos = st.registry.meta.list_repositories_by_format("composer").await.unwrap_or_default();
    let filter = q
        .as_deref()
        .and_then(|q| {
            q.split('&').find_map(|pair| {
                let mut it = pair.splitn(2, '=');
                if it.next() == Some("filter") {
                    it.next().map(|v| v.replace("%2A", "*"))
                } else {
                    None
                }
            })
        })
        .unwrap_or_default();
    let names: Vec<String> = if filter.is_empty() {
        repos
    } else {
        repos.into_iter().filter(|n| pkglab_common::globutil::glob_match(n, &filter)).collect()
    };
    json_ok(StatusCode::OK, serde_json::json!({"packageNames": names}))
}

async fn search(
    State(st): State<Arc<ComposerState>>,
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
    let repos = st.registry.meta.list_repositories_by_format("composer").await.unwrap_or_default();
    let results: Vec<serde_json::Value> = repos
        .into_iter()
        .filter(|n| query.is_empty() || n.to_lowercase().contains(&query))
        .map(|n| serde_json::json!({"name": n, "description": "", "url": "", "downloads": 0}))
        .collect();
    let total = results.len();
    json_ok(StatusCode::OK, serde_json::json!({"results": results, "total": total}))
}

async fn p2(
    State(st): State<Arc<ComposerState>>,
    axum::extract::Path((vendor, package)): axum::extract::Path<(String, String)>,
) -> Response {
    let pkg = package.trim_end_matches(".json").trim_end_matches("~dev");
    let full = format!("{vendor}/{pkg}");
    let versions = st.registry.meta.list_versions("composer", &full).await.unwrap_or_default();
    if versions.is_empty() {
        // Pull-through p2 metadata.
        if let Some(remote) = st.registry.remote("composer", None) {
            if let Ok(body) =
                remote.get_cached(&shared_cache(), &format!("/p2/{}.json", urlencode(&full))).await
            {
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json".to_string())],
                    body,
                )
                    .into_response();
            }
        }
        return error(StatusCode::NOT_FOUND, "not found");
    }
    let mut vs = Vec::new();
    for v in &versions {
        vs.push(version_entry(&st, &full, v).await);
    }
    json_ok(StatusCode::OK, serde_json::json!({"packages": {full.clone(): vs}}))
}

async fn version_entry(st: &ComposerState, name: &str, version: &str) -> serde_json::Value {
    let base = &st.self_base;
    let mut entry = serde_json::json!({
        "name": name,
        "version": version,
        "version_normalized": normalize_version(version),
        "type": "library",
        "uid": version,
        "dist": {
            "type": "zip",
            "url": format!("{base}/dist/{name}/{}/{}.zip", urlencode(version), version),
            "shasum": "",
        },
        "require": {"php": ">=7.4"},
    });
    // Carry the package's autoload (extracted at upload time) into the p2
    // metadata so `composer install` wires up PSR-4 autoloading.
    if let Ok(art) = st.registry.meta.get("composer", name, version).await {
        if let Ok(m) = serde_json::from_slice::<serde_json::Value>(&art.proprietary) {
            if let Some(al) = m.get("autoload") {
                entry["autoload"] = al.clone();
            }
        }
    }
    entry
}

/// Composer's 4-part normalized version: "1.0.0" -> "1.0.0.0".
fn normalize_version(v: &str) -> String {
    let mut parts: Vec<&str> = v.split('.').collect();
    while parts.len() < 4 {
        parts.push("0");
    }
    parts.join(".")
}

async fn provider(
    State(st): State<Arc<ComposerState>>,
    axum::extract::Path((vendor, package)): axum::extract::Path<(String, String)>,
) -> Response {
    let pkg = package.trim_end_matches(".json");
    let full = format!("{vendor}/{pkg}");
    let versions = st.registry.meta.list_versions("composer", &full).await.unwrap_or_default();
    if versions.is_empty() {
        return error(StatusCode::NOT_FOUND, "not found");
    }
    let mut vs = Vec::new();
    for v in &versions {
        vs.push(version_entry(&st, &full, v).await);
    }
    json_ok(StatusCode::OK, serde_json::json!({"packages": {full: vs}}))
}

/// providers-api: per-package provider document (empty providers map like
/// the reference implementation).
async fn providers_api(
    State(_st): State<Arc<ComposerState>>,
    axum::extract::Path((vendor, package)): axum::extract::Path<(String, String)>,
) -> Response {
    let _ = (vendor, package);
    json_ok(StatusCode::OK, serde_json::json!({"providers": {}}))
}

async fn dist(
    State(st): State<Arc<ComposerState>>,
    axum::extract::Path(rest): axum::extract::Path<String>,
) -> Response {
    // /dist/{vendor}/{package}/{version}/{reference}
    let rel = rest.trim_matches('/');
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.len() < 3 {
        return error(StatusCode::NOT_FOUND, "not found");
    }
    let (version, _reference) = (parts[parts.len() - 2], parts[parts.len() - 1]);
    let full = parts[..parts.len() - 2].join("/");
    let filename = format!("{}-{version}.zip", full.rsplit('/').next().unwrap_or(&full));
    if let Ok(art) = st.registry.meta.get("composer", &full, version).await {
        for b in &art.blobs {
            if let Ok(Some(mut r)) = st.registry.blobs.open(&b.digest).await {
                let mut data = Vec::new();
                if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                    return blob_response(data, &filename);
                }
            }
        }
    }
    // Pull-through dist.
    match st.registry.fetch("composer", "", &format!("/dist/{full}/{version}/{_reference}")).await {
        Ok(fetched) => {
            store_version_src(&st, &full, version, fetched.data.clone(), "pull").await;
            blob_response(fetched.data, &filename)
        }
        Err(_) => error(StatusCode::NOT_FOUND, "not found"),
    }
}

async fn notify_batch() -> Response {
    StatusCode::OK.into_response()
}

async fn upload(State(st): State<Arc<ComposerState>>, headers: HeaderMap, body: Body) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    let mut body = body;
    let data = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(c) => c.to_bytes().to_vec(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
    };
    // Accept name/version from the JSON document or from the zip's
    // composer.json; defaults mirror the reference.
    let mut name = "vendor/pkg".to_string();
    let mut version = "0.1.0".to_string();
    if let Some((n, v)) = composer_json_name_version(&data) {
        if !n.is_empty() {
            name = n;
        }
        if !v.is_empty() {
            version = v;
        }
    }
    store_version(&st, &name, &version, data).await;
    json_ok(StatusCode::CREATED, serde_json::json!({"ok": true}))
}

async fn store_version(st: &ComposerState, name: &str, version: &str, data: Vec<u8>) {
    store_version_src(st, name, version, data, "push").await
}

async fn delete_package(
    State(st): State<Arc<ComposerState>>,
    axum::extract::Path((vendor, package)): axum::extract::Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    let full = format!("{vendor}/{package}");
    if let Ok(versions) = st.registry.meta.list_versions("composer", &full).await {
        for v in versions {
            if let Ok(art) = st.registry.meta.get("composer", &full, &v).await {
                for b in &art.blobs {
                    let _ = st.registry.blobs.delete(&b.digest).await;
                }
            }
            let _ = st.registry.meta.delete("composer", &full, &v).await;
        }
    }
    json_ok(StatusCode::OK, serde_json::json!({"ok": true}))
}

pub async fn store_version_src(
    st: &ComposerState,
    name: &str,
    version: &str,
    data: Vec<u8>,
    source: &str,
) {
    let mut art = Artifact {
        format: "composer".into(),
        repository: name.to_string(),
        version: version.to_string(),
        source: source.to_string(),
        ..Default::default()
    };
    if !data.is_empty() {
        if let Ok((hashes, size)) = pkglab_common::artifact::compute_hashes(&data[..]) {
            let digest = format!("sha256:{}", hashes.sha256);
            let short = name.rsplit('/').next().unwrap_or(name);
            let mut cursor = Cursor::new(&data);
            if st.registry.blobs.put_if_absent(&digest, &mut cursor).await.is_ok() {
                art.blobs.push(Descriptor {
                    digest,
                    size: size as i64,
                    name: format!("{short}-{version}.zip"),
                    ..Default::default()
                });
            }
        }
        if let Some(autoload) = extract_autoload(&data) {
            art.proprietary = serde_json::json!({"autoload": autoload}).to_string().into_bytes();
        }
    }
    let _ = st.registry.meta.put(art).await;
}

/// Read composer.json bytes out of an upload payload: either the payload is
/// raw JSON or a zip containing composer.json.
fn composer_json_bytes(data: &[u8]) -> Vec<u8> {
    if data.len() >= 2 && data[0] == b'P' && data[1] == b'K' {
        let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(data)) else {
            return data.to_vec();
        };
        for i in 0..archive.len() {
            let Ok(mut f) = archive.by_index(i) else { continue };
            if f.name().ends_with("composer.json") {
                let mut buf = Vec::new();
                if f.read_to_end(&mut buf).is_ok() {
                    return buf;
                }
            }
        }
    }
    data.to_vec()
}

/// Read the "autoload" map out of an uploaded composer.json, which arrives
/// either as raw JSON or as a zip containing composer.json.
fn extract_autoload(data: &[u8]) -> Option<serde_json::Value> {
    let raw = composer_json_bytes(data);
    let doc: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    doc.get("autoload").cloned().filter(|v| v.is_object())
}

/// Extract name/version from composer.json bytes (raw or inside a zip).
fn composer_json_name_version(data: &[u8]) -> Option<(String, String)> {
    let raw = composer_json_bytes(data);
    let doc: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    let name = doc.get("name")?.as_str()?.to_string();
    let version = doc.get("version").and_then(|v| v.as_str()).unwrap_or("").to_string();
    Some((name, version))
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
    fn version_norm() {
        assert_eq!(normalize_version("1.0.0"), "1.0.0.0");
        assert_eq!(normalize_version("2.1"), "2.1.0.0");
        assert_eq!(normalize_version("1.2.3.4"), "1.2.3.4");
    }

    #[test]
    fn autoload_extract() {
        let doc = br#"{"name":"a/b","autoload":{"psr-4":{"A\\":"src/"}}}"#;
        let al = extract_autoload(doc).unwrap();
        assert!(al.get("psr-4").is_some());
        assert!(extract_autoload(br#"{"name":"a/b"}"#).is_none());
    }
}

#[cfg(test)]
mod tests_extras {
    use super::*;
    use std::io::Write;

    #[test]
    fn autoload_from_zip() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("composer.json", zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(br#"{"name":"a/b","autoload":{"psr-4":{"A\\":"src/"}}}"#).unwrap();
            z.finish().unwrap();
        }
        let al = extract_autoload(&buf.into_inner()).unwrap();
        assert!(al.get("psr-4").is_some());
    }
}
