//! pkglab-gateway — adapter trait + router builder.
//!
//! Protocol crates implement [`ProtocolAdapter`]; the embedder (jjlab server
//! or the dev harness) registers them via [`Gateway`] and obtains one
//! `axum::Router` with the agreed mount layout: every format under
//! `/pkgs/<format>`, except a chosen root format (OCI's `/v2` is fixed by the
//! distribution spec and must live at the origin root).

use std::collections::HashMap;
use std::sync::Arc;

/// A protocol implementation mountable into the gateway.
pub trait ProtocolAdapter: Send + Sync + 'static {
    /// Protocol identifier, e.g. "oci", "npm", "pypi".
    fn format(&self) -> &'static str;

    /// Register this adapter's routes. `prefix` is the absolute path the
    /// adapter is mounted under (normalized to start with `/`), empty for a
    /// root mount. The returned router replaces the caller's routes.
    fn mount(self: Arc<Self>, prefix: &str) -> axum::Router;
}

/// Dispatch configuration.
#[derive(Default)]
pub struct MountOpts {
    /// format -> absolute path prefix (e.g. "npm" -> "/pkgs/npm").
    pub prefixes: HashMap<String, String>,
    /// The format mounted at "/" (catch-all), typically "oci".
    pub root_format: Option<String>,
    /// hostname (lowercase, no port) -> format; whole origin serves it.
    pub hosts: HashMap<String, String>,
}

impl MountOpts {
    /// The standard pkglab layout: every format under `/pkgs/<format>`, OCI
    /// at root.
    pub fn standard(formats: &[&str]) -> Self {
        let mut prefixes = HashMap::new();
        for f in formats {
            prefixes.insert(f.to_string(), format!("/pkgs/{f}"));
        }
        Self { prefixes, root_format: None, hosts: HashMap::new() }
    }

    pub fn with_root(mut self, format: &str) -> Self {
        self.root_format = Some(format.to_string());
        self
    }
}

/// Adapter registry performing path-prefix and host dispatch.
#[derive(Default)]
pub struct Gateway {
    adapters: HashMap<String, Arc<dyn ProtocolAdapter>>,
}

impl Gateway {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an adapter. Fails when the format is already registered.
    pub fn register(&mut self, a: Arc<dyn ProtocolAdapter>) -> Result<(), String> {
        let format = a.format();
        if self.adapters.contains_key(format) {
            return Err(format!("gateway: adapter already registered: {format}"));
        }
        self.adapters.insert(format.to_string(), a);
        Ok(())
    }

    pub fn formats(&self) -> Vec<String> {
        let mut v: Vec<String> = self.adapters.keys().cloned().collect();
        v.sort();
        v
    }

    /// Build the top-level router.
    pub fn build(self, opts: &MountOpts) -> axum::Router {
        let mut root = axum::Router::new();

        // Path-prefix mounts.
        for (format, prefix) in &opts.prefixes {
            let Some(a) = self.adapters.get(format) else {
                continue;
            };
            let prefix =
                if prefix.starts_with('/') { prefix.clone() } else { format!("/{prefix}") };
            let sub = a.clone().mount(&prefix);
            if prefix == "/" {
                root = root.merge(sub);
            } else {
                root = root.nest(&prefix, sub);
            }
        }

        // Root mount (catch-all): receives every request not claimed by a
        // prefix route.
        if let Some(format) = &opts.root_format {
            if let Some(a) = self.adapters.get(format) {
                let sub = a.clone().mount("");
                root = root.fallback_service(sub);
            }
        }

        // Host-based mounts.
        for (host, format) in &opts.hosts {
            let Some(a) = self.adapters.get(format) else {
                continue;
            };
            let sub = a.clone().mount("");
            let host = host.clone();
            // Host dispatch is implemented as middleware-free route matching
            // on the Host header via a small filter service.
            let svc = axum::middleware::from_fn(
                move |req: axum::http::Request<axum::body::Body>, next: axum::middleware::Next| {
                    let host = host.clone();
                    async move {
                        let req_host = req
                            .headers()
                            .get(axum::http::header::HOST)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .split(':')
                            .next()
                            .unwrap_or("")
                            .to_lowercase();
                        if req_host != host {
                            return axum::http::StatusCode::NOT_FOUND.into_response();
                        }
                        next.run(req).await
                    }
                },
            );
            root = root.layer(svc).merge(sub);
        }

        root
    }
}

use axum::response::IntoResponse;

/// Convenience: assemble the standard layout in one call.
pub fn build_router(adapters: Vec<Arc<dyn ProtocolAdapter>>, opts: &MountOpts) -> axum::Router {
    let mut gw = Gateway::new();
    for a in adapters {
        if let Err(e) = gw.register(a) {
            tracing::warn!("{e}");
        }
    }
    gw.build(opts)
}
