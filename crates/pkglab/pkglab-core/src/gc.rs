//! GC: mark blobs reachable from any live artifact, sweep the rest.

use pkglab_common::store::ArtifactStore;
use pkglab_common::BlobStore;
use std::collections::HashSet;
use std::sync::Arc;

/// Collect the set of blobs referenced by any live artifact, then remove
/// unreferenced blobs from the store. Returns the number reclaimed.
pub async fn run(
    meta: &Arc<dyn ArtifactStore>,
    blobs: &Arc<dyn BlobStore>,
) -> pkglab_common::registry::Result<u64> {
    let mut live: HashSet<String> = HashSet::new();

    let pkgs = meta.list_packages().await?;
    for pkg in pkgs {
        for v in &pkg.versions {
            let art = match meta.get(&pkg.format, &pkg.repository, v).await {
                Ok(a) => a,
                Err(_) => continue,
            };
            for b in &art.blobs {
                live.insert(b.digest.clone());
                live.insert(b.hex().to_string());
            }
        }
    }

    let all = blobs.list().await?;
    let mut reclaimed = 0u64;
    for d in all {
        if live.contains(&d) || live.contains(&format!("sha256:{d}")) {
            continue;
        }
        if blobs.delete(&d).await.is_ok() {
            reclaimed += 1;
        }
    }
    Ok(reclaimed)
}
