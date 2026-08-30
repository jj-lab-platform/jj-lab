//! NuGet v3 protocol: service index, search/autocomplete (pull-through
//! fallback), registration index/leaf (64-leaf pages), flat container
//! index + nupkg download (pull-through with URL rewriting), push
//! (multipart or raw body with nuspec parsing), delete/relist.
use pkglab_common::httphelpers::urlencode;
use pkglab_common::httphelpers::{blob_response, error, json};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::put;
use pkglab_common::{Artifact, Descriptor};
use std::io::{Cursor, Read};
use std::sync::Arc;

pub struct NuGetState {
    pub registry: Arc<pkglab_common::Registry>,
    pub auth: Option<Arc<dyn pkglab_common::Auth>>,
    pub self_base: String,
}

fn lower(s: &str) -> String {
    s.to_lowercase()
}

fn is_prerelease(v: &str) -> bool {
    v.contains('-')
}

async fn authorize_write(state: &NuGetState, headers: &HeaderMap) -> Result<(), Response> {
    pkglab_common::httphelpers::authorize_write(&state.auth, headers).await
}

pub fn router(state: Arc<NuGetState>) -> axum::Router {
    let s0 = state.clone();
    axum::Router::new()
        .route("/v3/index.json", axum::routing::get(service_index))
        .route("/v3/query", axum::routing::get(search))
        .route("/v3/autocomplete", axum::routing::get(autocomplete))
        .route("/v3/registration/{id}/index.json", axum::routing::get(registration_index))
        .route("/v3/registration/{id}/{version}", axum::routing::get(registration_version))
        .route("/v3/flatcontainer/{id}/index.json", axum::routing::get(flat_index))
        .route("/v3/flatcontainer/{id}/{ver}/{filename}", axum::routing::get(flat_file))
        .route("/v3/package/{id}/{version}", axum::routing::delete(delete))
        .route("/api/v2/package/{id}/{version}", axum::routing::put(relist).delete(delete))
        .route("/v3/package", put(push))
        .route("/v3/package/", put(push))
        .route("/api/v2/package", put(push))
        .fallback(move |req: axum::http::Request<Body>| {
            let st = s0.clone();
            async move {
                // Also accept PUT /api/v2/package and trailing-slash variant.
                if req.method() == axum::http::Method::PUT
                    && matches!(
                        req.uri().path(),
                        "/api/v2/package" | "/api/v2/package/" | "/v3/package" | "/v3/package/"
                    )
                {
                    return Ok::<_, std::convert::Infallible>(
                        push(State(st), req.headers().clone(), req.into_body()).await,
                    );
                }
                Ok::<_, std::convert::Infallible>(StatusCode::NOT_FOUND.into_response())
            }
        })
        .with_state(state)
}

async fn service_index(State(st): State<Arc<NuGetState>>) -> Response {
    let base = &st.self_base;
    json(
        StatusCode::OK,
        serde_json::json!({
            "version": "3.0.0",
            "resources": [
                {"@id": format!("{base}/v3/query"), "@type": "SearchQueryService/3.5.0"},
                {"@id": format!("{base}/v3/autocomplete"), "@type": "SearchAutocompleteService/3.5.0"},
                {"@id": format!("{base}/v3/registration"), "@type": "RegistrationsBaseUrl/3.6.0"},
                {"@id": format!("{base}/v3/flatcontainer/"), "@type": "PackageBaseAddress/3.0.0"},
                {"@id": format!("{base}/v3/package"), "@type": "PackagePublish/2.0.0"},
            ],
        }),
    )
}

async fn autocomplete(
    State(st): State<Arc<NuGetState>>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
) -> Response {
    let qmap = parse_query(&q);
    let repos = st.registry.meta.list_repositories_by_format("nuget").await.unwrap_or_default();

    if let Some(id) = qmap.get("id") {
        let mut versions = st.registry.meta.list_versions("nuget", id).await.unwrap_or_default();
        let total = versions.len();
        if qmap.get("prerelease").map(|v| v != "true").unwrap_or(true) {
            versions.retain(|v| !is_prerelease(v));
        }
        return json(StatusCode::OK, serde_json::json!({"totalHits": total, "data": versions}));
    }
    let q = qmap.get("q").map(|v| v.to_lowercase()).unwrap_or_default();
    let data: Vec<String> =
        repos.into_iter().filter(|name| q.is_empty() || name.to_lowercase().contains(&q)).collect();
    let total = data.len();
    json(StatusCode::OK, serde_json::json!({"totalHits": total, "data": data}))
}

async fn search(
    State(st): State<Arc<NuGetState>>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
) -> Response {
    let qmap = parse_query(&q);
    let q = qmap.get("q").map(|v| v.to_lowercase()).unwrap_or_default();
    let repos = st.registry.meta.list_repositories_by_format("nuget").await.unwrap_or_default();
    let base = &st.self_base;
    let mut data = Vec::new();
    for name in repos {
        if !q.is_empty() && !name.to_lowercase().contains(&q) {
            continue;
        }
        let versions = st.registry.meta.list_versions("nuget", &name).await.unwrap_or_default();
        let latest = pkglab_common::versioncmp::highest(versions.iter())
            .map(str::to_string)
            .or_else(|| versions.last().cloned())
            .unwrap_or_default();
        data.push(serde_json::json!({
            "id": name,
            "version": latest,
            "registration": format!("{base}/v3/registration/{}/index.json", lower(&name)),
        }));
    }
    // Pull-through search when nothing local matches.
    if data.is_empty() && !q.is_empty() {
        if let Some(remote) = st.registry.remote_sub("nuget", "search") {
            if let Ok(body) =
                remote.get(&format!("/query?q={}&prerelease=false", urlencode(&q))).await
            {
                if body.status().is_success() {
                    if let Ok(bytes) = body.bytes().await {
                        let rewritten = rewrite_registration(&bytes, base);
                        return (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "application/json".to_string())],
                            String::from_utf8_lossy(&rewritten).to_string(),
                        )
                            .into_response();
                    }
                }
            }
        }
        return error(StatusCode::BAD_GATEWAY, "upstream");
    }
    let total = data.len();
    json(StatusCode::OK, serde_json::json!({"totalHits": total, "data": data}))
}

fn registration_leaf(base: &str, lower_id: &str, id: &str, v: &str) -> serde_json::Value {
    serde_json::json!({
        "@id": format!("{base}/v3/registration/{lower_id}/{v}.json"),
        "catalogEntry": {
            "@id": format!("{base}/v3/registration/{lower_id}/{v}.json"),
            "id": id,
            "version": v,
        },
        "packageContent": format!("{base}/v3/flatcontainer/{lower_id}/{v}/{lower_id}.{v}.nupkg"),
    })
}

async fn registration_index(
    State(st): State<Arc<NuGetState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let lower = lower(&id);
    let versions = st.registry.meta.list_versions("nuget", &id).await.unwrap_or_default();
    if versions.is_empty() {
        // Pull-through registration (cached).
        if let Some(remote) = st.registry.remote_sub("nuget", "registration") {
            if let Ok(body) = remote
                .get_cached(
                    &shared_cache(),
                    &format!("/v3/registration5-semver1/{lower}/index.json"),
                )
                .await
            {
                let rewritten = rewrite_registration(body.as_bytes(), &st.self_base);
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json".to_string())],
                    String::from_utf8_lossy(&rewritten).to_string(),
                )
                    .into_response();
            }
        }
        return error(StatusCode::NOT_FOUND, "package not found");
    }
    let base = &st.self_base;
    // Inline all leaves when small; page by 64 otherwise.
    const PAGE_SIZE: usize = 64;
    let leaves: Vec<serde_json::Value> =
        versions.iter().map(|v| registration_leaf(base, &lower, &id, v)).collect();
    let items = if leaves.len() <= PAGE_SIZE {
        vec![serde_json::json!({
            "@id": format!("{base}/v3/registration/{lower}/index.json"),
            "count": leaves.len(),
            "items": leaves,
        })]
    } else {
        leaves
            .chunks(PAGE_SIZE)
            .enumerate()
            .map(|(i, chunk)| {
                serde_json::json!({
                    "@id": format!("{base}/v3/registration/{lower}/page/{i}.json"),
                    "count": chunk.len(),
                    "items": chunk,
                })
            })
            .collect()
    };
    json(StatusCode::OK, serde_json::json!({"count": items.len(), "items": items}))
}

async fn registration_version(
    State(st): State<Arc<NuGetState>>,
    axum::extract::Path((id, ver)): axum::extract::Path<(String, String)>,
) -> Response {
    let ver = ver.trim_end_matches(".json");
    let lower = lower(&id);
    let base = &st.self_base;
    json(
        StatusCode::OK,
        serde_json::json!({
            "@id": format!("{base}/v3/registration/{lower}/{ver}.json"),
            "id": id,
            "version": ver,
            "listed": true,
            "published": "2024-01-01T00:00:00Z",
            "packageContent": format!("{base}/v3/flatcontainer/{lower}/{ver}/{lower}.{ver}.nupkg"),
            "catalogEntry": {
                "@id": format!("{base}/v3/registration/{lower}/{ver}.json"),
                "id": id,
                "version": ver,
                "listed": true,
                "published": "2024-01-01T00:00:00Z",
            },
        }),
    )
}

/// Rewrite upstream api.nuget.org URLs to self in registration/search bodies.
fn rewrite_registration(body: &[u8], self_base: &str) -> Vec<u8> {
    let s = String::from_utf8_lossy(body).to_string();
    let s = s
        .replace(
            "https://api.nuget.org/v3-flatcontainer/",
            &format!("{self_base}/v3/flatcontainer/"),
        )
        .replace(
            "https://api.nuget.org/v3/registration5-semver1/",
            &format!("{self_base}/v3/registration/"),
        )
        .replace(
            "https://api.nuget.org/v3/registration5-gz-semver2/",
            &format!("{self_base}/v3/registration/"),
        );
    s.replace(&format!("{self_base}/v3/registration//"), &format!("{self_base}/v3/registration/"))
        .into_bytes()
}

async fn flat_index(
    State(st): State<Arc<NuGetState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let lower = lower(&id);
    let versions = st.registry.meta.list_versions("nuget", &id).await.unwrap_or_default();
    if !versions.is_empty() {
        return json(StatusCode::OK, serde_json::json!({"versions": versions}));
    }
    // Pull-through flat container index.
    if let Some(remote) = st.registry.remote_sub("nuget", "registration") {
        if let Ok(data) = remote.get_bytes(&format!("/v3-flatcontainer/{lower}/index.json")).await {
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json".to_string())],
                String::from_utf8_lossy(&data).to_string(),
            )
                .into_response();
        }
    }
    json(StatusCode::OK, serde_json::json!({"versions": versions}))
}

async fn flat_file(
    State(st): State<Arc<NuGetState>>,
    axum::extract::Path((id, ver, filename)): axum::extract::Path<(String, String, String)>,
) -> Response {
    if let Ok(art) = st.registry.meta.get("nuget", &id, &ver).await {
        for b in &art.blobs {
            if let Ok(Some(mut r)) = st.registry.blobs.open(&b.digest).await {
                let mut data = Vec::new();
                if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                    return blob_response(data, &filename);
                }
            }
        }
    }
    // Pull-through nupkg.
    let lower = lower(&id);
    if let Some(remote) = st.registry.remote_sub("nuget", "registration") {
        if let Ok(data) =
            remote.get_bytes(&format!("/v3-flatcontainer/{lower}/{ver}/{filename}")).await
        {
            store_version_src(&st, &id, &ver, &filename, data.clone(), "pull").await;
            return blob_response(data, &filename);
        }
    }
    error(StatusCode::NOT_FOUND, "not found")
}

async fn delete(
    State(st): State<Arc<NuGetState>>,
    axum::extract::Path((id, version)): axum::extract::Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    if let Ok(art) = st.registry.meta.get("nuget", &id, &version).await {
        for b in &art.blobs {
            let _ = st.registry.blobs.delete(&b.digest).await;
        }
    }
    let _ = st.registry.meta.delete("nuget", &id, &version).await;
    StatusCode::NO_CONTENT.into_response()
}

async fn relist(
    State(st): State<Arc<NuGetState>>,
    axum::extract::Path(_pv): axum::extract::Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    StatusCode::OK.into_response()
}

async fn push(State(st): State<Arc<NuGetState>>, headers: HeaderMap, body: Body) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    let mut body = body;
    let raw = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(c) => c.to_bytes().to_vec(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
    };
    let ct = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    // `dotnet nuget push` sends multipart; older clients send the raw nupkg.
    let data = if ct.starts_with("multipart/form-data") {
        pkglab_common::multipart::extract_first_file(&raw, ct).map(|(_, part)| part).unwrap_or(raw)
    } else {
        raw
    };
    let (id, version) = parse_nuspec(&data);
    let id = if id.is_empty() { "unknown".to_string() } else { lower(&id) };
    let version = if version.is_empty() { "0.1.0".to_string() } else { version };
    let filename = format!("{id}.{version}.nupkg");
    store_version(&st, &id, &version, &filename, data).await;
    (
        StatusCode::CREATED,
        [(header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({"ok": true}).to_string(),
    )
        .into_response()
}

async fn store_version(st: &NuGetState, id: &str, version: &str, filename: &str, data: Vec<u8>) {
    store_version_src(st, id, version, filename, data, "push").await
}

pub async fn store_version_src(
    st: &NuGetState,
    id: &str,
    version: &str,
    filename: &str,
    data: Vec<u8>,
    source: &str,
) {
    let version = if version.is_empty() { "0.0.0" } else { version };
    let mut art = Artifact {
        format: "nuget".into(),
        repository: id.to_string(),
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

/// Parse `<id>`/`<version>` out of the .nuspec inside a .nupkg (zip).
fn parse_nuspec(nupkg: &[u8]) -> (String, String) {
    let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(nupkg)) else {
        return (String::new(), String::new());
    };
    for i in 0..archive.len() {
        let Ok(mut f) = archive.by_index(i) else { continue };
        if !f.name().to_lowercase().ends_with(".nuspec") {
            continue;
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_err() {
            continue;
        }
        let s = String::from_utf8_lossy(&buf);
        let id = extract_xml_tag(&s, "id");
        let version = extract_xml_tag(&s, "version");
        if !id.is_empty() && !version.is_empty() {
            return (id, version);
        }
    }
    (String::new(), String::new())
}

fn extract_xml_tag(xml: &str, tag: &str) -> String {
    let Some(start) = xml.find(&format!("<{tag}")) else {
        return String::new();
    };
    let Some(gt_rel) = xml[start..].find('>') else {
        return String::new();
    };
    let gt = start + gt_rel + 1;
    let Some(close_rel) = xml[gt..].find(&format!("</{tag}>")) else {
        return String::new();
    };
    xml[gt..gt + close_rel].trim().to_string()
}

fn parse_query(q: &Option<String>) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    if let Some(q) = q {
        for pair in q.split('&') {
            let mut it = pair.splitn(2, '=');
            let k = it.next().unwrap_or("").to_string();
            let v = it.next().unwrap_or("").replace("%20", " ").replace('+', " ");
            if !k.is_empty() {
                out.insert(k, v);
            }
        }
    }
    out
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
    fn xml_tags() {
        let xml = r#"<metadata><id>Foo.Bar</id><version>1.2.3</version></metadata>"#;
        assert_eq!(extract_xml_tag(xml, "id"), "Foo.Bar");
        assert_eq!(extract_xml_tag(xml, "version"), "1.2.3");
    }

    #[test]
    fn prerelease() {
        assert!(is_prerelease("1.0.0-beta"));
        assert!(!is_prerelease("1.0.0"));
    }

    #[test]
    fn registration_rewrite() {
        let body =
            br#"{"packageContent":"https://api.nuget.org/v3-flatcontainer/a/1.0/a.1.0.nupkg"}"#;
        let out = String::from_utf8_lossy(&rewrite_registration(body, "http://reg/pkgs/nuget"))
            .to_string();
        assert!(out.contains("http://reg/pkgs/nuget/v3/flatcontainer/"), "out={out}");
    }
}

#[cfg(test)]
mod tests_extras {
    use super::*;
    use std::io::Write;

    #[test]
    fn nuspec_parsing() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("Foo.Bar.nuspec", zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(
                b"<package><metadata><id>Foo.Bar</id><version>2.1.0</version></metadata></package>",
            )
            .unwrap();
            z.finish().unwrap();
        }
        let (id, version) = parse_nuspec(&buf.into_inner());
        assert_eq!(id, "Foo.Bar");
        assert_eq!(version, "2.1.0");
    }

    #[test]
    fn query_parsing() {
        let q = Some("q=hello&prerelease=true&skip=0".to_string());
        let m = parse_query(&q);
        assert_eq!(m.get("q").unwrap(), "hello");
        assert_eq!(m.get("prerelease").unwrap(), "true");
        assert_eq!(m.get("skip").unwrap(), "0");
    }
}
