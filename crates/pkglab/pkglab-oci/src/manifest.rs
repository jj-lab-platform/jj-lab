//! Manifest/blob helpers: digest parsing, blob extraction from manifests,
//! range parsing, registry-prefix splitting.

use pkglab_common::registry::{RegistryError, Result as RegResult};
use sha2::Digest as _;

/// Compute the sha256 digest string of bytes.
pub fn sha256_digest(data: &[u8]) -> String {
    let mut h = sha2::Sha256::new();
    h.update(data);
    format!("sha256:{}", hex::encode(h.finalize()))
}

/// Parse and validate `algo:hex`. Returns the lowercase hex portion.
pub fn parse_digest(d: &str) -> RegResult<String> {
    let Some((algo, hexpart)) = d.split_once(':') else {
        return Err(RegistryError::InvalidDigest(d.to_string()));
    };
    if algo.is_empty() || hexpart.is_empty() {
        return Err(RegistryError::InvalidDigest(d.to_string()));
    }
    match algo {
        "sha256" | "sha512" => {}
        _ => return Err(RegistryError::InvalidDigest(format!("unsupported algorithm: {algo}"))),
    }
    let want = if algo == "sha256" { 64 } else { 128 };
    if hexpart.len() != want || !hexpart.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(RegistryError::InvalidDigest(d.to_string()));
    }
    Ok(hexpart.to_lowercase())
}

/// A parsed tag-or-digest reference: digests contain ':'.
pub fn reference_is_digest(reference: &str) -> bool {
    reference.contains(':')
}

/// The blob digests referenced by a manifest or index body (config + layers,
/// or nested manifests for an index).
pub fn extract_blobs(body: &[u8]) -> Vec<pkglab_common::Descriptor> {
    #[derive(serde::Deserialize)]
    struct Probe2 {
        #[serde(default, rename = "mediaType")]
        media_type: String,
    }
    let Ok(probe) = serde_json::from_slice::<Probe2>(body) else {
        return Vec::new();
    };

    if probe.media_type == "application/vnd.oci.image.index.v1+json"
        || probe.media_type == "application/vnd.docker.distribution.manifest.list.v2+json"
    {
        #[derive(serde::Deserialize)]
        struct Idx {
            #[serde(default, rename = "manifests")]
            manifests: Vec<D>,
        }
        #[derive(serde::Deserialize)]
        struct D {
            #[serde(default, rename = "digest")]
            digest: String,
            #[serde(default, rename = "mediaType")]
            media_type: String,
            #[serde(default, rename = "size")]
            size: i64,
        }
        if let Ok(idx) = serde_json::from_slice::<Idx>(body) {
            return idx
                .manifests
                .into_iter()
                .map(|m| pkglab_common::Descriptor {
                    digest: m.digest,
                    media_type: m.media_type,
                    size: m.size,
                    name: String::new(),
                })
                .collect();
        }
        return Vec::new();
    }

    #[derive(serde::Deserialize)]
    struct M {
        #[serde(default, rename = "config")]
        config: D,
        #[serde(default, rename = "layers")]
        layers: Vec<D>,
    }
    #[derive(serde::Deserialize, Default)]
    struct D {
        #[serde(default, rename = "digest")]
        digest: String,
        #[serde(default, rename = "mediaType")]
        media_type: String,
        #[serde(default, rename = "size")]
        size: i64,
    }
    if let Ok(m) = serde_json::from_slice::<M>(body) {
        let mut out = Vec::with_capacity(m.layers.len() + 1);
        if !m.config.digest.is_empty() {
            out.push(pkglab_common::Descriptor {
                digest: m.config.digest,
                media_type: m.config.media_type,
                size: m.config.size,
                name: String::new(),
            });
        }
        for l in m.layers {
            out.push(pkglab_common::Descriptor {
                digest: l.digest,
                media_type: l.media_type,
                size: l.size,
                name: String::new(),
            });
        }
        return out;
    }
    Vec::new()
}

/// The subject digest of a manifest body (OCI 1.1 referrers).
pub fn manifest_subject(body: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Probe {
        #[serde(default, rename = "subject")]
        subject: Subj,
    }
    #[derive(serde::Deserialize, Default)]
    struct Subj {
        #[serde(default, rename = "digest")]
        digest: String,
    }
    serde_json::from_slice::<Probe>(body).ok().map(|p| p.subject.digest).filter(|s| !s.is_empty())
}

/// The artifactType of a manifest body (artifactType, falling back to the
/// config mediaType).
pub fn manifest_artifact_type(body: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Probe {
        #[serde(default, rename = "artifactType")]
        artifact_type: String,
        #[serde(default, rename = "config")]
        config: Cfg,
    }
    #[derive(serde::Deserialize, Default)]
    struct Cfg {
        #[serde(default, rename = "mediaType")]
        media_type: String,
    }
    serde_json::from_slice::<Probe>(body).ok().map(|p| {
        if p.artifact_type.is_empty() {
            p.config.media_type
        } else {
            p.artifact_type
        }
    })
}

/// A parsed single HTTP Range header (`bytes=start-end`), suffix form
/// supported. Returns (start, end) inclusive.
pub fn parse_range(hdr: &str, size: u64) -> RegResult<(u64, u64)> {
    let spec =
        hdr.strip_prefix("bytes=").ok_or_else(|| RegistryError::Other("invalid range".into()))?;
    let (a, b) =
        spec.split_once('-').ok_or_else(|| RegistryError::Other("invalid range".into()))?;
    let (start, end) = if a.is_empty() {
        // suffix: last N bytes
        let n: u64 = b.parse().map_err(|_| RegistryError::Other("invalid range".into()))?;
        let start = size.saturating_sub(n);
        (start, size.saturating_sub(1))
    } else {
        let start: u64 = a.parse().map_err(|_| RegistryError::Other("invalid range".into()))?;
        let end = if b.is_empty() {
            size.saturating_sub(1)
        } else {
            b.parse().map_err(|_| RegistryError::Other("invalid range".into()))?
        };
        (start, end)
    };
    if size == 0 || start >= size || start > end {
        return Err(RegistryError::Other("range not satisfiable".into()));
    }
    Ok((start, end.min(size - 1)))
}

/// A parsed Content-Range request header (`bytes start-end[/total]`).
/// Returns the start offset.
pub fn parse_content_range_start(cr: &str) -> RegResult<u64> {
    let s = cr.trim();
    let s = s.strip_prefix("bytes ").unwrap_or(s);
    let s = s.split('/').next().unwrap_or(s);
    let start = s
        .split('-')
        .next()
        .unwrap_or("")
        .trim()
        .parse::<u64>()
        .map_err(|_| RegistryError::Other("invalid content range".into()))?;
    Ok(start)
}

/// Split an OCI repository name into (explicit-registry, repository).
///
/// A first path component that looks like a registry host (contains a dot or
/// colon, or equals "localhost") is stripped so `ghcr.io/foo/bar` and
/// `foo/bar` share the same local repository.
pub fn split_registry(name: &str) -> (String, String) {
    let Some(slash) = name.find('/') else {
        return (String::new(), name.to_string());
    };
    let first = &name[..slash];
    if first.contains('.') || first.contains(':') || first == "localhost" {
        (first.to_string(), name[slash + 1..].to_string())
    } else {
        (String::new(), name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_parse() {
        let ok = format!("sha256:{}", "a".repeat(64));
        assert_eq!(parse_digest(&ok).unwrap(), "a".repeat(64));
        assert!(parse_digest("sha256:xyz").is_err());
        assert!(parse_digest("md5:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").is_err());
        assert!(parse_digest("nosalt").is_err());
        let ok512 = format!("sha512:{}", "b".repeat(128));
        assert!(parse_digest(&ok512).is_ok());
    }

    #[test]
    fn blobs_from_manifest() {
        let body = br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json",
            "config":{"digest":"sha256:c1","mediaType":"application/vnd.oci.image.config.v1+json","size":1},
            "layers":[{"digest":"sha256:l1","mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","size":2}]}"#;
        let blobs = extract_blobs(body);
        assert_eq!(blobs.len(), 2);
        assert_eq!(blobs[0].digest, "sha256:c1");
        assert_eq!(blobs[1].digest, "sha256:l1");
    }

    #[test]
    fn blobs_from_index() {
        let body = br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json",
            "manifests":[{"digest":"sha256:m1","mediaType":"application/vnd.oci.image.manifest.v1+json","size":3}]}"#;
        let blobs = extract_blobs(body);
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs[0].digest, "sha256:m1");
    }

    #[test]
    fn subject_and_artifact_type() {
        let body = br#"{"mediaType":"application/vnd.oci.image.manifest.v1+json",
            "artifactType":"application/vnd.example+type",
            "subject":{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:s1","size":4},
            "config":{"mediaType":"application/vnd.oci.empty.v1+json"},"layers":[]}"#;
        assert_eq!(manifest_subject(body).unwrap(), "sha256:s1");
        assert_eq!(manifest_artifact_type(body).unwrap(), "application/vnd.example+type");
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-9", 100).unwrap(), (0, 9));
        assert_eq!(parse_range("bytes=10-", 100).unwrap(), (10, 99));
        assert_eq!(parse_range("bytes=-5", 100).unwrap(), (95, 99));
        assert_eq!(parse_range("bytes=10-999", 100).unwrap(), (10, 99));
        assert!(parse_range("bytes=100-110", 100).is_err());
        assert!(parse_range("bytes=5-2", 100).is_err());
    }

    #[test]
    fn content_range() {
        assert_eq!(parse_content_range_start("bytes 10-20/100").unwrap(), 10);
        assert_eq!(parse_content_range_start("10-20").unwrap(), 10);
    }

    #[test]
    fn registry_split() {
        assert_eq!(split_registry("ghcr.io/foo/bar"), ("ghcr.io".into(), "foo/bar".into()));
        assert_eq!(split_registry("localhost:5000/foo"), ("localhost:5000".into(), "foo".into()));
        assert_eq!(split_registry("library/alpine"), ("".into(), "library/alpine".into()));
        assert_eq!(split_registry("alpine"), ("".into(), "alpine".into()));
    }
}
