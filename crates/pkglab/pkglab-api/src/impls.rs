//! Shared helpers used by the facade and the per-protocol wrappers.

use crate::types::BlobOutput;
use pkglab_common::Artifact;

/// The raw bytes and metadata of a stored artifact's blobs.
pub async fn read_blobs(registry: &pkglab_common::Registry, art: &Artifact) -> Vec<BlobOutput> {
    let mut out = Vec::new();
    for b in &art.blobs {
        if let Ok(Some(mut r)) = registry.blobs.open(&b.digest).await {
            let mut data = Vec::new();
            if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                out.push(BlobOutput {
                    name: b.name.clone(),
                    media_type: b.media_type.clone(),
                    sha256: b.hex().to_string(),
                    size: b.size,
                    data,
                });
            }
        }
    }
    out
}
