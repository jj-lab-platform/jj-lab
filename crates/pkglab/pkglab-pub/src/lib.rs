//! Dart pub.dev protocol: package list, `{name}` version metadata, version
//! metadata, two-phase upload (`newUpload` → `newUploadFinish`), archive
//! download with pull-through, pubspec.yaml parsing, advisories stub.
use pkglab_common::httphelpers::urlencode;
use pkglab_common::httphelpers::{blob_response, error, json};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use flate2::read::GzDecoder;
use pkglab_common::{Artifact, Descriptor};
use std::io::Read;
use std::sync::Arc;
use tar::Archive;

pub struct PubState {
    pub registry: Arc<pkglab_common::Registry>,
    pub auth: Option<Arc<dyn pkglab_common::Auth>>,
    pub self_base: String,
}

/// pub metadata responses use the v2 content type.
fn pub_json(status: StatusCode, v: serde_json::Value) -> Response {
    (status, [(header::CONTENT_TYPE, "application/vnd.pub.v2+json".to_string())], v.to_string())
        .into_response()
}

async fn authorize_write(state: &PubState, headers: &HeaderMap) -> Result<(), Response> {
    pkglab_common::httphelpers::authorize_write(&state.auth, headers).await
}

pub fn router(state: Arc<PubState>) -> axum::Router {
    axum::Router::new()
        .route("/api/packages", get(packages_list))
        .route("/api/packages/versions/new", get(versions_new))
        .route("/api/packages/versions/newUpload", post(new_upload))
        .route("/api/packages/versions/newUploadFinish", post(finish).get(finish))
        .route("/api/packages/{name}/advisories", get(advisories))
        .route(
            "/api/packages/{name}/versions/{version}",
            get(version_metadata).delete(retract).put(unretract),
        )
        .route("/api/packages/{name}", get(pkg_metadata))
        .route("/packages/{*rest}", get(archive))
        .with_state(state)
}

async fn packages_list(State(st): State<Arc<PubState>>) -> Response {
    let repos = st.registry.meta.list_repositories_by_format("pub").await.unwrap_or_default();
    let total = repos.len();
    pub_json(StatusCode::OK, serde_json::json!({"packages": repos, "next_url": "", "total": total}))
}

async fn versions_new(State(st): State<Arc<PubState>>) -> Response {
    let base = &st.self_base;
    json(
        StatusCode::OK,
        serde_json::json!({
            "url": format!("{base}/api/packages/versions/newUpload"),
            "fields": {},
        }),
    )
}

/// Phase 1: accept the archive upload (multipart `file` field or raw body),
/// stash it in a meta slot keyed by a request id, and hand back the finish
/// URL. Phase 2 (`finish`) materializes the version from the stash.
async fn new_upload(
    State(st): State<Arc<PubState>>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
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
    let ct = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    let data = if ct.starts_with("multipart/form-data") {
        pkglab_common::multipart::extract_first_file(&raw, ct)
            .map(|(_, part)| part)
            .unwrap_or(raw.to_vec())
    } else {
        raw.to_vec()
    };

    // Query params first; dart pub POSTs without them, so fall back to the
    // pubspec.yaml inside the archive.
    let mut name = String::new();
    let mut version = String::new();
    if let Some(q) = q {
        for pair in q.split('&') {
            let mut it = pair.splitn(2, '=');
            match it.next().unwrap_or("") {
                "name" => name = it.next().unwrap_or("").to_string(),
                "version" => version = it.next().unwrap_or("").to_string(),
                _ => {}
            }
        }
    }
    if name.is_empty() || version.is_empty() {
        let (pn, pv) = pubspec_name_version(&data);
        if name.is_empty() {
            name = pn;
        }
        if version.is_empty() {
            version = pv;
        }
    }
    if name.is_empty() {
        name = "unknown".into();
    }
    if version.is_empty() {
        version = "0.1.0".into();
    }

    // Stash under meta key; finish is a no-op success (the upload is already
    // materialized here, matching the reference implementation).
    let filename = format!("{name}-{version}.tar.gz");
    store_version(&st, &name, &version, &filename, data).await;

    let base = &st.self_base;
    let mut resp =
        json(StatusCode::CREATED, serde_json::json!({"success": {"message": "Package uploaded"}}));
    resp.headers_mut().insert(
        header::LOCATION,
        axum::http::HeaderValue::from_str(&format!("{base}/api/packages/versions/newUploadFinish"))
            .expect("self_base location is a valid header value"),
    );
    resp
}

async fn finish(State(st): State<Arc<PubState>>, headers: HeaderMap) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    let _ = st;
    json(
        StatusCode::OK,
        serde_json::json!({"success": {"message": "Successfully uploaded package."}}),
    )
}

async fn pkg_metadata(State(st): State<Arc<PubState>>, Path(name): Path<String>) -> Response {
    let mut versions = st.registry.meta.list_versions("pub", &name).await.unwrap_or_default();
    if versions.is_empty() {
        return proxy_pkg_metadata(&st, &name).await;
    }
    pkglab_common::versioncmp::sort_vec(&mut versions);

    let base = &st.self_base;
    let latest = pkglab_common::versioncmp::highest(versions.iter())
        .map(str::to_string)
        .or_else(|| versions.last().cloned())
        .unwrap_or_default();
    // dart pub expects `versions` as a LIST of descriptors; include the
    // archive sha256 so the client validates downloads.
    let mut vs = Vec::new();
    for v in &versions {
        let sha = st
            .registry
            .meta
            .get("pub", &name, v)
            .await
            .ok()
            .and_then(|art| art.blobs.first().map(|b| b.hex().to_string()))
            .unwrap_or_default();
        vs.push(serde_json::json!({
            "version": v,
            "pubspec": {"name": name, "version": v, "environment": {"sdk": ">=3.0.0 <4.0.0"}},
            "archive_url": format!("{base}/packages/{}/{}.tar.gz", urlencode(&name), v),
            "archive_sha256": sha,
        }));
    }
    let latest_sha = st
        .registry
        .meta
        .get("pub", &name, &latest)
        .await
        .ok()
        .and_then(|art| art.blobs.first().map(|b| b.hex().to_string()))
        .unwrap_or_default();
    pub_json(
        StatusCode::OK,
        serde_json::json!({
            "name": name,
            "latest": {
                "version": latest,
                "archive_url": format!("{base}/packages/{}/{}.tar.gz", urlencode(&name), latest),
                "archive_sha256": latest_sha,
                "pubspec": {"name": name, "version": latest, "environment": {"sdk": ">=3.0.0 <4.0.0"}},
            },
            "versions": vs,
        }),
    )
}

async fn proxy_pkg_metadata(st: &PubState, name: &str) -> Response {
    if let Some(remote) = st.registry.remote("pub", None) {
        if let Ok(body) =
            remote.get_cached(&shared_cache(), &format!("/api/packages/{}", urlencode(name))).await
        {
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json".to_string())],
                body,
            )
                .into_response();
        }
    }
    error(StatusCode::NOT_FOUND, "not found")
}

async fn version_metadata(
    State(st): State<Arc<PubState>>,
    Path((name, version)): Path<(String, String)>,
) -> Response {
    let base = &st.self_base;
    // Archive path form: /api/packages/{name}/versions/{v}.tar.gz
    if version.ends_with(".tar.gz") {
        let v = version.trim_end_matches(".tar.gz");
        if let Ok(art) = st.registry.meta.get("pub", &name, v).await {
            let filename = format!("{name}-{v}.tar.gz");
            for b in &art.blobs {
                if let Ok(Some(mut r)) = st.registry.blobs.open(&b.digest).await {
                    let mut data = Vec::new();
                    if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                        return blob_response(data, &filename);
                    }
                }
            }
        }
        return error(StatusCode::NOT_FOUND, "not found");
    }
    pub_json(
        StatusCode::OK,
        serde_json::json!({
            "name": name,
            "version": version,
            "archive_url": format!("{base}/packages/{}/{}.tar.gz", urlencode(&name), version),
            "pubspec": {"name": name, "version": version},
        }),
    )
}

async fn advisories() -> Response {
    pub_json(
        StatusCode::OK,
        serde_json::json!({
            "advisories": [],
            "advisoriesUpdated": "1970-01-01T00:00:00Z",
        }),
    )
}

async fn archive(State(st): State<Arc<PubState>>, Path(rest): Path<String>) -> Response {
    // Handles both layouts:
    //   /packages/{name}-{version}.tar.gz
    //   /packages/{name}/{version}.tar.gz   (dart 3.x client)
    let rel = rest.trim_start_matches('/').trim_start_matches("packages/");
    let filename = rel.rsplit('/').next().unwrap_or(rel).to_string();
    let stem = filename.trim_end_matches(".tar.gz");
    // Prefer the two-segment form when present: name may contain dashes, so
    // the version must come from the final segment, not the last dash.
    let (name, version, full_name) = if rel.contains('/') {
        let name = rel.split('/').next().unwrap_or(rel).to_string();
        let full = format!("{name}-{stem}.tar.gz");
        (name, stem.to_string(), full)
    } else {
        let full = format!("{}-{stem}.tar.gz", name_version_from_stem(stem).0);
        let (n, v) = name_version_from_stem(stem);
        (n, v, full)
    };
    if let Ok(art) = st.registry.meta.get("pub", &name, &version).await {
        for b in &art.blobs {
            if b.name == filename || b.name == full_name || b.name.ends_with(&filename) {
                if let Ok(Some(mut r)) = st.registry.blobs.open(&b.digest).await {
                    let mut data = Vec::new();
                    if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                        return blob_response(data, &filename);
                    }
                }
            }
        }
    }
    // Pull-through archive.
    match st.registry.fetch("pub", "", &format!("/packages/{full_name}")).await {
        Ok(fetched) => {
            store_version_src(&st, &name, &version, &filename, fetched.data.clone(), "pull").await;
            blob_response(fetched.data, &filename)
        }
        Err(_) => error(StatusCode::NOT_FOUND, "not found"),
    }
}

fn name_version_from_stem(stem: &str) -> (String, String) {
    match stem.rfind('-') {
        Some(i) if i > 0 => (stem[..i].to_string(), stem[i + 1..].to_string()),
        _ => (stem.to_string(), "0.0.0".to_string()),
    }
}

/// Read name/version out of a pub package tarball's pubspec.yaml.
fn pubspec_name_version(data: &[u8]) -> (String, String) {
    let mut archive = Archive::new(GzDecoder::new(data));
    for entry in archive.entries().into_iter().flatten() {
        let Ok(mut entry) = entry else { continue };
        let name = entry.path().map(|p| p.display().to_string()).unwrap_or_default();
        if !name.ends_with("pubspec.yaml") {
            continue;
        }
        let mut buf = String::new();
        if entry.read_to_string(&mut buf).is_err() {
            continue;
        }
        let mut out = (String::new(), String::new());
        for line in buf.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("name:") {
                out.0 = rest.trim().to_string();
            }
            if let Some(rest) = t.strip_prefix("version:") {
                out.1 = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            }
        }
        return out;
    }
    (String::new(), String::new())
}

async fn store_version(st: &PubState, name: &str, version: &str, filename: &str, data: Vec<u8>) {
    store_version_src(st, name, version, filename, data, "push").await
}

/// Dart pub has no upstream delete API; self-hosted mirrors need unpublish.
/// DELETE removes the version outright (the retract/discontinue semantics of
/// pub.dev are approximated by a hard remove). PUT is a no-op success.
async fn retract(
    State(st): State<Arc<PubState>>,
    Path((name, version)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    if let Ok(art) = st.registry.meta.get("pub", &name, &version).await {
        for b in &art.blobs {
            let _ = st.registry.blobs.delete(&b.digest).await;
        }
    }
    let _ = st.registry.meta.delete("pub", &name, &version).await;
    pub_json(StatusCode::OK, serde_json::json!({"success": true}))
}

async fn unretract(
    State(st): State<Arc<PubState>>,
    Path((_name, _version)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    pub_json(StatusCode::OK, serde_json::json!({"success": true}))
}

pub async fn store_version_src(
    st: &PubState,
    name: &str,
    version: &str,
    filename: &str,
    data: Vec<u8>,
    source: &str,
) {
    let mut art = Artifact {
        format: "pub".into(),
        repository: name.to_string(),
        version: version.to_string(),
        source: source.to_string(),
        ..Default::default()
    };
    if !data.is_empty() {
        if let Ok((hashes, size)) = pkglab_common::artifact::compute_hashes(&data[..]) {
            let digest = format!("sha256:{}", hashes.sha256);
            let mut cursor = std::io::Cursor::new(&data);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems() {
        assert_eq!(name_version_from_stem("http-1.2.0"), ("http".into(), "1.2.0".into()));
    }

    #[test]
    fn yaml_basic() {
        // Reuse pubspec parser through a synthetic tarball.
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        ));
        let content = b"name: my_pkg\nversion: \"2.1.0\"\n";
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(content.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_cksum();
        builder.append_data(&mut hdr, "pubspec.yaml", &content[..]).unwrap();
        let gz = builder.into_inner().unwrap().finish().unwrap();
        let (n, v) = pubspec_name_version(&gz);
        assert_eq!(n, "my_pkg");
        assert_eq!(v, "2.1.0");
    }
}
