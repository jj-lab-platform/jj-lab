//! Proxy-aware upstream HTTP client factory plus a small protocol-agnostic
//! remote handle. Mirrors the Go `core.Remote` semantics:
//! - one shared client per proxy policy (env / direct / explicit URL),
//! - no overall body timeout (large blobs stream for minutes),
//! - custom User-Agent (Maven Central 429s the default Go UA).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// User-Agent sent on every upstream request.
pub const USER_AGENT: &str = "pkglab/1.0 (pull-through mirror)";

/// Build (and cache) a reqwest client for a proxy policy.
///
/// * `None`            -> follow the environment proxy configuration
/// * `Some("")`        -> direct connection (environment proxy bypassed)
/// * `Some(url)`       -> always route through the given proxy
#[derive(Clone)]
pub struct ClientFactory {
    cache: Arc<Mutex<HashMap<String, reqwest::Client>>>,
}

impl Default for ClientFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientFactory {
    pub fn new() -> Self {
        Self { cache: Arc::new(Mutex::new(HashMap::new())) }
    }

    pub fn client(&self, proxy: Option<&str>) -> reqwest::Client {
        let key = proxy.unwrap_or("__env__").to_string();
        if let Some(c) = self.cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return c.clone();
        }
        let mut builder = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(120))
            // No overall request timeout: bodies stream without a deadline.
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(20);
        match proxy {
            None => {}
            Some("") => builder = builder.no_proxy(),
            Some(u) => {
                if let Ok(pu) = reqwest::Proxy::all(u) {
                    builder = builder.proxy(pu);
                } else {
                    builder = builder.no_proxy();
                }
            }
        }
        let client = builder.build().unwrap_or_default();
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).insert(key, client.clone());
        client
    }
}

/// A remote upstream root URL with an optional explicit proxy. Each adapter
/// maps its own URL layout onto [`Remote::get`] / [`Remote::get_cached`].
#[derive(Clone)]
pub struct Remote {
    base: String,
    client: reqwest::Client,
    extra_headers: Vec<(String, String)>,
}

impl Remote {
    /// Create a remote. `base` is trimmed of trailing slashes. `proxy` follows
    /// the [`ClientFactory`] convention; `None` uses the environment proxy.
    pub fn new(factory: &ClientFactory, base: &str, proxy: Option<&str>) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            client: factory.client(proxy),
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
