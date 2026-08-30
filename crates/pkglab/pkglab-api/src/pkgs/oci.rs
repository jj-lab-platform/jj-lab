//! OCI publish handler: decode a [`PublishRequest`] into stored OCI artifacts,
//! reusing `pkglab_oci::manifest` helpers so the stored bytes match the HTTP
//! adapter exactly.
//!
//! Request convention (see [`super::ProtocolHandler`]):
//! - `name` = repository (e.g. `library/alpine`).
//! - `version` = reference: a tag (`latest`) or a digest (`sha256:…`).
//! - `files` = one entry carrying the raw manifest JSON bytes (its own
//!   `.name` is ignored), plus optionally the config/layer blobs — though in
//!   practice blobs are pushed first via [`OciClient::push_blob`].
//! - `metadata.tags` = optional `[String]` extra tags to bind this manifest to.
//! - `metadata.media_type` = optional manifest media type (default OCI v1).

use crate::pkgs::ProtocolHandler;
use crate::types::PublishRequest;
use crate::RegistryApi;
use pkglab_common::artifact::compute_hashes;
use pkglab_common::registry::Result;
use pkglab_common::Artifact;
use pkglab_oci::manifest::{extract_blobs, parse_digest, reference_is_digest, sha256_digest};

/// Handler wiring the OCI adapter's exact manifest-persist logic.
pub struct OciHandler;

impl OciHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for OciHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ProtocolHandler for OciHandler {
    fn format(&self) -> &str {
        "oci"
    }

    async fn publish(&self, api: &RegistryApi, req: &PublishRequest) -> Result<()> {
        let reference = req.version.trim();
        let Some(file) = req.files.iter().find(|f| !f.data.is_empty()) else {
            return Err(pkglab_common::RegistryError::Other(
                "oci publish requires the manifest bytes in files".to_string(),
            ));
        };
        let bytes = file.data.clone();

        let media_type = req
            .metadata
            .get("media_type")
            .and_then(|m| m.as_str())
            .unwrap_or("application/vnd.oci.image.manifest.v1+json")
            .to_string();

        // Validate reference (tag vs digest) and compute the canonical digest.
        let sha = sha256_digest(&bytes);
        let effective_digest = sha.clone();
        if reference_is_digest(reference) {
            let algo = reference.split_once(':').map(|x| x.0).unwrap_or("sha256");
            if algo != "sha256" {
                return Err(pkglab_common::RegistryError::InvalidDigest(format!(
                    "unsupported digest algorithm: {algo}"
                )));
            }
            let hexpart = parse_digest(reference)?;
            if effective_digest != reference && !hexpart.eq_ignore_ascii_case(&sha[7..]) {
                return Err(pkglab_common::RegistryError::DigestMismatch {
                    expected: reference.to_string(),
                    got: effective_digest,
                });
            }
        }

        let blobs = extract_blobs(&bytes);

        // Persist the same artifacts the HTTP handler writes: one per
        // reference (tag and/or digest).
        let mut written = std::collections::HashSet::new();
        async fn put_ref(
            api: &RegistryApi,
            name: &str,
            version: &str,
            mt: &str,
            digest: &str,
            bytes: &[u8],
            blobs: &[pkglab_common::Descriptor],
        ) -> Result<()> {
            let art = Artifact {
                format: "oci".into(),
                repository: name.to_string(),
                version: version.to_string(),
                media_type: mt.to_string(),
                proprietary: bytes.to_vec(),
                blobs: blobs.to_vec(),
                digest: digest.to_string(),
                source: "push".into(),
            };
            api.put(art).await
        }

        // Tag reference.
        if !reference_is_digest(reference) {
            put_ref(api, &req.name, reference, &media_type, &effective_digest, &bytes, &blobs)
                .await?;
            written.insert(reference.to_string());
        }
        // Digest reference (always, so digest-indexed pulls work).
        put_ref(api, &req.name, &effective_digest, &media_type, &effective_digest, &bytes, &blobs)
            .await?;
        written.insert(effective_digest.clone());

        // Extra ?tag= bindings.
        if let Some(tags) = req.metadata.get("tags").and_then(|t| t.as_array()) {
            for t in tags {
                if let Some(s) = t.as_str() {
                    if !written.contains(s) {
                        put_ref(api, &req.name, s, &media_type, &effective_digest, &bytes, &blobs)
                            .await?;
                        written.insert(s.to_string());
                    }
                }
            }
        }
        Ok(())
    }
}

/// Advanced OCI access beyond the uniform [`crate::pkgs::Client`] surface.
pub struct OciClient {
    api: RegistryApi,
}

impl OciClient {
    pub fn new(api: RegistryApi) -> Self {
        Self { api }
    }

    pub fn api(&self) -> &RegistryApi {
        &self.api
    }

    /// Store a config/layer blob under its own digest (verify content).
    pub async fn push_blob(&self, digest: &str, data: &[u8]) -> Result<bool> {
        let (hashes, _) = compute_hashes(data).map_err(pkglab_common::RegistryError::from)?;
        let expected = format!("sha256:{}", hashes.sha256);
        parse_digest(digest)?;
        if digest != expected {
            return Err(pkglab_common::RegistryError::DigestMismatch {
                expected: digest.to_string(),
                got: expected,
            });
        }
        let mut cursor = std::io::Cursor::new(data);
        self.api.registry().blobs.put_if_absent(digest, &mut cursor).await
    }

    /// Fetch a manifest by tag or digest.
    pub async fn get_manifest(&self, repo: &str, reference: &str) -> Result<(Vec<u8>, String)> {
        let art = self.api.get("oci", repo, reference).await?;
        Ok((art.proprietary.clone(), art.media_type))
    }

    /// List tags of a repository.
    pub async fn list_tags(&self, repo: &str) -> Result<Vec<String>> {
        self.api.versions("oci", repo).await
    }

    /// Delete a manifest/tag binding.
    pub async fn delete(&self, repo: &str, reference: &str) -> Result<()> {
        let art = self.api.get("oci", repo, reference).await?;
        for b in &art.blobs {
            let _ = self.api.registry().blobs.delete(&b.digest).await;
        }
        self.api.delete("oci", repo, reference).await
    }
}
