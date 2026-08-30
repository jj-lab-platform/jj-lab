//! Per-format publish semantics layered on the neutral [`super::RegistryApi`].
//!
//! There is exactly one public client type — [`Client`] — and one publish
//! entry point regardless of protocol. The only thing that differs between
//! formats is *how* a [`PublishRequest`] is decoded into stored artifacts,
//! and that difference is isolated behind the [`ProtocolHandler`] trait.
//!
//! Every format now reuses the **same store function the HTTP adapter calls**
//! (each protocol crate exposes it as `pub`), so an in-process publish and an
//! HTTP publish produce byte-identical stored artifacts — there is no second
//! "naming table" to drift out of sync.
//!
//! `fetch`/`versions`/`delete`/`delete_repo` are protocol-neutral and do not
//! need a handler at all: they operate on `format + repository + version`.

pub mod conan;
pub mod oci;

use crate::types::{BlobOutput, PublishRequest};
use crate::{impls, RegistryApi};
use pkglab_common::registry::Result;
use pkglab_common::Artifact;

/// The per-format publish strategy: decode a [`PublishRequest`] into one or
/// more stored [`Artifact`]s.
#[async_trait::async_trait]
pub trait ProtocolHandler: Send + Sync {
    /// Protocol tag (`npm`, `oci`, ...).
    fn format(&self) -> &str;

    /// Publish the request using `api` (the composed substrate).
    async fn publish(&self, api: &RegistryApi, req: &PublishRequest) -> Result<()>;
}

/// The single, protocol-agnostic client. Construct with [`client`], then call
/// `publish` / `fetch` / `versions` / `delete` / `delete_repo`.
pub struct Client {
    api: RegistryApi,
    handler: std::sync::Arc<dyn ProtocolHandler>,
}

impl Client {
    pub fn new(api: RegistryApi, handler: std::sync::Arc<dyn ProtocolHandler>) -> Self {
        Self { api, handler }
    }

    pub fn format(&self) -> &str {
        self.handler.format()
    }

    /// Publish a release. Semantics are per-format but the call shape is
    /// uniform; see the handler modules for the exact meaning of
    /// `name`/`version`/`files`/`metadata`.
    pub async fn publish(&self, req: PublishRequest) -> Result<()> {
        self.handler.publish(&self.api, &req).await
    }

    /// The stored artifact (metadata + blob descriptors) for a version.
    pub async fn artifact(&self, name: &str, version: &str) -> Result<Artifact> {
        self.api.get(self.format(), name, version).await
    }

    /// Raw blob bytes (with filenames and digests) for a version.
    pub async fn fetch(&self, name: &str, version: &str) -> Result<Vec<BlobOutput>> {
        let art = self.artifact(name, version).await?;
        Ok(impls::read_blobs(self.api.registry(), &art).await)
    }

    /// Versions for a repository, ascending.
    pub async fn versions(&self, name: &str) -> Result<Vec<String>> {
        self.api.versions(self.format(), name).await
    }

    /// Remove a version and its blobs. Idempotent.
    pub async fn delete(&self, name: &str, version: &str) -> Result<()> {
        if let Ok(art) = self.artifact(name, version).await {
            for b in &art.blobs {
                let _ = self.api.registry().blobs.delete(&b.digest).await;
            }
        }
        self.api.delete(self.format(), name, version).await
    }

    /// Remove every version of a repository under this format.
    pub async fn delete_repo(&self, name: &str) -> Result<()> {
        for v in self.versions(name).await? {
            self.delete(name, &v).await?;
        }
        Ok(())
    }

    /// The neutral facade underneath (for raw/advanced access).
    pub fn api(&self) -> &RegistryApi {
        &self.api
    }
}

/// Build a [`Client`] with the handler appropriate for `format`.
pub fn client(api: RegistryApi, format: &str) -> Client {
    let h: std::sync::Arc<dyn ProtocolHandler> = match format {
        "oci" => std::sync::Arc::new(oci::OciHandler::new()),
        "conan" => std::sync::Arc::new(conan::ConanHandler::new()),
        _ => std::sync::Arc::new(AdapterHandler::new(format)),
    };
    Client::new(api, h)
}

/// Publish by delegating to the protocol adapter's own `store_*` function, so
/// the stored artifact is byte-identical to what its HTTP path produces.
pub struct AdapterHandler {
    format: String,
}

impl AdapterHandler {
    pub fn new(format: &str) -> Self {
        Self { format: format.to_string() }
    }
}

/// The first non-empty file's bytes (and its supplied name).
fn first_file(req: &PublishRequest) -> (String, Vec<u8>) {
    req.files
        .iter()
        .find(|f| !f.data.is_empty())
        .map(|f| (f.name.clone(), f.data.clone()))
        .unwrap_or_default()
}

#[async_trait::async_trait]
impl ProtocolHandler for AdapterHandler {
    fn format(&self) -> &str {
        &self.format
    }

    async fn publish(&self, api: &RegistryApi, req: &PublishRequest) -> Result<()> {
        let reg = api.registry().clone();
        let name = if req.name.is_empty() { "unknown" } else { req.name.trim() };
        let version = if req.version.is_empty() { "0.1.0" } else { req.version.trim() };
        let (fname, data) = first_file(req);

        match self.format.as_str() {
            "npm" => {
                // The npm publish path stores the package.json (proprietary) and
                // the tarball blob; the blob name is derived from name+version.
                let state =
                    pkglab_npm::NpmState { registry: reg, auth: None, self_base: String::new() };
                pkglab_npm::store_version_src(
                    &state,
                    name,
                    version,
                    data,
                    req.metadata.clone(),
                    "push",
                )
                .await;
            }
            "pypi" => {
                let state =
                    pkglab_pypi::PyPiState { registry: reg, auth: None, self_base: String::new() };
                let filename = if fname.is_empty() {
                    format!("{}-{}.tar.gz", pkglab_pypi::normalize_name(name), version)
                } else {
                    fname
                };
                pkglab_pypi::store_version_src(&state, name, version, &filename, data, "push")
                    .await;
            }
            "cargo" => {
                let state = pkglab_cargo::CargoState {
                    registry: reg,
                    auth: None,
                    self_base: String::new(),
                };
                pkglab_cargo::store_version_src(&state, name, version, data, "push").await;
            }
            "go" => {
                let state = pkglab_go::GoState { registry: reg, auth: None };
                pkglab_go::store_version_src(&state, name, version, data, "push").await;
            }
            "maven" => {
                let state = pkglab_maven::MavenState { registry: reg, auth: None };
                let filename =
                    if fname.is_empty() { format!("{name}-{version}.jar") } else { fname };
                pkglab_maven::store_version_src(&state, name, version, &filename, data, "push")
                    .await;
            }
            "composer" => {
                let state = pkglab_composer::ComposerState {
                    registry: reg,
                    auth: None,
                    self_base: String::new(),
                };
                pkglab_composer::store_version_src(&state, name, version, data, "push").await;
            }
            "nuget" => {
                let state = pkglab_nuget::NuGetState {
                    registry: reg,
                    auth: None,
                    self_base: String::new(),
                };
                let filename =
                    if fname.is_empty() { format!("{name}.{version}.nupkg") } else { fname };
                pkglab_nuget::store_version_src(&state, name, version, &filename, data, "push")
                    .await;
            }
            "rubygems" => {
                let state = pkglab_rubygems::RubyGemsState { registry: reg, auth: None };
                let filename =
                    if fname.is_empty() { format!("{name}-{version}.gem") } else { fname };
                pkglab_rubygems::store_version_src(&state, name, version, &filename, data, "push")
                    .await;
            }
            "hex" => {
                let state = pkglab_hex::HexState::new(reg, None);
                let filename =
                    if fname.is_empty() { format!("{name}-{version}.tar") } else { fname };
                pkglab_hex::store_version(&state, name, version, &filename, data).await;
            }
            "pub" => {
                let state =
                    pkglab_pub::PubState { registry: reg, auth: None, self_base: String::new() };
                let filename =
                    if fname.is_empty() { format!("{name}-{version}.tar.gz") } else { fname };
                pkglab_pub::store_version_src(&state, name, version, &filename, data, "push").await;
            }
            "swift" => {
                let state = pkglab_swift::SwiftState {
                    registry: reg,
                    auth: None,
                    self_base: String::new(),
                };
                let filename =
                    if fname.is_empty() { format!("{name}-{version}.zip") } else { fname };
                pkglab_swift::store_version_src(&state, name, version, &filename, data, "push")
                    .await;
            }
            "helm" => {
                let state =
                    pkglab_helm::HelmState { registry: reg, auth: None, self_base: String::new() };
                let filename =
                    if fname.is_empty() { format!("{name}-{version}.tgz") } else { fname };
                pkglab_helm::store_chart(&state, name, version, &filename, data).await;
            }
            "generic" => {
                let filename =
                    if fname.is_empty() { format!("{name}-{version}.bin") } else { fname };
                pkglab_generic::store(&reg, name, version, &filename, data).await;
            }
            other => {
                return Err(pkglab_common::RegistryError::Other(format!(
                    "no adapter handler for format: {other}"
                )));
            }
        }
        Ok(())
    }
}
