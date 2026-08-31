//! OCI Distribution protocol adapter (spec v1.1).
//!
//! Routes (spec-fixed paths):
//! - `GET /v2/`                       version check
//! - `GET /v2/_catalog`               repository list
//! - `GET|HEAD /v2/{name}/manifests/{ref}` (GET/HEAD/PUT/DELETE)
//! - `GET|HEAD|DELETE /v2/{name}/blobs/{digest}`
//! - `POST|GET|PATCH|PUT|DELETE /v2/{name}/blobs/uploads[/{id}]`
//! - `GET /v2/{name}/tags/list`
//! - `GET /v2/{name}/referrers/{digest}` (OCI 1.1)
//!
//! Pull-through: on a local miss, manifests/blobs are fetched from the
//! configured upstream (default Docker Hub; arbitrary registry prefixes in
//! the name are honored), cached locally, and streamed to the client.

pub mod manifest;
pub mod uploads;
pub mod upstream;

use axum::body::Body;
use axum::extract::RawQuery;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use manifest::*;
use pkglab_common::auth::{scope, Action, Auth};
use pkglab_common::blob::{BlobReader, BlobStore};
use pkglab_common::registry::RegistryError;
use pkglab_common::store::ArtifactStore;
use pkglab_common::upstreams::Upstreams;
use pkglab_common::Artifact;
use std::collections::HashMap;
use std::sync::Arc;
use uploads::Uploads;

pub struct OciState {
    pub blobs: Arc<dyn BlobStore>,
    pub meta: Arc<dyn ArtifactStore>,
    pub upstreams: Upstreams,
    pub auth: Option<Arc<dyn Auth>>,
    /// Default pull-through upstream (scheme://host); None disables
    /// pull-through.
    pub default_upstream: Option<String>,
    /// Absolute base URL of this server (scheme://host[:port]); used to emit
    /// the token endpoint as an absolute realm in auth challenges.
    pub self_base: String,
}

pub struct OciAdapter {
    pub state: Arc<OciState>,
    uploads: Arc<Uploads>,
    /// On-demand upstreams for explicitly prefixed registries (ghcr.io/...).
    registry_upstreams: tokio::sync::Mutex<HashMap<String, upstream::Upstream>>,
}

impl OciAdapter {
    pub fn new(state: Arc<OciState>) -> Arc<Self> {
        let tmp = std::env::temp_dir().join("pkglab-oci-uploads");
        let uploads = Arc::new(Uploads::new(state.blobs.clone(), state.meta.clone(), tmp));
        Arc::new(Self {
            state,
            uploads,
            registry_upstreams: tokio::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Restore persisted upload sessions (call once at startup).
    pub async fn restore_uploads(&self) {
        self.uploads.restore().await;
    }

    /// Absolute token realm URL for auth challenges. When the base has a
    /// scheme it is used verbatim; otherwise the token endpoint must be an
    /// absolute URL so OCI clients (skopeo) can resolve it.
    fn token_realm(&self) -> String {
        let base = self.state.self_base.trim_end_matches('/');
        if base.starts_with("http://") || base.starts_with("https://") {
            format!("{base}/token")
        } else {
            format!("http://{base}/token")
        }
    }

    /// The pull-through upstream for a registry host ("" = default upstream).
    async fn upstream_for_registry(&self, host: &str) -> Option<upstream::Upstream> {
        let host = host.trim().to_lowercase();
        match host.as_str() {
            "" => {
                let base = self.state.default_upstream.as_ref()?;
                Some(self.make_upstream(base))
            }
            "docker.io" | "index.docker.io" | "registry-1.docker.io" => {
                let base = self.state.default_upstream.as_ref()?;
                Some(self.make_upstream(base))
            }
            _ => {
                let mut map = self.registry_upstreams.lock().await;
                if let Some(u) = map.get(&host) {
                    return Some(u.clone());
                }
                let proxy = self.state.upstreams.proxy_url(&host);
                let u =
                    upstream::Upstream::for_registry(&self.state.upstreams.factory(), &host, proxy);
                map.insert(host, u.clone());
                Some(u)
            }
        }
    }

    fn make_upstream(&self, base: &str) -> upstream::Upstream {
        // OCI uses its own upstream entry ("oci") for proxy policy.
        let proxy = self.state.upstreams.proxy_url("oci");
        upstream::Upstream::new(&self.state.upstreams.factory(), base, proxy)
    }

    fn error(status: StatusCode, code: &str, message: &str) -> Response {
        (
            status,
            [(axum::http::header::CONTENT_TYPE, "application/json".to_string())],
            serde_json::json!({
                "errors": [{"code": code, "message": message, "detail": ""}]
            })
            .to_string(),
        )
            .into_response()
    }

    fn not_found() -> Response {
        Self::error(StatusCode::NOT_FOUND, "MANIFEST_UNKNOWN", "manifest unknown")
    }

    async fn authorize(
        &self,
        headers: &HeaderMap,
        name: &str,
        action: Action,
    ) -> Result<(), Response> {
        let Some(auth) = &self.state.auth else {
            return Ok(());
        };
        let wanted = scope(name, action);
        // Raw token / any valid credential grants access (the default Auth
        // implementation is permissive; scope enforcement lives in impls).
        let tok = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.strip_prefix("Bearer ").unwrap_or(v))
            .unwrap_or("");
        if !tok.is_empty() && auth.check_bearer(tok, &wanted).await.is_some() {
            return Ok(());
        }
        if !auth.authenticate(headers).await.is_empty() {
            return Ok(());
        }
        // Challenge per distribution spec.
        let realm = self.token_realm();
        let resp = (
            StatusCode::UNAUTHORIZED,
            [(
                axum::http::header::WWW_AUTHENTICATE,
                format!("Bearer realm=\"{realm}\",service=\"oci-registry\",scope=\"{wanted}\""),
            )],
        )
            .into_response();
        Err(resp)
    }
}

/// Route `/{name}/manifests/{ref}`-style relative paths by method.
pub fn router(state: Arc<OciState>) -> axum::Router {
    let adapter = OciAdapter::new(state);
    let a1 = adapter.clone();
    tokio::spawn(async move { adapter.restore_uploads().await });

    axum::Router::new().route("/token", axum::routing::any(token_endpoint)).fallback_service(
        tower::service_fn(move |req: axum::http::Request<Body>| {
            let a1 = a1.clone();
            async move {
                let method = req.method().clone();
                let uri = req.uri().clone();
                let headers = req.headers().clone();
                let raw = RawQuery(uri.query().map(|q| q.to_string()));
                let body = req.into_body();
                Ok::<_, std::convert::Infallible>(
                    dispatch(a1.clone(), method, uri, headers, raw, body).await,
                )
            }
        }),
    )
}

async fn ping(adapter: &Arc<OciAdapter>) -> Response {
    // Distribution spec: when authentication is enabled, challenge every
    // anonymous client on the /v2 ping so it obtains a bearer token before
    // proceeding (Docker Hub behavior). Skopeo relies on this challenge; a
    // bare 200 here makes it treat a subsequent 401 as fatal instead of
    // retrying with credentials.
    if adapter.state.auth.is_some() {
        let realm = adapter.token_realm();
        return (
            StatusCode::UNAUTHORIZED,
            [(
                axum::http::header::WWW_AUTHENTICATE,
                format!("Bearer realm=\"{realm}\",service=\"oci-registry\""),
            )],
        )
            .into_response();
    }
    (
        [(
            axum::http::header::HeaderName::from_static("docker-distribution-api-version"),
            "registry/2.0".to_string(),
        )],
        StatusCode::OK,
    )
        .into_response()
}

/// Tiny shim: fetch the adapter from a process-global (single-instance dev
/// server assumption) to keep handlers simple.
static ADAPTER: tokio::sync::OnceCell<Arc<OciAdapter>> = tokio::sync::OnceCell::const_new();

async fn token_endpoint(RawQuery(raw): RawQuery, headers: HeaderMap) -> Response {
    let Some(a) = ADAPTER.get().cloned() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let mut scopes: Vec<String> = Vec::new();
    if let Some(q) = raw {
        // URL-decode the query and gather scope values. Docker sends space-
        // separated scopes; skopeo (containers/image) sends a single scope
        // whose action list is comma-separated ("repository:x:pull,push").
        // Normalize by expanding each scope into one entry per action.
        for (k, v) in url::form_urlencoded::parse(q.as_bytes()) {
            if k != "scope" {
                continue;
            }
            for s in v.split(' ') {
                if s.is_empty() {
                    continue;
                }
                if let Some((repo, acts)) = s.rsplit_once(':') {
                    for a in acts.split(',') {
                        if !a.is_empty() {
                            scopes.push(format!("{repo}:{a}"));
                        }
                    }
                } else {
                    scopes.push(s.to_string());
                }
            }
        }
    }
    let Some(auth) = &a.state.auth else {
        // Anonymous: issue an empty-scope token.
        return (
            StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                "application/json".to_string(),
            )],
            serde_json::json!({"token": "anonymous", "access_token": "anonymous", "expires_in": 3600}).to_string(),
        )
            .into_response();
    };
    let username = auth.authenticate(&headers).await;
    // Refuse push/delete scopes for anonymous.
    for s in &scopes {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() == 3
            && (parts[2] == "push" || parts[2] == "delete" || parts[2] == "*")
            && username.is_empty()
        {
            return (
                StatusCode::UNAUTHORIZED,
                [(axum::http::header::WWW_AUTHENTICATE, "Basic realm=\"/token\"".to_string())],
                serde_json::json!({
                    "errors": [{"code": "UNAUTHORIZED", "message": "authentication required"}]
                })
                .to_string(),
            )
                .into_response();
        }
    }
    let token = auth.issue_token(&username, &scopes, 3600).await;
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({
            "token": token,
            "access_token": token,
            "expires_in": 3600,
            "issued_at": "2026-01-01T00:00:00Z",
        })
        .to_string(),
    )
        .into_response()
}

/// The big method-dispatch fallback for every /v2/* path.
async fn dispatch(
    adapter: Arc<OciAdapter>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: RawQuery,
    body: Body,
) -> Response {
    let path = uri.path().to_string();
    let rel = path.trim_start_matches('/');
    let rel = rel.strip_prefix("v2/").unwrap_or(rel);

    // Ensure the OnceCell is set when the router was built without the
    // extension state (devserver path).
    let _ = ADAPTER.get_or_init(|| async { adapter.clone() }).await;

    if rel.is_empty() || rel == "/" {
        return ping(&adapter).await;
    }
    if rel == "_catalog" {
        return catalog_response(&adapter, raw).await;
    }

    if let Some(name) = rel.strip_suffix("/tags/list") {
        if adapter.authorize(&headers, name, Action::Pull).await.is_err() {
            return challenge(&adapter, name, Action::Pull).await;
        }
        return list_tags(&adapter, name, raw).await;
    }

    if let Some(idx) = rel.rfind("/referrers/") {
        let name = rel[..idx].to_string();
        let digest = rel[idx + "/referrers/".len()..].to_string();
        if adapter.authorize(&headers, &name, Action::Pull).await.is_err() {
            return challenge(&adapter, &name, Action::Pull).await;
        }
        return list_referrers(&adapter, &name, &digest, raw).await;
    }

    if let Some(idx) = rel.rfind("/manifests/") {
        let name = rel[..idx].to_string();
        let reference = rel[idx + "/manifests/".len()..].to_string();
        return match method {
            Method::HEAD => {
                if adapter.authorize(&headers, &name, Action::Pull).await.is_err() {
                    return challenge(&adapter, &name, Action::Pull).await;
                }
                get_manifest(&adapter, &name, &reference, false).await
            }
            Method::GET => {
                if adapter.authorize(&headers, &name, Action::Pull).await.is_err() {
                    return challenge(&adapter, &name, Action::Pull).await;
                }
                get_manifest(&adapter, &name, &reference, true).await
            }
            Method::PUT => {
                if adapter.authorize(&headers, &name, Action::Push).await.is_err() {
                    return challenge(&adapter, &name, Action::Push).await;
                }
                put_manifest(&adapter, &name, &reference, headers, raw, body).await
            }
            Method::DELETE => {
                if adapter.authorize(&headers, &name, Action::Delete).await.is_err() {
                    return challenge(&adapter, &name, Action::Delete).await;
                }
                delete_manifest(&adapter, &name, &reference).await
            }
            _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
        };
    }

    if let Some(idx) = rel.rfind("/blobs/uploads/") {
        let name = rel[..idx].to_string();
        let session = rel[idx + "/blobs/uploads/".len()..].to_string();
        if adapter.authorize(&headers, &name, Action::Push).await.is_err() {
            return challenge(&adapter, &name, Action::Push).await;
        }
        return uploads_dispatch(&adapter, &name, &session, method, raw, headers, body).await;
    }

    if let Some(idx) = rel.rfind("/blobs/") {
        let name = rel[..idx].to_string();
        let digest = rel[idx + "/blobs/".len()..].to_string();
        return match method {
            Method::HEAD => {
                if adapter.authorize(&headers, &name, Action::Pull).await.is_err() {
                    return challenge(&adapter, &name, Action::Pull).await;
                }
                check_blob(&adapter, &digest).await
            }
            Method::GET => {
                if adapter.authorize(&headers, &name, Action::Pull).await.is_err() {
                    return challenge(&adapter, &name, Action::Pull).await;
                }
                get_blob(&adapter, &name, &digest, &headers).await
            }
            Method::DELETE => {
                if adapter.authorize(&headers, &name, Action::Delete).await.is_err() {
                    return challenge(&adapter, &name, Action::Delete).await;
                }
                delete_blob(&adapter, &digest).await
            }
            _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
        };
    }

    OciAdapter::not_found()
}

async fn challenge(adapter: &Arc<OciAdapter>, name: &str, action: Action) -> Response {
    let wanted = scope(name, action);
    let realm = adapter.token_realm();
    (
        StatusCode::UNAUTHORIZED,
        [(
            axum::http::header::WWW_AUTHENTICATE,
            format!("Bearer realm=\"{realm}\",service=\"oci-registry\",scope=\"{wanted}\""),
        )],
    )
        .into_response()
}

async fn catalog_response(adapter: &Arc<OciAdapter>, _raw: RawQuery) -> Response {
    let mut repos = adapter.state.meta.list_repositories_by_format("oci").await.unwrap_or_default();
    repos.sort();
    (
        [(axum::http::header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({ "repositories": repos }).to_string(),
    )
        .into_response()
}

async fn list_tags(adapter: &Arc<OciAdapter>, name: &str, raw: RawQuery) -> Response {
    let (registry, repo) = split_registry(name);
    let mut tags = adapter.state.meta.list_versions("oci", &repo).await.unwrap_or_default();
    // Pull-through: overlay upstream tags so un-cached tags are still listed.
    if let Some(up) = adapter.upstream_for_registry(&registry).await {
        if let Ok(utags) = up.list_tags(&repo).await {
            let seen: std::collections::HashSet<String> = tags.iter().cloned().collect();
            for t in utags {
                if !seen.contains(&t) {
                    tags.push(t);
                }
            }
            tags.sort();
            tags.dedup();
        }
    }
    // last= pagination (lexicographic, spec form).
    if let Some(q) = raw.0 {
        let mut n: Option<usize> = None;
        let mut last: Option<String> = None;
        for pair in q.split('&') {
            let mut it = pair.splitn(2, '=');
            let k = it.next().unwrap_or("");
            let v = it.next().unwrap_or("");
            match k {
                "last" if !v.is_empty() => last = Some(v.to_string()),
                "n" => n = v.parse().ok(),
                _ => {}
            }
        }
        if let Some(last) = last {
            if let Some(pos) = tags.iter().position(|t| *t == last) {
                tags.drain(..=pos);
            } else {
                tags.retain(|t| t.as_str() > last.as_str());
            }
        }
        if let Some(n) = n {
            tags.truncate(n);
        }
    }
    (
        [(axum::http::header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({ "name": name, "tags": tags }).to_string(),
    )
        .into_response()
}

async fn list_referrers(
    adapter: &Arc<OciAdapter>,
    name: &str,
    subject: &str,
    raw: RawQuery,
) -> Response {
    let mut artifact_type_filter = String::new();
    if let Some(q) = raw.0 {
        for pair in q.split('&') {
            let mut it = pair.splitn(2, '=');
            let k = it.next().unwrap_or("");
            let v = it.next().unwrap_or("");
            if k == "artifactType" {
                artifact_type_filter = v.to_string();
            }
        }
    }

    let mut manifests: Vec<serde_json::Value> = Vec::new();
    let Ok(versions) = adapter.state.meta.list_versions("oci", name).await else {
        return empty_referrers(&artifact_type_filter);
    };
    for v in versions {
        let Ok(digest_hex) = parse_digest(&v) else {
            continue; // tags are not referrers
        };
        let Ok(art) = adapter.state.meta.get("oci", name, &v).await else {
            continue;
        };
        let Some(at) = manifest_artifact_type(&art.proprietary) else {
            continue;
        };
        if manifest_subject(&art.proprietary).as_deref() != Some(subject) {
            continue;
        }
        if !artifact_type_filter.is_empty() && at != artifact_type_filter {
            continue;
        }
        manifests.push(serde_json::json!({
            "mediaType": art.media_type,
            "digest": format!("sha256:{digest_hex}"),
            "size": art.proprietary.len(),
            "artifactType": at,
            "annotations": {},
        }));
    }

    let mut resp_headers =
        [(axum::http::header::CONTENT_TYPE, "application/vnd.oci.image.index.v1+json".to_string())];
    let mut body = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": manifests,
    });
    if !artifact_type_filter.is_empty() {
        resp_headers[0].1 = "application/vnd.oci.image.index.v1+json".into();
        body["annotations"] = serde_json::json!({});
    }
    let mut r = (StatusCode::OK, resp_headers, body.to_string()).into_response();
    if !artifact_type_filter.is_empty() {
        r.headers_mut().insert(
            axum::http::header::HeaderName::from_static("oci-filters-applied"),
            axum::http::HeaderValue::from_static("artifactType"),
        );
    }
    r
}

fn empty_referrers(_filter: &str) -> Response {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/vnd.oci.image.index.v1+json".to_string())],
        serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [],
        })
        .to_string(),
    )
        .into_response()
}

async fn get_manifest(
    adapter: &Arc<OciAdapter>,
    name: &str,
    reference: &str,
    write_body: bool,
) -> Response {
    // A reference containing ':' must parse as a digest.
    if reference.contains(':') && parse_digest(reference).is_err() {
        return OciAdapter::error(
            StatusCode::BAD_REQUEST,
            "DIGEST_INVALID",
            "invalid digest reference",
        );
    }
    let (registry, repo) = split_registry(name);

    match local_manifest(adapter, &repo, reference).await {
        Ok((body, media_type, dgst)) => manifest_ok(body, &media_type, &dgst, write_body),
        Err(_) => {
            // Pull-through.
            let Some(up) = adapter.upstream_for_registry(&registry).await else {
                return OciAdapter::not_found();
            };
            match up.get_manifest(&repo, reference).await {
                Ok((body, media_type)) => {
                    let dgst = sha256_digest(&body);
                    let art = Artifact {
                        format: "oci".into(),
                        repository: repo.clone(),
                        version: reference.to_string(),
                        media_type: media_type.clone(),
                        proprietary: body.clone(),
                        digest: dgst.clone(),
                        blobs: extract_blobs(&body),
                        source: "pull".into(),
                    };
                    let _ = adapter.state.meta.put(art).await;
                    let digest_art = Artifact {
                        format: "oci".into(),
                        repository: repo.clone(),
                        version: dgst.clone(),
                        media_type: media_type.clone(),
                        proprietary: body.clone(),
                        digest: dgst.clone(),
                        blobs: extract_blobs(&body),
                        source: "pull".into(),
                    };
                    let _ = adapter.state.meta.put(digest_art).await;
                    manifest_ok(body, &media_type, &dgst, write_body)
                }
                Err(RegistryError::UpstreamStatus { status, .. })
                    if status == 404 || status == 403 || status == 401 =>
                {
                    OciAdapter::not_found()
                }
                Err(e) => {
                    OciAdapter::error(StatusCode::BAD_GATEWAY, "UPSTREAM_ERROR", &e.to_string())
                }
            }
        }
    }
}

/// Resolve name+reference (tag or digest) into manifest bytes from the local
/// store.
async fn local_manifest(
    adapter: &Arc<OciAdapter>,
    repo: &str,
    reference: &str,
) -> Result<(Vec<u8>, String, String), RegistryError> {
    let art = adapter.state.meta.get("oci", repo, reference).await?;
    if art.proprietary.is_empty() {
        return Err(RegistryError::Other("missing manifest body".into()));
    }
    // Prefer the authoritative digest recorded at publish time (sha512 ok).
    if !art.digest.is_empty() && parse_digest(&art.digest).is_ok() {
        return Ok((art.proprietary, art.media_type, art.digest));
    }
    let sha = sha256_digest(&art.proprietary);
    Ok((art.proprietary, art.media_type, sha))
}

fn manifest_ok(body: Vec<u8>, media_type: &str, dgst: &str, write_body: bool) -> Response {
    let mut headers = [
        (axum::http::header::CONTENT_TYPE, media_type.to_string()),
        (axum::http::header::HeaderName::from_static("docker-content-digest"), dgst.to_string()),
        (axum::http::header::CONTENT_LENGTH, body.len().to_string()),
    ];
    let _ = &mut headers;
    if write_body {
        (headers, body).into_response()
    } else {
        // HEAD: no body.
        let mut resp = headers.into_response();
        *resp.body_mut() = Body::empty();
        resp
    }
}

async fn put_manifest(
    adapter: &Arc<OciAdapter>,
    name: &str,
    reference: &str,
    headers: HeaderMap,
    raw: RawQuery,
    body: Body,
) -> Response {
    let media_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/vnd.oci.image.manifest.v1+json")
        .to_string();

    let body = body;
    let bytes =
        match http_body_util::BodyExt::collect(http_body_util::Limited::new(body, 4 << 20)).await {
            Ok(b) => b.to_bytes(),
            Err(_) => {
                return OciAdapter::error(
                    StatusCode::BAD_REQUEST,
                    "MANIFEST_INVALID",
                    "error reading manifest",
                )
            }
        };
    let bytes = bytes.to_vec();
    let sha = sha256_digest(&bytes);

    // Validate the reference.
    let mut effective_digest = sha.clone();
    if reference.contains(':') {
        match parse_digest(reference) {
            Ok(_hexpart) => {
                // Recompute under the reference's algorithm.
                let algo = reference.split_once(':').map(|x| x.0).unwrap_or("sha256");
                if algo != "sha256" {
                    let d = match algo {
                        "sha512" => {
                            use sha2::Digest as _;
                            let mut h = sha2::Sha512::new();
                            h.update(&bytes);
                            format!("sha512:{}", hex::encode(h.finalize()))
                        }
                        _ => {
                            return OciAdapter::error(
                                StatusCode::BAD_REQUEST,
                                "DIGEST_INVALID",
                                "unsupported digest algorithm",
                            )
                        }
                    };
                    effective_digest = d;
                }
                if effective_digest != reference {
                    return OciAdapter::error(
                        StatusCode::BAD_REQUEST,
                        "MANIFEST_INVALID",
                        "manifest digest does not match content",
                    );
                }
            }
            Err(_) => {
                return OciAdapter::error(
                    StatusCode::BAD_REQUEST,
                    "DIGEST_INVALID",
                    "invalid digest reference",
                )
            }
        }
    }

    let blobs = extract_blobs(&bytes);
    let art = Artifact {
        format: "oci".into(),
        repository: name.to_string(),
        version: reference.to_string(),
        media_type: media_type.clone(),
        proprietary: bytes.clone(),
        blobs: blobs.clone(),
        digest: effective_digest.clone(),
        source: "push".into(),
    };
    if let Err(e) = adapter.state.meta.put(art).await {
        return OciAdapter::error(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string());
    }
    if effective_digest != reference {
        let digest_art = Artifact {
            format: "oci".into(),
            repository: name.to_string(),
            version: effective_digest.clone(),
            media_type: media_type.clone(),
            proprietary: bytes.clone(),
            blobs: blobs.clone(),
            digest: effective_digest.clone(),
            source: "push".into(),
        };
        if let Err(e) = adapter.state.meta.put(digest_art).await {
            return OciAdapter::error(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string());
        }
    }

    // OCI 1.1: PUT by digest may carry one or more ?tag= values.
    let mut valid_tags: Vec<String> = Vec::new();
    if let Some(q) = raw.0 {
        for pair in q.split('&') {
            let mut it = pair.splitn(2, '=');
            if it.next() == Some("tag") {
                if let Ok(t) = percent_decode(it.next().unwrap_or("")) {
                    if !t.is_empty() {
                        let tag_art = Artifact {
                            format: "oci".into(),
                            repository: name.to_string(),
                            version: t.clone(),
                            media_type: media_type.clone(),
                            proprietary: bytes.clone(),
                            blobs: blobs.clone(),
                            digest: effective_digest.clone(),
                            source: "push".into(),
                        };
                        if let Err(e) = adapter.state.meta.put(tag_art).await {
                            return OciAdapter::error(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "UNKNOWN",
                                &e.to_string(),
                            );
                        }
                        valid_tags.push(t);
                    }
                }
            }
        }
    }

    let mut builder = Response::builder().status(StatusCode::CREATED);
    {
        let hm = builder.headers_mut().expect("response builder headers");
        hm.insert(
            axum::http::header::HeaderName::from_static("docker-content-digest"),
            axum::http::HeaderValue::from_str(&effective_digest)
                .expect("digest is a valid header value"),
        );
        hm.insert(
            axum::http::header::LOCATION,
            axum::http::HeaderValue::from_str(&format!("/v2/{name}/manifests/{effective_digest}"))
                .expect("manifest location is a valid header value"),
        );
        if !valid_tags.is_empty() {
            hm.insert(
                axum::http::header::HeaderName::from_static("oci-tag"),
                axum::http::HeaderValue::from_str(&valid_tags.join(","))
                    .expect("tags are a valid header value"),
            );
        }
        if let Some(subject) = manifest_subject(&bytes) {
            hm.insert(
                axum::http::header::HeaderName::from_static("oci-subject"),
                axum::http::HeaderValue::from_str(&subject)
                    .expect("subject is a valid header value"),
            );
        }
    }
    builder.body(Body::empty()).unwrap_or_else(|_| StatusCode::CREATED.into_response())
}

fn percent_decode(s: &str) -> Result<String, RegistryError> {
    percent_encoding::percent_decode_str(s)
        .decode_utf8()
        .map(|c| c.to_string())
        .map_err(|e| RegistryError::Other(e.to_string()))
}

async fn delete_manifest(adapter: &Arc<OciAdapter>, name: &str, reference: &str) -> Response {
    match adapter.state.meta.delete("oci", name, reference).await {
        Ok(_) => StatusCode::ACCEPTED.into_response(),
        Err(e) if e.is_unknown() => OciAdapter::not_found(),
        Err(e) => OciAdapter::error(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string()),
    }
}

// --- blobs -----------------------------------------------------------------

async fn check_blob(adapter: &Arc<OciAdapter>, digest: &str) -> Response {
    if parse_digest(digest).is_err() {
        return OciAdapter::error(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "invalid digest");
    }
    match adapter.state.blobs.stat(digest).await {
        Ok(Some(size)) => (
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_LENGTH, size.to_string()),
                (
                    axum::http::header::HeaderName::from_static("docker-content-digest"),
                    digest.to_string(),
                ),
            ],
        )
            .into_response(),
        Ok(None) => {
            OciAdapter::error(StatusCode::NOT_FOUND, "BLOB_UNKNOWN", "blob unknown to registry")
        }
        Err(e) => OciAdapter::error(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string()),
    }
}

async fn get_blob(
    adapter: &Arc<OciAdapter>,
    name: &str,
    digest: &str,
    headers: &HeaderMap,
) -> Response {
    if parse_digest(digest).is_err() {
        return OciAdapter::error(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "invalid digest");
    }
    // Local hit: serve with Range support.
    if let Ok(Some(size)) = adapter.state.blobs.stat(digest).await {
        return serve_local_blob(adapter, digest, size, headers).await;
    }
    // Pull-through: stream from upstream, caching locally.
    let (registry, repo) = split_registry(name);
    let Some(up) = adapter.upstream_for_registry(&registry).await else {
        return OciAdapter::error(
            StatusCode::NOT_FOUND,
            "BLOB_UNKNOWN",
            "blob unknown to registry",
        );
    };
    match up.get_blob(&repo, digest).await {
        Ok((resp, len)) => stream_and_cache(adapter, resp, len, digest).await,
        Err(RegistryError::UpstreamStatus { status, .. }) if status == 404 || status == 403 => {
            OciAdapter::error(StatusCode::NOT_FOUND, "BLOB_UNKNOWN", "blob unknown to registry")
        }
        Err(e) => OciAdapter::error(StatusCode::BAD_GATEWAY, "UPSTREAM_ERROR", &e.to_string()),
    }
}

async fn serve_local_blob(
    _adapter: &Arc<OciAdapter>,
    digest: &str,
    size: u64,
    headers: &HeaderMap,
) -> Response {
    // Range handling requires a reopen-able file; stat/open again.
    match _adapter.state.blobs.open(digest).await {
        Ok(Some(mut reader)) => {
            let mut base_headers: Vec<(axum::http::header::HeaderName, String)> = vec![
                (
                    axum::http::header::HeaderName::from_static("docker-content-digest"),
                    digest.to_string(),
                ),
                (axum::http::header::CONTENT_TYPE, "application/octet-stream".to_string()),
                (axum::http::header::ACCEPT_RANGES, "bytes".to_string()),
            ];
            if let Some(range) =
                headers.get(axum::http::header::RANGE).and_then(|v| v.to_str().ok())
            {
                match parse_range(range, size) {
                    Ok((start, end)) => {
                        if reader.seek_to(start).is_err() {
                            return OciAdapter::error(
                                StatusCode::RANGE_NOT_SATISFIABLE,
                                "RANGE_INVALID",
                                "seek failed",
                            );
                        }
                        let length = end - start + 1;
                        base_headers.push((
                            axum::http::header::CONTENT_RANGE,
                            format!("bytes {start}-{end}/{size}"),
                        ));
                        base_headers.push((axum::http::header::CONTENT_LENGTH, length.to_string()));
                        let stream = read_exact_stream(reader, length);
                        return headers_body_response(
                            StatusCode::PARTIAL_CONTENT,
                            base_headers,
                            stream,
                        );
                    }
                    Err(_) => {
                        return OciAdapter::error(
                            StatusCode::RANGE_NOT_SATISFIABLE,
                            "RANGE_INVALID",
                            "invalid range",
                        )
                    }
                }
            }
            base_headers.push((axum::http::header::CONTENT_LENGTH, size.to_string()));
            let stream = read_all_stream(reader);
            headers_body_response(StatusCode::OK, base_headers, stream)
        }
        Ok(None) => {
            OciAdapter::error(StatusCode::NOT_FOUND, "BLOB_UNKNOWN", "blob unknown to registry")
        }
        Err(e) => OciAdapter::error(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string()),
    }
}

/// Build a response from a status, (name,value) headers and a streaming body.
fn headers_body_response(
    status: StatusCode,
    headers: Vec<(axum::http::header::HeaderName, String)>,
    body: Body,
) -> Response {
    let mut builder = Response::builder().status(status);
    {
        let hm = builder.headers_mut().expect("response builder headers");
        for (k, v) in headers {
            if let Ok(val) = axum::http::HeaderValue::from_str(&v) {
                hm.insert(k, val);
            }
        }
    }
    builder.body(body).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Wrap a BlobReader into a byte stream limited to `len` bytes.
fn read_exact_stream(mut reader: Box<dyn BlobReader>, len: u64) -> Body {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(4);
    tokio::spawn(async move {
        let mut remaining = len;
        let mut buf = vec![0u8; 64 * 1024];
        while remaining > 0 {
            let want = buf.len().min(remaining as usize);
            // Blocking read on a reader; fine for file-backed blobs.
            let n = {
                let slice = &mut buf[..want];
                std::io::Read::read(&mut reader, slice).unwrap_or(0)
            };
            if n == 0 {
                break;
            }
            remaining -= n as u64;
            if tx.send(Ok(bytes::Bytes::copy_from_slice(&buf[..n]))).await.is_err() {
                break;
            }
        }
    });
    Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx))
}

fn read_all_stream(mut reader: Box<dyn BlobReader>) -> Body {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(4);
    tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = {
                let slice = &mut buf[..];
                std::io::Read::read(&mut reader, slice).unwrap_or(0)
            };
            if n == 0 {
                break;
            }
            if tx.send(Ok(bytes::Bytes::copy_from_slice(&buf[..n]))).await.is_err() {
                break;
            }
        }
    });
    Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx))
}

/// Stream an upstream blob response to the client while verifying + caching
/// it locally (CacheThrough semantics: the client is served even when the
/// digest does not match; nothing is cached on mismatch).
async fn stream_and_cache(
    adapter: &Arc<OciAdapter>,
    resp: reqwest::Response,
    len: Option<u64>,
    digest: &str,
) -> Response {
    let mut headers: Vec<(axum::http::header::HeaderName, String)> = vec![(
        axum::http::header::HeaderName::from_static("docker-content-digest"),
        digest.to_string(),
    )];
    if let Some(l) = len {
        headers.push((axum::http::header::CONTENT_LENGTH, l.to_string()));
    }
    let _ = headers;

    // Buffer through a temp file so we can verify before storing.
    let tmp = std::env::temp_dir().join(format!(
        "pkglab-cache-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let tmp2 = tmp.clone();
    let digest_owned = digest.to_string();
    let adapter2 = adapter.clone();

    let stream = resp.bytes_stream();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(4);
    use sha2::Digest as _;

    tokio::spawn(async move {
        let mut file = match tokio::fs::File::create(&tmp2).await {
            Ok(f) => f,
            Err(_) => return,
        };
        let mut hasher = sha2::Sha256::new();
        use futures::StreamExt;
        use tokio::io::AsyncWriteExt;
        let mut stream = stream;
        while let Some(chunk) = stream.next().await {
            let Ok(bytes) = chunk else { break };
            hasher.update(&bytes);
            let _ = file.write_all(&bytes).await;
            if tx.send(Ok(bytes)).await.is_err() {
                break;
            }
        }
        let _ = file.flush().await;
        let got = format!("sha256:{}", hex::encode(hasher.finalize()));
        if got == digest_owned {
            // Commit into the CAS (dedup handled inside).
            if let Ok(data) = tokio::fs::read(&tmp2).await {
                let mut cursor = std::io::Cursor::new(data);
                let _ = adapter2.state.blobs.put_if_absent(&digest_owned, &mut cursor).await;
            }
        }
        let _ = tokio::fs::remove_file(&tmp2).await;
    });

    let mut resp_builder = Response::builder().status(StatusCode::OK);
    if let Some(l) = len {
        resp_builder = resp_builder.header(axum::http::header::CONTENT_LENGTH, l);
    }
    resp_builder
        .header(axum::http::header::CONTENT_TYPE, "application/octet-stream")
        .header(axum::http::header::HeaderName::from_static("docker-content-digest"), digest)
        .body(Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn delete_blob(adapter: &Arc<OciAdapter>, digest: &str) -> Response {
    if parse_digest(digest).is_err() {
        return OciAdapter::error(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "invalid digest");
    }
    match adapter.state.blobs.delete(digest).await {
        Ok(_) => (
            StatusCode::ACCEPTED,
            [(
                axum::http::header::HeaderName::from_static("docker-content-digest"),
                digest.to_string(),
            )],
        )
            .into_response(),
        Err(e) => OciAdapter::error(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string()),
    }
}

// --- blob upload sessions ---------------------------------------------------

async fn uploads_dispatch(
    adapter: &Arc<OciAdapter>,
    name: &str,
    session: &str,
    method: Method,
    raw: RawQuery,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let query = parse_query(&raw.0.unwrap_or_default());

    match method {
        Method::POST => {
            // Mount: POST .../uploads/?mount={digest}&from={repo}
            if let Some(mount) = query.get("mount") {
                if parse_digest(mount).is_ok()
                    && matches!(adapter.state.blobs.stat(mount).await, Ok(Some(_)))
                {
                    return (
                        StatusCode::CREATED,
                        [
                            (axum::http::header::LOCATION, format!("/v2/{name}/blobs/{mount}")),
                            (
                                axum::http::header::HeaderName::from_static(
                                    "docker-content-digest",
                                ),
                                mount.to_string(),
                            ),
                        ],
                    )
                        .into_response();
                }
            }
            // Single-shot with ?digest=.
            if let Some(dgst) = query.get("digest") {
                if parse_digest(dgst).is_err() {
                    return OciAdapter::error(
                        StatusCode::BAD_REQUEST,
                        "DIGEST_INVALID",
                        "digest parameter is invalid",
                    );
                }
                let mut stream_body = body;
                match http_body_util::BodyExt::collect(&mut stream_body).await {
                    Ok(collected) => {
                        let data = collected.to_bytes().to_vec();
                        match adapter.state.blobs.put_if_absent(dgst, &mut data.as_slice()).await {
                            Ok(_) => (
                                StatusCode::CREATED,
                                [
                                    (
                                        axum::http::header::LOCATION,
                                        format!("/v2/{name}/blobs/{dgst}"),
                                    ),
                                    (
                                        axum::http::header::HeaderName::from_static(
                                            "docker-content-digest",
                                        ),
                                        dgst.to_string(),
                                    ),
                                ],
                            )
                                .into_response(),
                            Err(e) => OciAdapter::error(
                                StatusCode::BAD_REQUEST,
                                "DIGEST_INVALID",
                                &e.to_string(),
                            ),
                        }
                    }
                    Err(_) => {
                        OciAdapter::error(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "read error")
                    }
                }
            } else {
                // Start a session.
                match adapter.uploads.start(name).await {
                    Ok(sess) => (
                        StatusCode::ACCEPTED,
                        [
                            (
                                axum::http::header::LOCATION,
                                format!("/v2/{name}/blobs/uploads/{}", sess.id),
                            ),
                            (
                                axum::http::header::HeaderName::from_static("docker-upload-uuid"),
                                sess.id.clone(),
                            ),
                        ],
                    )
                        .into_response(),
                    Err(e) => OciAdapter::error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "UNKNOWN",
                        &e.to_string(),
                    ),
                }
            }
        }
        Method::GET => {
            if session.is_empty() {
                return OciAdapter::error(
                    StatusCode::NOT_FOUND,
                    "BLOB_UPLOAD_UNKNOWN",
                    "blob upload unknown to registry",
                );
            }
            match adapter.uploads.get(session) {
                Some(sess) => (
                    StatusCode::NO_CONTENT,
                    [
                        (
                            axum::http::header::HeaderName::from_static("docker-upload-uuid"),
                            sess.id.clone(),
                        ),
                        (axum::http::header::RANGE, format!("0-{}", sess.size().saturating_sub(1))),
                    ],
                )
                    .into_response(),
                None => OciAdapter::error(
                    StatusCode::NOT_FOUND,
                    "BLOB_UPLOAD_UNKNOWN",
                    "blob upload unknown to registry",
                ),
            }
        }
        Method::PATCH => {
            if session.is_empty() {
                return OciAdapter::error(
                    StatusCode::NOT_FOUND,
                    "BLOB_UPLOAD_UNKNOWN",
                    "blob upload unknown to registry",
                );
            }
            let expect_start = headers
                .get(axum::http::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| parse_content_range_start(v).ok());
            match adapter.uploads.patch(session, expect_start, body.into_data_reader()).await {
                Ok(total) => (
                    StatusCode::ACCEPTED,
                    [
                        (
                            axum::http::header::HeaderName::from_static("docker-upload-uuid"),
                            session.to_string(),
                        ),
                        (axum::http::header::RANGE, format!("0-{}", total - 1)),
                        (
                            axum::http::header::LOCATION,
                            format!("/v2/{name}/blobs/uploads/{session}"),
                        ),
                    ],
                )
                    .into_response(),
                Err(RegistryError::Other(msg)) if msg.contains("out of order") => {
                    OciAdapter::error(StatusCode::RANGE_NOT_SATISFIABLE, "RANGE_INVALID", &msg)
                }
                Err(e) => {
                    OciAdapter::error(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string())
                }
            }
        }
        Method::PUT => {
            if session.is_empty() {
                return OciAdapter::error(
                    StatusCode::NOT_FOUND,
                    "BLOB_UPLOAD_UNKNOWN",
                    "blob upload unknown to registry",
                );
            }
            let expect_start = headers
                .get(axum::http::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| parse_content_range_start(v).ok());
            if let Some(s) = expect_start {
                match adapter.uploads.get(session) {
                    Some(sess) if s != sess.size() => {
                        return OciAdapter::error(
                            StatusCode::RANGE_NOT_SATISFIABLE,
                            "RANGE_INVALID",
                            "out of order chunk",
                        )
                    }
                    None => {
                        return OciAdapter::error(
                            StatusCode::NOT_FOUND,
                            "BLOB_UPLOAD_UNKNOWN",
                            "blob upload unknown to registry",
                        )
                    }
                    _ => {}
                }
            }
            let Some(dgst) = query.get("digest") else {
                return OciAdapter::error(
                    StatusCode::BAD_REQUEST,
                    "DIGEST_INVALID",
                    "digest parameter missing or invalid",
                );
            };
            // The final PUT may carry the last chunk (with Content-Range) or
            // the whole body (no Content-Range). Either way append it once.
            if let Err(e) =
                adapter.uploads.patch(session, expect_start, body.into_data_reader()).await
            {
                if expect_start.is_some() {
                    return OciAdapter::error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "UNKNOWN",
                        &e.to_string(),
                    );
                }
                // No Content-Range and patch failed with out-of-order guard:
                // ignore (body may be empty).
            }
            match adapter.uploads.commit(session, dgst).await {
                Ok(_) => (
                    StatusCode::CREATED,
                    [
                        (axum::http::header::LOCATION, format!("/v2/{name}/blobs/{dgst}")),
                        (
                            axum::http::header::HeaderName::from_static("docker-content-digest"),
                            dgst.to_string(),
                        ),
                    ],
                )
                    .into_response(),
                Err(e) => {
                    OciAdapter::error(StatusCode::BAD_REQUEST, "DIGEST_INVALID", &e.to_string())
                }
            }
        }
        Method::DELETE => {
            if session.is_empty() {
                return OciAdapter::error(
                    StatusCode::NOT_FOUND,
                    "BLOB_UPLOAD_UNKNOWN",
                    "blob upload unknown to registry",
                );
            }
            if adapter.uploads.get(session).is_none() {
                return OciAdapter::error(
                    StatusCode::NOT_FOUND,
                    "BLOB_UPLOAD_UNKNOWN",
                    "blob upload unknown to registry",
                );
            }
            adapter.uploads.cancel(session).await;
            StatusCode::NO_CONTENT.into_response()
        }
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

fn parse_query(q: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in q.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next().unwrap_or("");
        let v = it.next().unwrap_or("");
        if let Ok(decoded) = percent_decode(v) {
            out.insert(k.to_string(), decoded);
        }
    }
    out
}

// Extension trait to read an axum Body as an AsyncRead.
trait BodyReadExt {
    fn into_data_reader(self) -> BodyReader;
}

impl BodyReadExt for Body {
    fn into_data_reader(self) -> BodyReader {
        BodyReader { body: self, buf: bytes::Bytes::new() }
    }
}

struct BodyReader {
    body: Body,
    buf: bytes::Bytes,
}

impl tokio::io::AsyncRead for BodyReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        out: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        use axum::body::HttpBody as _;
        loop {
            if !self.buf.is_empty() {
                let n = out.remaining().min(self.buf.len());
                out.put_slice(&self.buf[..n]);
                use bytes::Buf as _;
                self.buf.advance(n);
                return std::task::Poll::Ready(Ok(()));
            }
            match std::pin::Pin::new(&mut self.body).poll_frame(cx) {
                std::task::Poll::Ready(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        self.buf = data;
                    }
                }
                std::task::Poll::Ready(Some(Err(e))) => {
                    return std::task::Poll::Ready(Err(std::io::Error::other(e)))
                }
                std::task::Poll::Ready(None) => return std::task::Poll::Ready(Ok(())),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests_helpers {
    use super::*;

    #[test]
    fn percent_decode_handles() {
        assert_eq!(percent_decode("a%2Fb").unwrap(), "a/b");
        assert_eq!(percent_decode("plain").unwrap(), "plain");
        // `%zz` is not valid percent-encoding; percent_decode_str leaves it
        // verbatim, so it decodes to a literal "%zz".
        assert_eq!(percent_decode("%zz").unwrap(), "%zz");
    }

    #[test]
    fn parse_query_decodes_values_only() {
        let m = parse_query("tag=v1%2E0&n=slice%20with%20space");
        assert_eq!(m.get("tag").unwrap(), "v1.0");
        assert_eq!(m.get("n").unwrap(), "slice with space");
    }

    #[test]
    fn manifest_ok_head_vs_get() {
        // GET carries the body; HEAD carries headers only.
        let get = manifest_ok(
            b"{}".to_vec(),
            "application/vnd.oci.image.manifest.v1+json",
            "sha256:x",
            true,
        );
        let head = manifest_ok(
            b"{}".to_vec(),
            "application/vnd.oci.image.manifest.v1+json",
            "sha256:x",
            false,
        );
        assert!(get.headers().get("docker-content-digest").is_some());
        assert!(head.headers().get("docker-content-digest").is_some());
    }
}
