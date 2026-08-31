//! In-process package registry assembly (pkglab).
//!
//! jjlab serves its own OCI registry (`/v2`) and the 15 language-package
//! protocols (`/pkgs/<format>`) in the same process, replacing the external
//! Go `artifact` service. The substrate (SQLite metadata + CAS blobs +
//! upstreams) is opened at `JJLAB_PKGLAB_ROOT`; the HTTP surface is a thin
//! shell over `pkglab_*` routers, identical to the pkglab devserver layout.
//!
//! Auth is bridged onto jjlab's existing `JJLAB_TOKENS` (`token=level`):
//!   - pull        -> read token or anonymous (mirrors git clone)
//!   - push/delete -> write token (mirrors git push)

use std::sync::Arc;

use axum::Router;

use crate::{AppState, Level};

/// Bridge jjlab's static tokens onto the pkglab [`pkglab_common::Auth`] trait.
pub struct TokenAuth {
    tokens: Arc<Vec<(String, Level)>>,
}

impl TokenAuth {
    pub fn new(tokens: Arc<Vec<(String, Level)>>) -> Self {
        Self { tokens }
    }

    fn level_of(&self, token: &str) -> Option<Level> {
        self.tokens.iter().find(|(t, _)| t == token).map(|(_, l)| *l)
    }

    fn username_for(&self, token: &str) -> Option<String> {
        self.level_of(token).map(|l| match l {
            Level::Write => format!("token:{token}"),
            _ => format!("token:{token}"),
        })
    }

    /// Whether `action` (pull/push/delete/*) is granted by `token`.
    fn permits(&self, token: &str, action: &str) -> bool {
        match self.level_of(token) {
            Some(Level::Write) => true,
            Some(Level::Read) => action == "pull",
            _ => false,
        }
    }
}

#[async_trait::async_trait]
impl pkglab_common::Auth for TokenAuth {
    async fn authenticate(&self, headers: &http::HeaderMap) -> String {
        let raw =
            headers.get(http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or("");
        // Bearer / token prefix.
        let bearer = raw
            .strip_prefix("Bearer ")
            .or_else(|| raw.strip_prefix("token "))
            .or_else(|| raw.strip_prefix("Token "));
        if let Some(t) = bearer {
            if let Some(u) = self.username_for(t.trim()) {
                return u;
            }
        }
        // Bare token in Authorization (cargo registry token sends `Authorization: <token>`).
        if !raw.is_empty()
            && !raw.starts_with("Bearer ")
            && !raw.starts_with("token ")
            && !raw.starts_with("Token ")
            && !raw.starts_with("Basic ")
        {
            if let Some(u) = self.username_for(raw.trim()) {
                return u;
            }
        }
        // Basic (user:token).
        if let Some(basic) = raw.strip_prefix("Basic ") {
            use base64::Engine as _;
            if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(basic.trim()) {
                if let Ok(text) = String::from_utf8(decoded) {
                    if let Some((_, secret)) = text.split_once(':') {
                        if let Some(u) = self.username_for(secret) {
                            return u;
                        }
                    }
                }
            }
        }
        // X-NuGet-ApiKey (dotnet nuget push --api-key).
        if let Some(key) = headers.get("X-NuGet-ApiKey").and_then(|v| v.to_str().ok()) {
            if let Some(u) = self.username_for(key.trim()) {
                return u;
            }
        }
        String::new()
    }

    async fn check_basic(&self, _user: &str, pass: &str) -> bool {
        self.level_of(pass).is_some()
    }

    async fn check_token(&self, token: &str) -> Option<String> {
        self.username_for(token)
    }

    async fn issue_token(&self, _username: &str, _scopes: &[String], _ttl_seconds: u64) -> String {
        // Tokens ARE the credentials: return the write token verbatim so it
        // round-trips as a Bearer credential.
        self.tokens
            .iter()
            .find(|(_, l)| *l == Level::Write)
            .map(|(t, _)| t.clone())
            .unwrap_or_default()
    }

    async fn check_bearer(&self, token: &str, wanted_scope: &str) -> Option<String> {
        // wanted_scope: "repository:name:action".
        let action = wanted_scope.rsplit_once(':').map(|(_, a)| a).unwrap_or("");
        if self.permits(token, action) {
            self.username_for(token)
        } else {
            None
        }
    }
}

/// The external base URL (scheme://host[:port]) used for auth challenge
/// realms and absolute self URLs in protocol responses.
fn self_base() -> String {
    std::env::var("JJLAB_SELF_BASE")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "http://localhost:8080".to_string())
}

/// Assemble the pkglab protocol routers. Each returned entry is
/// `(mount_path, router)`: OCI at `/v2`, protocols at `/pkgs/<format>`.
/// Returns `None` when the registry substrate is unavailable (e.g. unit tests
/// that only exercise the git surface).
pub fn assemble(
    registry: &Arc<pkglab_common::Registry>,
    state: &AppState,
) -> Option<Vec<(&'static str, Router)>> {
    let auth: Option<Arc<dyn pkglab_common::Auth>> =
        Some(Arc::new(TokenAuth::new(state.tokens.clone())));

    let base = self_base();
    let common = registry.clone();

    // OCI adapter (mounted at root: /v2 is spec-fixed). The pull-through
    // upstream defaults to Docker Hub unless overridden for air-gapped
    // deployments.
    let default_upstream = registry
        .upstreams
        .get("oci")
        .or_else(|| std::env::var("JJLAB_OCI_UPSTREAM").ok())
        .unwrap_or_else(|| "https://registry-1.docker.io".to_string());
    let oci = pkglab_oci::router(Arc::new(pkglab_oci::OciState {
        blobs: registry.blobs.clone(),
        meta: registry.meta.clone(),
        upstreams: registry.upstreams.clone(),
        auth: auth.clone(),
        default_upstream: Some(default_upstream),
        self_base: format!("{}/v2", base.trim_end_matches('/')),
    }));

    let generic = pkglab_generic::router(Arc::new(pkglab_generic::GenericState {
        registry: common.clone(),
        auth: auth.clone(),
    }));
    let helm = pkglab_helm::router(Arc::new(pkglab_helm::HelmState {
        registry: common.clone(),
        auth: auth.clone(),
        self_base: format!("{base}/pkgs/helm"),
    }));
    let go_mod = pkglab_go::router(Arc::new(pkglab_go::GoState {
        registry: common.clone(),
        auth: auth.clone(),
    }));
    let maven = pkglab_maven::router(Arc::new(pkglab_maven::MavenState {
        registry: common.clone(),
        auth: auth.clone(),
    }));
    let pypi = pkglab_pypi::router(Arc::new(pkglab_pypi::PyPiState {
        registry: common.clone(),
        auth: auth.clone(),
        self_base: format!("{base}/pkgs/pypi"),
    }));
    let composer = pkglab_composer::router(Arc::new(pkglab_composer::ComposerState {
        registry: common.clone(),
        auth: auth.clone(),
        self_base: format!("{base}/pkgs/composer"),
    }));
    let npm = pkglab_npm::router(Arc::new(pkglab_npm::NpmState {
        registry: common.clone(),
        auth: auth.clone(),
        self_base: format!("{base}/pkgs/npm"),
    }));
    let nuget = pkglab_nuget::router(Arc::new(pkglab_nuget::NuGetState {
        registry: common.clone(),
        auth: auth.clone(),
        self_base: format!("{base}/pkgs/nuget"),
    }));
    let rubygems = pkglab_rubygems::router(Arc::new(pkglab_rubygems::RubyGemsState {
        registry: common.clone(),
        auth: auth.clone(),
    }));
    let pub_dev = pkglab_pub::router(Arc::new(pkglab_pub::PubState {
        registry: common.clone(),
        auth: auth.clone(),
        self_base: format!("{base}/pkgs/pub"),
    }));
    let hex = pkglab_hex::router(Arc::new(pkglab_hex::HexState::new(common.clone(), auth.clone())));
    let cargo = pkglab_cargo::router(Arc::new(pkglab_cargo::CargoState {
        registry: common.clone(),
        auth: auth.clone(),
        self_base: format!("{base}/pkgs/cargo"),
    }));
    let swift = pkglab_swift::router(Arc::new(pkglab_swift::SwiftState {
        registry: common.clone(),
        auth: auth.clone(),
        self_base: format!("{base}/pkgs/swift"),
    }));
    let conan = pkglab_conan::router(Arc::new(pkglab_conan::ConanState {
        registry: common.clone(),
        auth: auth.clone(),
        self_base: format!("{base}/pkgs/conan"),
    }));
    let system = pkglab_core::system::router(Arc::new(pkglab_core::system::SystemState {
        registry: common.clone(),
        auth: auth.clone(),
    }));

    Some(vec![
        ("/v2", oci),
        ("/pkgs/generic", generic),
        ("/pkgs/helm", helm),
        ("/pkgs/go", go_mod),
        ("/pkgs/maven", maven),
        ("/pkgs/pypi", pypi),
        ("/pkgs/composer", composer),
        ("/pkgs/npm", npm),
        ("/pkgs/nuget", nuget),
        ("/pkgs/rubygems", rubygems),
        ("/pkgs/pub", pub_dev),
        ("/pkgs/hex", hex),
        ("/pkgs/cargo", cargo),
        ("/pkgs/swift", swift),
        ("/pkgs/conan", conan),
        ("/pkgs/system", system),
    ])
}
