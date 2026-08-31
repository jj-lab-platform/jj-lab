//! Runtime namespace registry for the `/ops` surface.
//!
//! jjlab is the cluster orchestrator: it may create/delete pods, services and
//! deployments on behalf of the ops-extension, and every such request must land
//! in an approved namespace. The registry is seeded from
//! `JJLAB_OPS_NAMESPACES` (comma-separated, first entry = default) and can be
//! grown at runtime via `register` (write-token gated at the HTTP layer). A
//! namespace has to be in the registry before any `/ops` run/service/helm
//! request may target it.
//!
//! The authority on *what* a namespace may contain stays with Kubernetes RBAC;
//! the registry only gates jjlab's own ops surface, it does not (and cannot)
//! grant cluster permissions.

use std::collections::HashSet;
use std::sync::RwLock;

use crate::repo::{RepoError, RepoResult};

/// Registry of approved `/ops` target namespaces plus the default.
pub struct NamespaceRegistry {
    inner: RwLock<WorkspaceInner>,
}

/// State guarded by the lock: the approved set + the default name.
#[derive(Clone)]
struct WorkspaceInner {
    names: Vec<String>,
    set: HashSet<String>,
    default: String,
}

impl NamespaceRegistry {
    /// Build from an optional comma-separated env value. Empty value yields a
    /// registry with only `"default"`.
    pub fn from_env() -> Self {
        let raw = std::env::var("JJLAB_OPS_NAMESPACES").unwrap_or_default();
        let names: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        Self::new(names)
    }

    /// Build from an explicit list. Falls back to `["default"]` when empty.
    pub fn new(names: Vec<String>) -> Self {
        let names: Vec<String> = if names.is_empty() {
            vec!["default".to_string()]
        } else {
            names
        };
        let set: HashSet<String> = names.iter().cloned().collect();
        let default = names[0].clone();
        NamespaceRegistry {
            inner: RwLock::new(WorkspaceInner { names, set, default }),
        }
    }

    /// Current list (insertion order preserved) of approved namespaces.
    pub fn list(&self) -> Vec<String> {
        self.inner.read().expect("namespace registry poisoned").names.clone()
    }

    /// The default namespace (first registered).
    pub fn default(&self) -> String {
        self.inner.read().expect("namespace registry poisoned").default.clone()
    }

    /// Approve a new namespace (idempotent). Returns false if already present.
    pub fn register(&self, name: &str) -> RepoResult<bool> {
        validate_namespace(name)?;
        let mut inner = self.inner.write().expect("namespace registry poisoned");
        if inner.set.contains(name) {
            return Ok(false);
        }
        inner.names.push(name.to_string());
        inner.set.insert(name.to_string());
        Ok(true)
    }

    /// Resolve a requested namespace against the registry: `None` resolves to
    /// the default; a name not in the registry is `Invalid`.
    pub fn resolve(&self, requested: Option<&str>) -> RepoResult<String> {
        let inner = self.inner.read().expect("namespace registry poisoned");
        match requested {
            None => Ok(inner.default.clone()),
            Some(n) if inner.set.contains(n) => Ok(n.to_string()),
            Some(n) => Err(RepoError::Invalid(format!(
                "namespace {n:?} not registered; available: {}",
                inner.names.join(", ")
            ))),
        }
    }
}

/// Validate a namespace name (RFC 1123 label, same grammar as K8s names).
fn validate_namespace(name: &str) -> RepoResult<()> {
    if name.is_empty()
        || name.len() > 63
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || name.starts_with('-')
        || name.ends_with('-')
    {
        return Err(RepoError::Invalid(format!(
            "invalid namespace {name:?}: must be a lowercase RFC 1123 label"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_default_namespace_when_empty() {
        let r = NamespaceRegistry::new(vec![]);
        assert_eq!(r.default(), "default");
        assert_eq!(r.resolve(None).unwrap(), "default");
    }

    #[test]
    fn resolves_requested_and_rejects_unknown() {
        let r = NamespaceRegistry::new(vec!["a".into(), "b".into()]);
        assert_eq!(r.resolve(None).unwrap(), "a");
        assert_eq!(r.resolve(Some("b")).unwrap(), "b");
        assert!(r.resolve(Some("nope")).is_err());
    }

    #[test]
    fn register_is_idempotent_and_validated() {
        let r = NamespaceRegistry::new(vec!["a".into()]);
        assert!(!r.register("a").unwrap());
        assert!(r.register("b").unwrap());
        assert!(r.register("Bad_Name").is_err());
        assert!(r.register("-x").is_err());
    }
}