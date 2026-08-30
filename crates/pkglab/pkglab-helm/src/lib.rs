//! Helm chart repository protocol: `index.yaml` (local entries merged with
//! the upstream index so public charts pull through), chart download via
//! `/charts/{file}` (local first, upstream fallback), and upload via
//! `/api/charts` (multipart `chart` field or raw body; name/version parsed
//! from Chart.yaml).

use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use flate2::read::GzDecoder;
use pkglab_common::httphelpers::{blob_response, error, json as json_ok};
use pkglab_common::{Artifact, Descriptor};
use std::io::Read;
use std::sync::Arc;
use tar::Archive;

pub struct HelmState {
    pub registry: Arc<pkglab_common::Registry>,
    pub auth: Option<Arc<dyn pkglab_common::Auth>>,
    pub self_base: String,
}

async fn authorize_write(state: &HelmState, headers: &HeaderMap) -> Result<(), Response> {
    pkglab_common::httphelpers::authorize_write(&state.auth, headers).await
}

pub fn router(state: Arc<HelmState>) -> axum::Router {
    axum::Router::new()
        .route("/index.yaml", axum::routing::get(index_yaml))
        .route("/charts/{*filename}", axum::routing::get(chart))
        .route("/api/charts", axum::routing::post(upload).put(upload))
        .route("/api/charts/{name}", axum::routing::delete(delete_chart))
        .route("/api/charts/{name}/{version}", axum::routing::delete(delete_chart_version))
        .with_state(state)
}

async fn self_base(state: &HelmState) -> String {
    state.self_base.clone()
}

async fn index_yaml(State(st): State<Arc<HelmState>>, _headers: HeaderMap) -> Response {
    let base = self_base(&st).await;
    let mut out = String::from("apiVersion: v1\nentries:\n");

    let repos = st.registry.meta.list_repositories_by_format("helm").await.unwrap_or_default();
    // Group every version of a chart under a single `name:` key (helm's YAML
    // rejects duplicate top-level keys; emitting one key per version breaks
    // the index once a chart has 2+ versions).
    for name in &repos {
        let versions = st.registry.meta.list_versions("helm", name).await.unwrap_or_default();
        if versions.is_empty() {
            continue;
        }
        out.push_str(&format!("  {name}:\n"));
        for v in versions {
            let Ok(art) = st.registry.meta.get("helm", name, &v).await else {
                continue;
            };
            let (fname, digest) = match art.blobs.first() {
                Some(b) => (b.name.clone(), b.hex().to_string()),
                None => (format!("{name}-{v}.tgz"), String::new()),
            };
            out.push_str(&format!(
                "    - apiVersion: v2\n      name: {name}\n      version: {v}\n      urls:\n        - {base}/charts/{fname}\n      created: 2024-01-01T00:00:00Z\n      digest: {digest}\n"
            ));
        }
    }

    // Merge the upstream index so `helm pull` of public charts works as a
    // pull-through mirror. Upstream URLs are rewritten to self.
    let upstream = st.registry.upstream("helm");
    if let Some(up_base) = upstream {
        let proxy = st.registry.upstreams.proxy_url("helm");
        let remote = pkglab_common::remote::Remote::new(
            &st.registry.upstreams.factory(),
            &up_base,
            proxy.as_deref(),
        );
        if let Ok(bytes) = remote.get_bytes("/index.yaml").await {
            let text = String::from_utf8_lossy(&bytes).to_string();
            if let Some(idx) = text.find("entries:") {
                let rewritten = text[idx + "entries:".len()..]
                    .replace("https://charts.helm.sh/stable/packages/", &format!("{base}/charts/"));
                out.push_str(&rewritten);
            }
        }
    }

    (StatusCode::OK, [(header::CONTENT_TYPE, "application/yaml".to_string())], out).into_response()
}

async fn chart(State(st): State<Arc<HelmState>>, Path(filename): Path<String>) -> Response {
    // Local hit.
    let repos = st.registry.meta.list_repositories_by_format("helm").await.unwrap_or_default();
    for name in repos {
        let versions = st.registry.meta.list_versions("helm", &name).await.unwrap_or_default();
        for v in versions {
            let Ok(art) = st.registry.meta.get("helm", &name, &v).await else {
                continue;
            };
            for b in &art.blobs {
                if b.name == filename {
                    if let Ok(Some(mut r)) = st.registry.blobs.open(&b.digest).await {
                        let mut data = Vec::new();
                        if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                            return blob_response(data, &filename);
                        }
                    }
                }
            }
        }
    }

    // Pull-through: fetch the chart .tgz from the upstream.
    if let Some(up) = st.registry.remote("helm", None) {
        match up.get(&format!("/packages/{filename}")).await {
            Ok(resp) => {
                let status = resp.status();
                if status == StatusCode::NOT_FOUND {
                    return error(StatusCode::NOT_FOUND, "chart not found");
                }
                if !status.is_success() {
                    return error(
                        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                        "upstream",
                    );
                }
                match resp.bytes().await {
                    Ok(bytes) => return blob_response(bytes.to_vec(), &filename),
                    Err(_) => return error(StatusCode::BAD_GATEWAY, "upstream read"),
                }
            }
            Err(_) => return error(StatusCode::BAD_GATEWAY, "upstream"),
        }
    }
    error(StatusCode::NOT_FOUND, "chart not found")
}

#[derive(serde::Deserialize, Default)]
struct UploadQuery {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
}

async fn upload(
    State(st): State<Arc<HelmState>>,
    RawQuery(q): RawQuery,
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

    let query: UploadQuery = serde_qs_or_default(&q);

    let mut data: Vec<u8> = Vec::new();
    let ct = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    if ct.starts_with("multipart/form-data") {
        // helm push uses multipart with a "chart" file field.
        if let Some((_fname, part)) = pkglab_common::multipart::extract_first_file(&raw, ct) {
            data = part;
        }
    }
    if data.is_empty() {
        data = raw;
    }

    let (mut name, mut version) =
        (query.name.unwrap_or_default(), query.version.unwrap_or_default());
    if name.is_empty() || version.is_empty() {
        if let Some((cn, cv)) = chart_name_version(&data) {
            if name.is_empty() {
                name = cn;
            }
            if version.is_empty() {
                version = cv;
            }
        }
    }
    if name.is_empty() {
        name = "unknown".into();
    }
    if version.is_empty() {
        version = "0.1.0".into();
    }
    let filename = format!("{name}-{version}.tgz");
    store_chart(&st, &name, &version, &filename, data).await;
    json_ok(StatusCode::CREATED, serde_json::json!({"saved": true}))
}

/// Store a chart release (shared by the HTTP upload path and pkglab-api).
pub async fn store_chart(st: &HelmState, name: &str, version: &str, filename: &str, data: Vec<u8>) {
    let mut art = Artifact {
        format: "helm".into(),
        repository: name.to_string(),
        version: version.to_string(),
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

fn serde_qs_or_default(q: &Option<String>) -> UploadQuery {
    let Some(q) = q else {
        return UploadQuery::default();
    };
    let mut out = UploadQuery::default();
    for pair in q.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next().unwrap_or("");
        let v = it.next().unwrap_or("").replace("%20", " ").replace('+', " ");
        match k {
            "name" => out.name = Some(v),
            "version" => out.version = Some(v),
            _ => {}
        }
    }
    out
}

/// Read Chart.yaml out of a chart .tgz (gzip + tar) and extract
/// name/version scalars.
fn chart_name_version(data: &[u8]) -> Option<(String, String)> {
    let gz = GzDecoder::new(data);
    let mut archive = Archive::new(gz);
    for entry in archive.entries().ok()? {
        let Ok(mut entry) = entry else { continue };
        let name = entry.path().ok()?.display().to_string();
        if !name.ends_with("Chart.yaml") {
            continue;
        }
        let mut buf = Vec::new();
        if entry.read_to_end(&mut buf).is_err() {
            continue;
        }
        let name = yaml_scalar(&buf, "name");
        let version = yaml_scalar(&buf, "version");
        return Some((name, version));
    }
    None
}

fn yaml_scalar(data: &[u8], key: &str) -> String {
    let text = String::from_utf8_lossy(data);
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&format!("{key}:")) {
            return rest.trim().trim_matches('"').trim_matches('\'').to_string();
        }
    }
    String::new()
}

async fn helm_remove_version(st: &HelmState, name: &str, version: &str) {
    if let Ok(art) = st.registry.meta.get("helm", name, version).await {
        for b in &art.blobs {
            let _ = st.registry.blobs.delete(&b.digest).await;
        }
    }
    let _ = st.registry.meta.delete("helm", name, version).await;
}

async fn delete_chart(
    State(st): State<Arc<HelmState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    if let Ok(versions) = st.registry.meta.list_versions("helm", &name).await {
        for v in versions {
            helm_remove_version(&st, &name, &v).await;
        }
    }
    json_ok(StatusCode::OK, serde_json::json!({"deleted": true}))
}

async fn delete_chart_version(
    State(st): State<Arc<HelmState>>,
    Path((name, version)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    helm_remove_version(&st, &name, &version).await;
    json_ok(StatusCode::OK, serde_json::json!({"deleted": true}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_scalars() {
        let yaml = b"apiVersion: v2\nname: \"my-chart\"\nversion: 1.2.3\n";
        assert_eq!(yaml_scalar(yaml, "name"), "my-chart");
        assert_eq!(yaml_scalar(yaml, "version"), "1.2.3");
        assert_eq!(yaml_scalar(yaml, "nope"), "");
    }
}
