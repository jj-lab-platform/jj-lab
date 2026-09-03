//! Neutral, protocol-agnostic artifact model plus the shared multi-hash type.

use serde::{Deserialize, Serialize};
use sha1::Digest as _;
use std::io::Read;

/// The multi-digest summary of a blob. Several protocols require digests other
/// than sha256 (Maven: md5+sha1, npm: sha1 shasum + sha512 integrity, ...), so
/// they are computed together once and cached alongside the blob.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hashes {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub md5: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha1: String,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha512: String,
}

/// Compute MD5, SHA1, SHA256 and SHA512 of `r`, consuming it. Returns the
/// hashes together with the total byte count. `r` is read to EOF.
pub fn compute_hashes<R: Read>(mut r: R) -> std::io::Result<(Hashes, u64)> {
    use sha2::Digest as _;
    let mut md5h = md5::Md5::new();
    let mut sha1h = sha1::Sha1::new();
    let mut sha256h = sha2::Sha256::new();
    let mut sha512h = sha2::Sha512::new();
    let mut n: u64 = 0;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = r.read(&mut buf)?;
        if read == 0 {
            break;
        }
        let chunk = &buf[..read];
        md5h.update(chunk);
        sha1h.update(chunk);
        sha256h.update(chunk);
        sha512h.update(chunk);
        n += read as u64;
    }
    Ok((
        Hashes {
            md5: hex::encode(md5h.finalize()),
            sha1: hex::encode(sha1h.finalize()),
            sha256: hex::encode(sha256h.finalize()),
            sha512: hex::encode(sha512h.finalize()),
        },
        n,
    ))
}

/// Convenience: sha256 hex of a byte slice.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

/// Convenience: sha1 hex of a byte slice.
pub fn sha1_hex(data: &[u8]) -> String {
    let mut h = sha1::Sha1::new();
    h.update(data);
    hex::encode(h.finalize())
}

/// A reference to one blob: its content digest, size, media type and logical
/// filename (e.g. `pkg-1.0.0.tgz`, `foo-1.0.whl`, `pom.xml`). `name` may be
/// empty for OCI layers/config.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Descriptor {
    pub digest: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub media_type: String,
    #[serde(default)]
    pub size: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

impl Descriptor {
    /// The bare hex portion of [`Self::digest`].
    pub fn hex(&self) -> &str {
        self.digest.split_once(':').map(|x| x.1).unwrap_or(&self.digest)
    }
}

/// A single published unit (an OCI manifest+layers, an npm package+tarball, a
/// PyPI distribution, a Conan recipe/package, ...).
///
/// `proprietary` holds protocol-specific metadata serialized by the adapter
/// (npm packument JSON, PyPI upload time, ...). The substrate never interprets
/// it. It is stored as raw bytes so content-addressed bodies (OCI manifests)
/// keep their exact bytes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Artifact {
    /// Fully-qualified repository/path name, e.g. `library/alpine`,
    /// `@scope/pkg`, `requests`, `conan/lib/1.0`.
    pub repository: String,
    /// Owning protocol adapter (`npm`, `pypi`, ...). Namespaces `repository`
    /// so protocols with overlapping names never collide.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub format: String,
    /// Protocol-specific version reference: an OCI tag/digest, a semantic
    /// version, an empty string for format-level extras (npm dist-tags).
    pub version: String,
    /// Top-level media type of the artifact body.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub media_type: String,
    /// Protocol-specific payload bytes (kept verbatim).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proprietary: Vec<u8>,
    /// Payload blobs referenced by this artifact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blobs: Vec<Descriptor>,
    /// Authoritative content digest of the artifact body (OCI manifests).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub digest: String,
    /// How the artifact entered the store: `push` (published) or `pull`
    /// (cached via pull-through). Empty means `push`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// Owning source repo (org/repo) when the artifact was published from a
    /// jjlab workspace — e.g. `build/zergx-agent`. Empty for pull-through
    /// caches and protocol-level extras. Enables repo-scoped package queries
    /// (GitHub-style "linked to repository").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo: String,
    /// The workspace bookmark (branch) the artifact was published from.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bookmark: String,
    /// The commit sha the artifact was published from (content provenance).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha: String,
}

impl Artifact {
    /// Defensive copy of the proprietary bytes.
    pub fn clone_proprietary(&self) -> Vec<u8> {
        self.proprietary.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_of_abc() {
        let (h, n) = compute_hashes("abc".as_bytes()).unwrap();
        assert_eq!(n, 3);
        assert_eq!(h.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(h.md5, "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(h.sha256, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn proprietary_bytes_survive_roundtrip() {
        // Regression (ported from Go TestArtifactProprietaryPreservesBytes):
        // proprietary bytes must survive a JSON roundtrip unchanged so
        // content-addressed OCI manifests keep their digest.
        let prop = br#"{"schemaVersion":2,
  "mediaType": "application/vnd.oci.image.manifest.v1+json",
  "config": { "digest": "sha256:abc", "size": 1 }
}"#;
        let a = Artifact {
            repository: "go-registry".into(),
            format: "oci".into(),
            version: "dev".into(),
            media_type: "application/vnd.oci.image.manifest.v1+json".into(),
            proprietary: prop.to_vec(),
            digest: "sha256:original".into(),
            ..Default::default()
        };
        let b = serde_json::to_vec(&a).unwrap();
        let got: Artifact = serde_json::from_slice(&b).unwrap();
        assert_eq!(got.proprietary, prop);
    }

    #[test]
    fn descriptor_hex() {
        let d = Descriptor { digest: "sha256:deadbeef".into(), ..Default::default() };
        assert_eq!(d.hex(), "deadbeef");
    }
}
