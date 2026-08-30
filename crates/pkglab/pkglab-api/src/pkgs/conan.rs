//! Conan publish handler: decode a [`PublishRequest`] into stored Conan
//! artifacts, reusing `pkglab_conan` helpers (`store_file`, `save_recipe_rev`,
//! `package_file_name`, `drop_artifact`) so the stored bytes match the HTTP
//! adapter exactly.
//!
//! Request convention (see [`super::ProtocolHandler`]):
//! - `name` = recipe name, `version` = recipe version.
//! - `metadata.user` / `metadata.channel` / `metadata.revision` = the Conan
//!   coordinates; revision defaults to "0".
//! - `files` = recipe files (`conanfile.py`, `conanmanifest.txt`, ...) carried
//!   by their filenames. Files whose name starts with `package/` are package
//!   files under the same recipe.

use crate::pkgs::ProtocolHandler;
use crate::types::PublishRequest;
use crate::RegistryApi;
use pkglab_common::registry::Result;
use pkglab_conan::{package_file_name, save_recipe_rev, store_file, ConanState};

pub struct ConanHandler;

impl ConanHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ConanHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ProtocolHandler for ConanHandler {
    fn format(&self) -> &str {
        "conan"
    }

    async fn publish(&self, api: &RegistryApi, req: &PublishRequest) -> Result<()> {
        let name = req.name.trim();
        let version = if req.version.is_empty() { "0.1.0" } else { req.version.trim() };
        let revision =
            req.metadata.get("revision").and_then(|r| r.as_str()).unwrap_or("0").to_string();

        let state =
            ConanState { registry: api.registry().clone(), auth: None, self_base: String::new() };
        let state = &state;

        save_recipe_rev(state, name, version, &revision).await;

        for f in &req.files {
            if f.data.is_empty() {
                continue;
            }
            let filename = if f.name.starts_with("package/") {
                // package/{pid}/{prev}/{filename}; uploaded verbatim.
                f.name.clone()
            } else {
                f.name.clone()
            };
            store_file(state, name, version, &filename, f.data.clone()).await;
        }
        Ok(())
    }
}

/// Direct Conan access beyond the uniform [`crate::pkgs::Client`] surface.
pub struct ConanClient {
    api: RegistryApi,
}

impl ConanClient {
    pub fn new(api: RegistryApi) -> Self {
        Self { api }
    }

    pub fn api(&self) -> &RegistryApi {
        &self.api
    }

    fn state(&self) -> ConanState {
        ConanState { registry: self.api.registry().clone(), auth: None, self_base: String::new() }
    }

    /// Store a single package file (bind it to pid/prev).
    pub async fn put_package_file(
        &self,
        name: &str,
        version: &str,
        pid: &str,
        prev: &str,
        filename: &str,
        data: Vec<u8>,
    ) -> Result<()> {
        let st = self.state();
        let full = package_file_name(pid, prev, filename);
        store_file(&st, name, version, &full, data).await;
        Ok(())
    }

    /// Revisions recorded for a recipe (via the recipe-rev "" artifact).
    pub async fn recipe_revision(&self, name: &str) -> Result<Option<String>> {
        match self.api.get("conan", name, "").await {
            Ok(art) => Ok(serde_json::from_slice::<serde_json::Value>(&art.proprietary)
                .ok()
                .and_then(|m| m.get("revision").and_then(|r| r.as_str()).map(str::to_string))),
            Err(_) => Ok(None),
        }
    }

    /// Versions of a recipe.
    pub async fn versions(&self, name: &str) -> Result<Vec<String>> {
        self.api.versions("conan", name).await
    }

    /// Delete a recipe (all revisions, blobs reclaimed).
    pub async fn delete_recipe(&self, name: &str, version: &str) -> Result<()> {
        let st = self.state();
        pkglab_conan::drop_artifact(&st, name, version).await;
        Ok(())
    }
}
