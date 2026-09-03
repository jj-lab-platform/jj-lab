//! Runtime upstream registry: format → base URL overrides and dotted
//! `format.sub` sub-endpoint overrides.
//!
//! A key is either a bare format name (`npm`, `pypi`) or a dotted
//! sub-endpoint (`cargo.static`, `nuget.search`). The bare key is the
//! format's primary upstream; dotted keys override only a specific endpoint.
//!
//! Upstreams only carry the base URL. Proxy config is not managed here — all
//! upstream requests honor the ambient `HTTP(S)_PROXY`/`NO_PROXY` environment
//! variables via [`crate::remote::ClientFactory`].

use crate::remote::ClientFactory;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Well-known public upstream for each package format.
pub fn default_upstream(format: &str) -> Option<&'static str> {
    Some(match format {
        "cargo" => "https://crates.io",
        "composer" => "https://repo.packagist.org",
        "conan" => "https://center.conan.io",
        "go" => "https://proxy.golang.org",
        "helm" => "https://charts.helm.sh/stable",
        "hex" => "https://repo.hex.pm",
        "maven" => "https://repo.maven.apache.org/maven2",
        "npm" => "https://registry.npmjs.org",
        "nuget" => "https://api.nuget.org",
        "pub" => "https://pub.dev",
        "pypi" => "https://pypi.org",
        "rubygems" => "https://rubygems.org",
        "swift" => "https://api.spm.swift.org",
        "oci" => "https://registry-1.docker.io",
        _ => return None,
    })
}

/// Well-known URL for a sub-endpoint that does not share the format's primary
/// upstream host.
pub fn default_sub_upstream(format: &str, sub: &str) -> Option<&'static str> {
    Some(match (format, sub) {
        ("cargo", "static") => "https://static.crates.io/crates",
        ("cargo", "index") => "https://index.crates.io",
        ("conan", "center") => "https://center2.conan.io",
        ("nuget", "search") => "https://azuresearch-usnc.nuget.org",
        ("nuget", "registration") => "https://api.nuget.org",
        ("hex", "repo") => "https://repo.hex.pm",
        ("rubygems", "index") => "https://index.rubygems.org",
        ("rubygems", "gems") => "https://rubygems.org/gems",
        _ => return None,
    })
}

/// Every sub-endpoint key that has a built-in default.
pub const SUB_KEYS: &[&str] = &[
    "cargo.static",
    "cargo.index",
    "conan.center",
    "nuget.search",
    "nuget.registration",
    "hex.repo",
    "rubygems.index",
    "rubygems.gems",
];

#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    upstreams: BTreeMap<String, String>,
}

struct Inner {
    overrides: BTreeMap<String, String>,
}

/// Thread-safe runtime upstream table with optional JSON file persistence.
#[derive(Clone)]
pub struct Upstreams {
    inner: std::sync::Arc<Mutex<Inner>>,
    path: std::sync::Arc<Option<PathBuf>>,
    factory: std::sync::Arc<ClientFactory>,
}

impl Upstreams {
    /// Create the table. `path` enables JSON persistence of overrides.
    pub fn new(path: Option<PathBuf>) -> Self {
        let u = Self {
            inner: std::sync::Arc::new(Mutex::new(Inner {
                overrides: BTreeMap::new(),
            })),
            path: std::sync::Arc::new(path),
            factory: std::sync::Arc::new(ClientFactory::new()),
        };
        u.load();
        u
    }

    pub fn factory(&self) -> ClientFactory {
        self.factory.as_ref().clone()
    }

    /// Effective upstream base for a bare format key.
    pub fn get(&self, format: &str) -> Option<String> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = inner.overrides.get(format) {
            if !v.is_empty() {
                return Some(v.trim_end_matches('/').to_string());
            }
        }
        default_upstream(format).map(|s| s.trim_end_matches('/').to_string())
    }

    /// Effective URL for a dotted sub-endpoint.
    pub fn sub(&self, format: &str, sub: &str) -> Option<String> {
        let key = format!("{format}.{sub}");
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = inner.overrides.get(&key) {
            if !v.is_empty() {
                return Some(v.trim_end_matches('/').to_string());
            }
        }
        default_sub_upstream(format, sub).map(|s| s.trim_end_matches('/').to_string())
    }

    /// Override a key (format or format.sub) with a URL and persist.
    pub fn set(&self, key: &str, url: &str) {
        let key = key.trim();
        if key.is_empty() {
            return;
        }
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.overrides.insert(key.to_string(), url.trim().to_string());
        }
        self.save();
    }

    /// Drop an override, reverting to defaults.
    pub fn reset(&self, key: &str) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).overrides.remove(key.trim());
        self.save();
    }

    /// True when the key has an explicit (non-default) override.
    pub fn is_override(&self, key: &str) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).overrides.contains_key(key.trim())
    }

    /// Snapshot of all effective upstreams (bare formats + sub endpoints).
    pub fn all(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for f in [
            "cargo", "composer", "conan", "go", "helm", "hex", "maven", "npm", "nuget", "pub",
            "pypi", "rubygems", "swift", "oci",
        ] {
            if let Some(v) = self.get(f) {
                out.insert(f.to_string(), v);
            }
        }
        for key in SUB_KEYS {
            // SUB_KEYS entries are statically guaranteed to contain a dot.
            let (f, s) = key.split_once('.').expect("SUB_KEYS entries are dotted");
            if let Some(v) = self.sub(f, s) {
                out.insert(key.to_string(), v);
            }
        }
        out
    }

    fn save(&self) {
        if let Some(path) = self.path.as_ref() {
            let overrides = {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.overrides.clone()
            };
            let state = Persisted { upstreams: overrides };
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(json) = serde_json::to_string_pretty(&state) {
                let _ = std::fs::write(path, json);
            }
        }
    }

    fn load(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let Ok(data) = std::fs::read_to_string(path) else {
            return;
        };
        if let Ok(state) = serde_json::from_str::<Persisted>(&data) {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            for (k, v) in state.upstreams {
                if !v.is_empty() {
                    inner.overrides.insert(k, v);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let u = Upstreams::new(None);
        assert_eq!(u.get("npm").unwrap(), "https://registry.npmjs.org");
        assert_eq!(u.get("nope"), None);
        u.set("npm", "https://registry.npmmirror.com/");
        assert_eq!(u.get("npm").unwrap(), "https://registry.npmmirror.com");
        u.reset("npm");
        assert_eq!(u.get("npm").unwrap(), "https://registry.npmjs.org");
    }

    #[test]
    fn sub_endpoints() {
        let u = Upstreams::new(None);
        assert_eq!(u.sub("cargo", "static").unwrap(), "https://static.crates.io/crates");
        assert_eq!(u.sub("cargo", "nope"), None);
        u.set("cargo.static", "https://mirror/static");
        assert_eq!(u.sub("cargo", "static").unwrap(), "https://mirror/static");
    }
}
