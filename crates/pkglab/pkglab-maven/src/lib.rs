//! Maven 2 repository protocol: arbitrary path layout
//! `group/artifact/version/filename`, hash sidecars (`.md5`/`.sha1`),
//! `maven-metadata.xml` (artifact-level + SNAPSHOT version-level), PUT
//! publish, HEAD, and pull-through from the upstream (Maven Central).
//!
//! Coordinates: the artifactID is the third path segment from the end
//! (group = the leading segments joined by dots), matching the reference
//! implementation's storage model.

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use pkglab_common::httphelpers::{blob_response, error, text};
use pkglab_common::Descriptor;
use std::io::Cursor;
use std::sync::Arc;

pub struct MavenState {
    pub registry: Arc<pkglab_common::Registry>,
    pub auth: Option<Arc<dyn pkglab_common::Auth>>,
}

#[derive(Debug, Clone, Default)]
struct Coords {
    artifact_id: String,
    version: String,
}

/// Parse `group.../artifact/version/filename` into coordinates.
fn parse_maven_path(p: &str) -> Option<Coords> {
    let parts: Vec<&str> = p.trim_matches('/').split('/').collect();
    if parts.len() < 3 {
        return None;
    }
    let version = parts[parts.len() - 2];
    let artifact_id = parts[parts.len() - 3];
    Some(Coords {
        artifact_id: artifact_id.to_string(),
        version: version.to_string(),
    })
}

fn coords_from_hash_path(p: &str) -> Coords {
    let orig = p.trim_end_matches(".md5").trim_end_matches(".sha1");
    parse_maven_path(orig).unwrap_or_default()
}

async fn authorize_write(state: &MavenState, headers: &HeaderMap) -> Result<(), Response> {
    pkglab_common::httphelpers::authorize_write(&state.auth, headers).await
}

pub fn router(state: Arc<MavenState>) -> axum::Router {
    let s0 = state.clone();
    axum::Router::new()
        .route(
            "/{*path}",
            any(move |req: axum::http::Request<Body>| {
                let st = s0.clone();
                async move {
                    let method = req.method().clone();
                    let path = req.uri().path().to_string();
                    let headers = req.headers().clone();
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
    state: Arc<MavenState>,
    method: Method,
    path: &str,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let p = path
        .trim_start_matches('/')
        .strip_prefix("pkgs/maven/")
        .or_else(|| path.trim_start_matches('/').strip_prefix("maven/"))
        .unwrap_or(path.trim_start_matches('/'));

    // archetype-catalog.xml at the repository root.
    if p == "archetype-catalog.xml" {
        return text(
            StatusCode::OK,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><archetype-catalog></archetype-catalog>"
                .into(),
            "application/xml",
        );
    }

    // Hash sidecars (.md5/.sha1).
    if p.ends_with(".md5") || p.ends_with(".sha1") {
        if method == Method::PUT {
            return put_path(&state, p, coords_from_hash_path(p), headers, body).await;
        }
        return hash_file(&state, p).await;
    }

    // maven-metadata.xml (and its PUTs).
    if p.ends_with("maven-metadata.xml") {
        if method == Method::PUT {
            return put_metadata_path(&state, p, headers, body).await;
        }
        return metadata_xml(&state, p).await;
    }

    let Some(c) = parse_maven_path(p) else {
        return error(StatusCode::NOT_FOUND, "invalid maven path");
    };

    match method {
        Method::PUT => put_path(&state, p, c, headers, body).await,
        Method::HEAD => head_path(&state, p, c).await,
        Method::GET => get_path(&state, p, c).await,
        Method::DELETE => delete_path(&state, p, c, headers).await,
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

async fn get_path(state: &MavenState, p: &str, c: Coords) -> Response {
    let filename = p.rsplit('/').next().unwrap_or(p);
    if let Ok(art) = state.registry.meta.get("maven", &c.artifact_id, &c.version).await {
        for b in &art.blobs {
            if b.name != filename {
                continue;
            }
            if let Ok(Some(mut r)) = state.registry.blobs.open(&b.digest).await {
                let mut data = Vec::new();
                if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                    return blob_response(data, filename);
                }
            }
        }
    }
    // Pull-through.
    match state.registry.fetch("maven", "", &format!("/{p}")).await {
        Ok(fetched) => {
            store_version_src(
                state,
                &c.artifact_id,
                &c.version,
                filename,
                fetched.data.clone(),
                "pull",
            )
            .await;
            blob_response(fetched.data, filename)
        }
        Err(e) => {
            tracing::warn!("maven pull-through miss {p}: {e}");
            error(StatusCode::NOT_FOUND, "not found")
        }
    }
}

async fn head_path(state: &MavenState, p: &str, c: Coords) -> Response {
    let filename = p.rsplit('/').next().unwrap_or(p);
    if let Ok(art) = state.registry.meta.get("maven", &c.artifact_id, &c.version).await {
        for b in &art.blobs {
            if b.name == filename {
                return (StatusCode::OK, [(header::CONTENT_LENGTH, b.size.to_string())])
                    .into_response();
            }
        }
    }
    match state.registry.fetch("maven", "", &format!("/{p}")).await {
        Ok(fetched) => {
            (StatusCode::OK, [(header::CONTENT_LENGTH, fetched.size.to_string())]).into_response()
        }
        Err(_) => error(StatusCode::NOT_FOUND, "not found"),
    }
}

async fn put_path(
    state: &MavenState,
    p: &str,
    c: Coords,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(resp) = authorize_write(state, &headers).await {
        return resp;
    }
    let filename = p.rsplit('/').next().unwrap_or(p);
    let mut body = body;
    let data = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(x) => x.to_bytes().to_vec(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
    };
    store_version(state, &c.artifact_id, &c.version, filename, data).await;
    (
        StatusCode::CREATED,
        [(header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({"ok": true}).to_string(),
    )
        .into_response()
}

/// DELETE of a specific artifact file or a whole version directory.
async fn delete_path(state: &MavenState, p: &str, c: Coords, headers: HeaderMap) -> Response {
    if let Err(resp) = authorize_write(state, &headers).await {
        return resp;
    }
    let filename = p.rsplit('/').next().unwrap_or(p);
    if let Ok(art) = state.registry.meta.get("maven", &c.artifact_id, &c.version).await {
        let mut art = art;
        let keep: Vec<Descriptor> =
            art.blobs.iter().filter(|b| b.name != filename).cloned().collect();
        let removed: Vec<Descriptor> =
            art.blobs.iter().filter(|b| b.name == filename).cloned().collect();
        art.blobs = keep;
        if art.blobs.is_empty() {
            let _ = state.registry.meta.delete("maven", &c.artifact_id, &c.version).await;
        } else {
            let _ = state.registry.meta.put(art).await;
        }
        for b in &removed {
            let _ = state.registry.blobs.delete(&b.digest).await;
        }
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({"ok": true}).to_string(),
    )
        .into_response()
}

/// PUT of maven-metadata.xml at the artifact (not version) level: stored
/// under a sentinel version `maven-metadata` so it is served back.
async fn put_metadata_path(
    state: &MavenState,
    p: &str,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(resp) = authorize_write(state, &headers).await {
        return resp;
    }
    let Some(c) = parse_maven_path(p) else {
        return error(StatusCode::NOT_FOUND, "invalid path");
    };
    let mut body = body;
    let data = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(x) => x.to_bytes().to_vec(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
    };
    store_version(state, &c.artifact_id, "maven-metadata", p.rsplit('/').next().unwrap_or(p), data)
        .await;
    (
        StatusCode::CREATED,
        [(header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({"ok": true}).to_string(),
    )
        .into_response()
}

/// Serve `.md5`/`.sha1` sidecars from the stored blob's hash set; fall back
/// to the upstream hash file.
async fn hash_file(state: &MavenState, p: &str) -> Response {
    let is_md5 = p.ends_with(".md5");
    let orig = p.trim_end_matches(".md5").trim_end_matches(".sha1");
    let Some(c) = parse_maven_path(orig) else {
        return error(StatusCode::NOT_FOUND, "not found");
    };
    let filename = orig.rsplit('/').next().unwrap_or(orig);
    if let Ok(art) = state.registry.meta.get("maven", &c.artifact_id, &c.version).await {
        for b in &art.blobs {
            if b.name != filename {
                continue;
            }
            if let Ok(h) = state.registry.blobs.hashes_for(&b.digest).await {
                let value = if is_md5 { h.md5 } else { h.sha1 };
                if !value.is_empty() {
                    return text(StatusCode::OK, value, "text/plain");
                }
            }
        }
    }
    match state.registry.fetch("maven", "", &format!("/{p}")).await {
        Ok(fetched) => {
            text(StatusCode::OK, String::from_utf8_lossy(&fetched.data).to_string(), "text/plain")
        }
        Err(_) => error(StatusCode::NOT_FOUND, "not found"),
    }
}

/// Serve maven-metadata.xml: artifact-level (all versions) or version-level
/// (SNAPSHOT info).
async fn metadata_xml(state: &MavenState, p: &str) -> Response {
    let clean = p.trim_end_matches("maven-metadata.xml").trim_matches('/');
    let parts: Vec<&str> = clean.split('/').collect();
    if parts.is_empty() || parts[0].is_empty() {
        return text(StatusCode::OK, "<metadata></metadata>".into(), "application/xml");
    }

    // Version-level maven-metadata.xml is only meaningful for SNAPSHOT
    // versions (maven requests the timestamped <snapshot> block). A plain
    // version segment may be an artifactId that merely contains digits
    // (e.g. "e2e-lib-123"), so routing on any digit would misfire.
    if parts.len() >= 3 && parts[parts.len() - 1].ends_with("-SNAPSHOT") {
        return version_metadata_xml(state, &parts).await;
    }

    let artifact_id = parts[parts.len() - 1];
    let group_id = parts[..parts.len() - 1].join(".");
    let mut versions =
        state.registry.meta.list_versions("maven", artifact_id).await.unwrap_or_default();
    pkglab_common::versioncmp::sort_vec(&mut versions);
    if versions.is_empty() {
        return text(
            StatusCode::OK,
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><metadata><groupId>{group_id}</groupId><artifactId>{artifact_id}</artifactId><versioning><versions></versions></versioning></metadata>"
            ),
            "application/xml",
        );
    }
    let latest = pkglab_common::versioncmp::highest(versions.iter())
        .map(str::to_string)
        .or_else(|| versions.last().cloned())
        .unwrap_or_default();
    let release =
        pkglab_common::versioncmp::highest(versions.iter().filter(|v| !v.contains("-SNAPSHOT")))
            .map(str::to_string)
            .or_else(|| versions.iter().rfind(|v| !v.contains("-SNAPSHOT")).cloned())
            .unwrap_or(latest.clone());
    let mut sb = String::new();
    sb.push_str(&format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><metadata><groupId>{group_id}</groupId><artifactId>{artifact_id}</artifactId><versioning><latest>{latest}</latest><release>{release}</release><versions>"
    ));
    for v in &versions {
        sb.push_str(&format!("<version>{v}</version>"));
    }
    sb.push_str("</versions><lastUpdated>20240101120000</lastUpdated></versioning></metadata>");
    text(StatusCode::OK, sb, "application/xml")
}

/// Version-level maven-metadata.xml for a SNAPSHOT version.
async fn version_metadata_xml(state: &MavenState, parts: &[&str]) -> Response {
    let version = parts[parts.len() - 1];
    let artifact_id = parts[parts.len() - 2];
    let group_id = parts[..parts.len() - 2].join(".");

    let mut ts = "20240101.120000".to_string();
    let build = "1";
    if let Ok(art) = state.registry.meta.get("maven", artifact_id, version).await {
        for b in &art.blobs {
            if b.name.contains(&format!("-{version}")) {
                if let Some(s) = snapshot_timestamp_from_name(&b.name) {
                    ts = s;
                }
            }
        }
    }

    let timestamped = version.replace("-SNAPSHOT", &format!("-{ts}-{build}"));

    let sb = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><metadata><groupId>{group_id}</groupId><artifactId>{artifact_id}</artifactId><version>{version}</version><versioning><snapshot><timestamp>{ts}</timestamp><buildNumber>{build}</buildNumber></snapshot><lastUpdated>20240101120000</lastUpdated><snapshotVersions><snapshotVersion><extension>jar</extension><value>{timestamped}</value><updated>20240101120000</updated></snapshotVersion><snapshotVersion><extension>pom</extension><value>{timestamped}</value><updated>20240101120000</updated></snapshotVersion></snapshotVersions></versioning></metadata>"
    );
    text(StatusCode::OK, sb, "application/xml")
}

#[cfg(test)]
fn looks_like_version(s: &str) -> bool {
    if s.contains("-SNAPSHOT") {
        return true;
    }
    s.chars().any(|c| c.is_ascii_digit())
}

/// Extract `YYYYMMDD.HHMMSS-N` from a timestamped-snapshot filename
/// (`artifact-1.0-20240101.120000-1.jar`).
fn snapshot_timestamp_from_name(name: &str) -> Option<String> {
    // Scan every position for the pattern: 8 digits '.' 6 digits '-' digits.
    let b: Vec<char> = name.chars().collect();
    let is_dig = |c: char| c.is_ascii_digit();
    for i in 0..b.len() {
        if i + 8 > b.len() || !b[i..i + 8].iter().copied().all(is_dig) {
            continue;
        }
        let num1: String = b[i..i + 8].iter().collect();
        let mut j = i + 8;
        if j >= b.len() || b[j] != '.' {
            continue;
        }
        j += 1;
        if j + 6 > b.len() || !b[j..j + 6].iter().copied().all(is_dig) {
            continue;
        }
        let num2: String = b[j..j + 6].iter().collect();
        j += 6;
        if j >= b.len() || b[j] != '-' {
            continue;
        }
        j += 1;
        let start = j;
        while j < b.len() && is_dig(b[j]) {
            j += 1;
        }
        if j == start {
            continue;
        }
        let num3: String = b[start..j].iter().collect();
        return Some(format!("{num1}.{num2}-{num3}"));
    }
    None
}

async fn store_version(
    state: &MavenState,
    artifact_id: &str,
    version: &str,
    filename: &str,
    data: Vec<u8>,
) {
    store_version_src(state, artifact_id, version, filename, data, "push").await
}

pub async fn store_version_src(
    state: &MavenState,
    artifact_id: &str,
    version: &str,
    filename: &str,
    data: Vec<u8>,
    source: &str,
) {
    let version = if version.is_empty() { "0.0.0" } else { version };
    let mut art = state.registry.meta.get("maven", artifact_id, version).await.unwrap_or_default();
    art.format = "maven".into();
    art.source = source.to_string();
    art.repository = artifact_id.to_string();
    art.version = version.to_string();
    if !data.is_empty() {
        if let Ok((hashes, size)) = pkglab_common::artifact::compute_hashes(&data[..]) {
            let digest = format!("sha256:{}", hashes.sha256);
            // Replace (not append) same-named files on re-publish.
            let removed: Vec<Descriptor> =
                art.blobs.iter().filter(|b| b.name == filename).cloned().collect();
            art.blobs.retain(|b| b.name != filename);
            let mut cursor = Cursor::new(&data);
            if state.registry.blobs.put_if_absent(&digest, &mut cursor).await.is_ok() {
                let d = Descriptor {
                    digest: digest.clone(),
                    size: size as i64,
                    name: filename.to_string(),
                    ..Default::default()
                };
                art.blobs.push(d);
            }
            // Drop the old blob only if it is genuinely different (otherwise
            // it is still the live blob we just re-stored).
            for b in removed {
                if b.digest != digest {
                    let _ = state.registry.blobs.delete(&b.digest).await;
                }
            }
        }
    }
    let _ = state.registry.meta.put(art).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_coords() {
        let c = parse_maven_path("org/example/app/1.0.0/app-1.0.0.jar").unwrap();
        assert_eq!(c.artifact_id, "app");
        assert_eq!(c.version, "1.0.0");
        let c = parse_maven_path("com/foo/bar-baz/2.1/bar-baz-2.1.pom").unwrap();
        assert_eq!(c.artifact_id, "bar-baz");
        assert_eq!(c.version, "2.1");
    }

    #[test]
    fn snapshot_ts() {
        assert_eq!(
            snapshot_timestamp_from_name("app-1.0-20240101.120000-3.jar")
                .unwrap_or_else(|| "<none>".into()),
            "20240101.120000-3"
        );
        assert_eq!(snapshot_timestamp_from_name("app-1.0.jar"), None);
    }

    #[test]
    fn version_like() {
        assert!(looks_like_version("1.0.0-SNAPSHOT"));
        assert!(looks_like_version("2.1"));
        assert!(!looks_like_version("metadata"));
    }
}

#[cfg(test)]
mod tests_extras {
    use super::*;

    #[test]
    fn snapshot_timestamp_variants() {
        assert_eq!(
            snapshot_timestamp_from_name("app-1.0-20240101.120000-3.jar"),
            Some("20240101.120000-3".to_string())
        );
        assert_eq!(snapshot_timestamp_from_name("app-1.0.jar"), None);
        assert_eq!(snapshot_timestamp_from_name("no-digits-here.txt"), None);
    }
}
