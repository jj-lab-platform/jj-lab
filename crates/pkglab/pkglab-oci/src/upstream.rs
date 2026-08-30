//! Remote OCI registry used as a pull-through source (port of Go
//! `core/proxy.go`): transparent bearer-token acquisition on 401/403 via the
//! `WWW-Authenticate` challenge, per-scope token caching, manifest/blob/tag
//! access.

use pkglab_common::registry::{RegistryError, Result};
use pkglab_common::remote::{status_error, ClientFactory};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const ACCEPT_MANIFESTS: &str = "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.index.v1+json, application/vnd.oci.artifact.manifest.v1+json";

#[derive(Debug, Clone)]
pub struct UpstreamStatusError {
    pub status: u16,
}

impl From<UpstreamStatusError> for RegistryError {
    fn from(e: UpstreamStatusError) -> Self {
        RegistryError::UpstreamStatus { path: String::new(), status: e.status }
    }
}

impl UpstreamStatusError {
    /// A manifest miss upstream: 404/403/401 (Docker Hub famously answers 401
    /// for private or nonexistent repositories even with a valid anon token).
    pub fn is_manifest_miss(&self) -> bool {
        matches!(self.status, 404 | 403 | 401)
    }
    pub fn is_not_found(&self) -> bool {
        self.status == 404 || self.status == 403
    }
}

/// Scheme+host of an upstream registry; https default, http for localhost/IP.
#[derive(Clone)]
pub struct Upstream {
    scheme: String,
    host: String,
    factory: ClientFactory,
    proxy: Option<String>,
    tokens: Arc<Mutex<HashMap<String, String>>>,
}

impl Upstream {
    /// Parse a host string with or without a scheme.
    pub fn new(factory: &ClientFactory, host: &str, proxy: Option<String>) -> Self {
        let mut scheme = "https".to_string();
        let mut h = host.to_string();
        for p in ["https://", "http://"] {
            if let Some(rest) = host.strip_prefix(p) {
                scheme = p[..p.len() - 3].to_string();
                h = rest.to_string();
                break;
            }
        }
        Self {
            scheme,
            host: h,
            factory: factory.clone(),
            proxy,
            tokens: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Build an upstream for an explicitly prefixed registry host. https is
    /// assumed except for localhost/IP literals (insecure-registry
    /// convention).
    pub fn for_registry(factory: &ClientFactory, host: &str, proxy: Option<String>) -> Self {
        // The host may itself embed a scheme already (rare); normalize.
        let mut h = host.to_string();
        let mut scheme_override: Option<String> = None;
        for p in ["https://", "http://"] {
            if let Some(rest) = host.strip_prefix(p) {
                scheme_override = Some(p[..p.len() - 3].to_string());
                h = rest.to_string();
                break;
            }
        }
        let ip_host = h.split_once(':').map(|x| x.0).unwrap_or(&h);
        let insecure = h == "localhost"
            || h.starts_with("localhost:")
            || ip_host.parse::<std::net::IpAddr>().is_ok();
        let scheme = scheme_override.unwrap_or_else(|| {
            if insecure {
                "http".to_string()
            } else {
                "https".to_string()
            }
        });
        Self {
            scheme,
            host: h,
            factory: factory.clone(),
            proxy,
            tokens: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn scheme(&self) -> &str {
        &self.scheme
    }
    pub fn host(&self) -> &str {
        &self.host
    }

    fn base(&self) -> String {
        format!("{}://{}/v2", self.scheme, self.host)
    }

    fn client(&self) -> reqwest::Client {
        self.factory.client(self.proxy.as_deref())
    }

    /// GET a path under /v2 with the given scope, transparently retrying once
    /// with a fresh scoped token on 401/403.
    async fn do_get(
        &self,
        path: &str,
        scope: &str,
        accept: Option<&str>,
    ) -> Result<reqwest::Response> {
        let url = format!("{}{}", self.base(), path);
        let mut req = self.client().get(&url);
        req = req.header("Accept", accept.unwrap_or(ACCEPT_MANIFESTS));
        if let Some(tok) = self.token_for(scope) {
            req = req.header("Authorization", format!("Bearer {tok}"));
        }
        let resp = req.send().await.map_err(|e| RegistryError::Http(e.to_string()))?;
        let status = resp.status();
        if status != reqwest::StatusCode::UNAUTHORIZED && status != reqwest::StatusCode::FORBIDDEN {
            return Ok(resp);
        }
        // Token rejected (or absent): fetch a fresh one for this scope and
        // retry exactly once. 403 is treated like 401 because registries that
        // bind tokens to a scope answer 403 for another repo's scope.
        let challenge = resp
            .headers()
            .get("WWW-Authenticate")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let token = self.fetch_token_for_challenge(&challenge, scope).await?;
        let mut req2 = self.client().get(&url);
        req2 = req2.header("Accept", accept.unwrap_or(ACCEPT_MANIFESTS));
        req2 = req2.header("Authorization", format!("Bearer {token}"));
        req2.send().await.map_err(|e| RegistryError::Http(e.to_string()))
    }

    fn token_for(&self, scope: &str) -> Option<String> {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner()).get(scope).cloned()
    }

    async fn fetch_token_for_challenge(&self, challenge: &str, scope: &str) -> Result<String> {
        if let Some(rest) = challenge.strip_prefix("Bearer ") {
            let tok = self.fetch_token(rest, scope).await?;
            self.tokens
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(scope.to_string(), tok.clone());
            return Ok(tok);
        }
        // No challenge: probe /v2/ to discover it.
        let url = format!("{}/", self.base());
        let resp =
            self.client().get(&url).send().await.map_err(|e| RegistryError::Http(e.to_string()))?;
        let c = resp
            .headers()
            .get("WWW-Authenticate")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let Some(rest) = c.strip_prefix("Bearer ") else {
            return Err(RegistryError::Http(format!("unsupported challenge: {c:?}")));
        };
        let tok = self.fetch_token(rest, scope).await?;
        self.tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(scope.to_string(), tok.clone());
        Ok(tok)
    }

    async fn fetch_token(&self, challenge_params: &str, scope: &str) -> Result<String> {
        let params = parse_auth_params(challenge_params);
        let realm = params
            .get("realm")
            .ok_or_else(|| RegistryError::Http("challenge missing realm".into()))?;
        let mut url = reqwest::Url::parse(realm).map_err(|e| RegistryError::Http(e.to_string()))?;
        {
            let mut q = url.query_pairs_mut();
            if !scope.is_empty() {
                q.append_pair("scope", scope);
            }
            if let Some(s) = params.get("service") {
                q.append_pair("service", s);
            }
        }
        let resp =
            self.client().get(url).send().await.map_err(|e| RegistryError::Http(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(RegistryError::Http(format!("token request failed: {status}")));
        }
        let body = resp.bytes().await.map_err(|e| RegistryError::Http(e.to_string()))?;
        Ok(extract_token(&body))
    }

    /// Fetch a manifest by name+reference. Returns (body, content-type).
    pub async fn get_manifest(&self, name: &str, reference: &str) -> Result<(Vec<u8>, String)> {
        let scope = format!("repository:{name}:pull");
        let path = format!("/{name}/manifests/{reference}");
        let resp = self.do_get(&path, &scope, None).await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(UpstreamStatusError { status: status.as_u16() }.into());
        }
        let ct = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/vnd.oci.image.manifest.v1+json")
            .to_string();
        let body = resp.bytes().await.map_err(|e| RegistryError::Http(e.to_string()))?.to_vec();
        Ok((body, ct))
    }

    /// Stream a blob by digest. Returns (stream, content-length).
    pub async fn get_blob(
        &self,
        name: &str,
        digest: &str,
    ) -> Result<(reqwest::Response, Option<u64>)> {
        let scope = format!("repository:{name}:pull");
        let path = format!("/{name}/blobs/{digest}");
        let resp = self.do_get(&path, &scope, None).await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(UpstreamStatusError { status: status.as_u16() }.into());
        }
        let len = resp.content_length();
        Ok((resp, len))
    }

    /// Fetch the tag list for a repository.
    pub async fn list_tags(&self, name: &str) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Tags {
            #[serde(default)]
            tags: Vec<String>,
        }
        let scope = format!("repository:{name}:pull");
        let path = format!("/{name}/tags/list");
        let resp = self.do_get(&path, &scope, None).await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(UpstreamStatusError { status: status.as_u16() }.into());
        }
        let limit = resp.content_length().unwrap_or(4 * 1024 * 1024).min(32 << 20);
        let body = read_limited(resp, limit).await?;
        let t: Tags =
            serde_json::from_slice(&body).map_err(|e| RegistryError::Http(e.to_string()))?;
        Ok(t.tags)
    }
}

/// Read a response body with a byte cap.
async fn read_limited(resp: reqwest::Response, cap: u64) -> Result<bytes::Bytes> {
    let body = resp.bytes().await.map_err(|e| RegistryError::Http(e.to_string()))?;
    if body.len() as u64 > cap {
        return Err(RegistryError::Http("upstream body too large".into()));
    }
    Ok(body)
}

/// Tolerant token extraction: registries emit `token` and/or `access_token`
/// (sometimes both). Take the first non-empty without strict unmarshalling.
fn extract_token(body: &[u8]) -> String {
    let v: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
    for key in ["token", "access_token"] {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    String::new()
}

pub fn parse_auth_params(s: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for part in s.split(',') {
        let part = part.trim();
        if let Some((k, v)) = part.split_once('=') {
            out.insert(k.trim().to_string(), v.trim_matches('"').to_string());
        }
    }
    out
}

/// Keep status_error referenced (used by adapters for error mapping).
#[allow(dead_code)]
fn _use(status: reqwest::StatusCode, path: &str) -> RegistryError {
    status_error(path, status)
}
