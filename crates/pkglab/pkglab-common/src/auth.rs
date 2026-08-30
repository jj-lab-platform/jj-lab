//! Auth abstraction. Protocol crates accept `Arc<dyn Auth>`; the default
//! HMAC/bcrypt implementation lives in `pkglab-core` so an embedder (jjlab)
//! can supply its own issuer.

use async_trait::async_trait;

/// OCI-style scope actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Pull,
    Push,
    Delete,
    All,
}

impl Action {
    pub fn as_str(&self) -> &'static str {
        match self {
            Action::Pull => "pull",
            Action::Push => "push",
            Action::Delete => "delete",
            Action::All => "*",
        }
    }
}

/// Canonical `repository:<name>:<action>` scope string.
pub fn scope(name: &str, action: Action) -> String {
    format!("repository:{}:{}", name, action.as_str())
}

/// The single authentication surface shared by every protocol adapter.
///
/// Implementations resolve a caller identity from HTTP headers and decide
/// authorization. A "read-mostly anonymous" policy (pull without credentials,
/// writes authenticated) is expressed by the implementation, not by adapters.
#[async_trait]
pub trait Auth: Send + Sync + 'static {
    /// Resolve the caller identity from any supported credential (Basic,
    /// Bearer token/JWT, `X-NuGet-ApiKey`). Empty string = anonymous/unknown.
    async fn authenticate(&self, headers: &http::HeaderMap) -> String;

    /// Verify a credential pair (Basic auth user + password).
    async fn check_basic(&self, user: &str, pass: &str) -> bool;

    /// Validate a raw bearer token and return the username it belongs to.
    async fn check_token(&self, token: &str) -> Option<String>;

    /// Issue a token granting the given `repository:name:action` scopes.
    async fn issue_token(&self, username: &str, scopes: &[String], ttl_seconds: u64) -> String;

    /// Validate a bearer token against a wanted scope
    /// (`repository:name:action`). Returns the username when granted.
    async fn check_bearer(&self, token: &str, _wanted_scope: &str) -> Option<String> {
        // Default: any valid token grants everything (per-scope enforcement
        // is an implementation choice).
        self.check_token(token).await
    }

    /// Gate for non-OCI write operations (publish/upload/delete). Returns the
    /// username when the request carries any recognized credential.
    async fn authorize_write(&self, headers: &http::HeaderMap) -> Option<String> {
        let u = self.authenticate(headers).await;
        if u.is_empty() {
            None
        } else {
            Some(u)
        }
    }
}

// Minimal local header type so common does not pull axum/hyper directly.
pub use http::HeaderMap;
