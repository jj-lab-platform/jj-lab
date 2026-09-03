//! Upstream HTTP client factory plus a small protocol-agnostic remote handle.
//! Mirrors the Go `core.Remote` semantics:
//! - one shared client for all upstream requests, following the ambient
//!   `HTTP(S)_PROXY` / `NO_PROXY` environment variables (no per-key override),
//! - no overall body timeout (large blobs stream for minutes),
//! - custom User-Agent (Maven Central 429s the default Go UA).
//!
//! The client is intentionally proxy-agnostic: proxy config lives solely in
//! the container environment, so a single cached client is shared everywhere.

use std::sync::{Arc, Mutex};
use std::time::Duration;

/// User-Agent sent on every upstream request.
pub const USER_AGENT: &str = "pkglab/1.0 (pull-through mirror)";

/// Build (and cache) a single reqwest client that follows the environment
/// proxy configuration (`HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`).
#[derive(Clone)]
pub struct ClientFactory {
    cache: Arc<Mutex<Option<reqwest::Client>>>,
}

impl Default for ClientFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientFactory {
    pub fn new() -> Self {
        Self { cache: Arc::new(Mutex::new(None)) }
    }

    pub fn client(&self) -> reqwest::Client {
        let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(client) = c.as_ref() {
            return client.clone();
        }
        // No explicit proxy()/no_proxy(): reqwest reads the environment proxy
        // configuration by default.
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(120))
            // No overall request timeout: bodies stream without a deadline.
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(20)
            .build()
            .unwrap_or_default();
        *c = Some(client.clone());
        client
    }
}

/// A remote upstream root URL. Each adapter maps its own URL layout onto
/// [`Remote::get`] / [`Remote::get_cached`].
#[derive(Clone)]
pub struct Remote {
    base: String,
    client: reqwest::Client,
    extra_headers: Vec<(String, String)>,
}

impl Remote {
    /// Create a remote. `base` is trimmed of trailing slashes. All requests go
    /// through the environment-configured proxy (see [`ClientFactory::client`]).
    pub fn new(factory: &ClientFactory, base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            client: factory.client(),
            extra_headers: Vec::new(),
        }
    }

    pub fn with_header(mut self, key: &str, value: &str) -> Self {
        self.extra_headers.push((key.to_string(), value.to_string()));
        self
    }

    /// Absolute URL for a path starting with `/`.
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// GET, returning the response. Non-2xx statuses are returned as-is; the
    /// caller (or [`status_error`]) interprets them.
    pub async fn get(&self, path: &str) -> reqwest::Result<reqwest::Response> {
        let mut req = self.client.get(self.url(path));
        for (k, v) in &self.extra_headers {
            req = req.header(k, v);
        }
        req.send().await
    }

    /// GET returning the body bytes; errors on non-2xx.
    pub async fn get_bytes(&self, path: &str) -> crate::registry::Result<Vec<u8>> {
        let resp = self.get(path).await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(crate::registry::RegistryError::UpstreamStatus {
                path: path.to_string(),
                status: status.as_u16(),
            });
        }
        Ok(resp.bytes().await?.to_vec())
    }

    /// PUT with a body and optional content type.
    pub async fn put(
        &self,
        path: &str,
        content_type: Option<&str>,
        body: Vec<u8>,
    ) -> reqwest::Result<reqwest::Response> {
        let mut req = self.client.put(self.url(path)).body(body);
        if let Some(ct) = content_type {
            req = req.header(reqwest::header::CONTENT_TYPE, ct);
        }
        for (k, v) in &self.extra_headers {
            req = req.header(k, v);
        }
        req.send().await
    }

    /// POST with a body and optional content type.
    pub async fn post(
        &self,
        path: &str,
        content_type: Option<&str>,
        body: Vec<u8>,
    ) -> reqwest::Result<reqwest::Response> {
        let mut req = self.client.post(self.url(path)).body(body);
        if let Some(ct) = content_type {
            req = req.header(reqwest::header::CONTENT_TYPE, ct);
        }
        for (k, v) in &self.extra_headers {
            req = req.header(k, v);
        }
        req.send().await
    }

    /// GET served through the shared TTL index cache, keyed by absolute URL.
    /// Intended for small index/metadata documents re-fetched on every client
    /// resolution.
    pub async fn get_cached(
        &self,
        cache: &crate::cache::MemCache,
        path: &str,
    ) -> crate::registry::Result<String> {
        let key = self.url(path);
        if let Some(body) = cache.get(&key) {
            return Ok(body);
        }
        let body = self.get_bytes(path).await?;
        let s = String::from_utf8_lossy(&body).to_string();
        cache.set(&key, s.clone());
        Ok(s)
    }
}

/// Map a response status into the shared upstream-status error.
pub fn status_error(path: &str, status: reqwest::StatusCode) -> crate::registry::RegistryError {
    crate::registry::RegistryError::UpstreamStatus {
        path: path.to_string(),
        status: status.as_u16(),
    }
}

/// True for 404/410: the artifact simply does not exist upstream.
pub fn is_not_found(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE
}
