//! Tests ported from the original Go implementation (core_test.go,
//! hashes_test.go, gc_test.go).

use crate::blob::FsBlobStore;
use crate::store::SqliteArtifactStore;
use pkglab_common::blob::BlobStore;
use pkglab_common::store::ArtifactStore;
use pkglab_common::{Artifact, Descriptor};
use std::io::Cursor;
use std::sync::Arc;

fn sha256_of(data: &[u8]) -> String {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    h.update(data);
    format!("sha256:{}", hex::encode(h.finalize()))
}

async fn put_blob(b: &FsBlobStore, content: &[u8]) -> String {
    let digest = sha256_of(content);
    let mut r = Cursor::new(content.to_vec());
    b.put_if_absent(&digest, &mut r).await.unwrap();
    digest
}

async fn stat_ok(b: &FsBlobStore, digest: &str) -> bool {
    b.stat(digest).await.unwrap().is_some()
}

#[tokio::test(flavor = "multi_thread")]
async fn blob_store_put_get_stat_delete() {
    let dir = tempfile::tempdir().unwrap();
    let b = FsBlobStore::new(dir.path()).unwrap();
    let content = b"hello world";
    let digest = sha256_of(content);

    let stored = {
        let mut r = Cursor::new(content.to_vec());
        b.put_if_absent(&digest, &mut r).await.unwrap()
    };
    assert!(stored, "expected blob to be stored");

    // Dedup: same content should not be stored twice.
    let stored = {
        let mut r = Cursor::new(content.to_vec());
        b.put_if_absent(&digest, &mut r).await.unwrap()
    };
    assert!(!stored, "expected blob to be deduplicated");

    let size = b.stat(&digest).await.unwrap().unwrap();
    assert_eq!(size as usize, content.len());

    {
        let mut reader = b.open(&digest).await.unwrap().unwrap();
        let mut got = Vec::new();
        std::io::Read::read_to_end(&mut reader, &mut got).unwrap();
        assert_eq!(got, content);
    }

    b.delete(&digest).await.unwrap();
    assert!(!stat_ok(&b, &digest).await, "expected blob gone");
}

#[tokio::test(flavor = "multi_thread")]
async fn blob_store_digest_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let b = FsBlobStore::new(dir.path()).unwrap();
    let sum = sha256_of(b"different");
    let mut r = Cursor::new(b"actual".to_vec());
    let err = b.put_if_absent(&sum, &mut r).await;
    assert!(err.is_err(), "expected digest mismatch error");
}

#[tokio::test(flavor = "multi_thread")]
async fn blob_store_hashes_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let b = FsBlobStore::new(dir.path()).unwrap();
    let data = b"hello hashes";
    let digest = put_blob(&b, data).await;
    let h = b.hashes_for(&digest).await.unwrap();
    assert_eq!(h.sha256, digest.strip_prefix("sha256:").unwrap());
    assert!(!h.md5.is_empty() && !h.sha1.is_empty(), "md5/sha1 empty");
}

#[tokio::test]
async fn metadata_store_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let m = SqliteArtifactStore::open(&dir.path().join("meta.sqlite")).unwrap();

    let a = Artifact {
        repository: "library/alpine".into(),
        format: "oci".into(),
        version: "3.19".into(),
        media_type: "application/vnd.oci.image.manifest.v1+json".into(),
        blobs: vec![Descriptor { digest: "sha256:abc".into(), size: 3, ..Default::default() }],
        ..Default::default()
    };
    m.put(a).await.unwrap();

    let got = m.get("oci", "library/alpine", "3.19").await.unwrap();
    assert_eq!(got.repository, "library/alpine");
    assert_eq!(got.version, "3.19");
    assert_eq!(got.blobs.len(), 1);
    assert_eq!(got.blobs[0].digest, "sha256:abc");

    let vs = m.list_versions("oci", "library/alpine").await.unwrap();
    assert_eq!(vs, vec!["3.19".to_string()]);

    let repos = m.list_repositories().await.unwrap();
    assert_eq!(repos, vec!["library/alpine".to_string()]);

    m.delete("oci", "library/alpine", "3.19").await.unwrap();
    let err = m.get("oci", "library/alpine", "3.19").await;
    assert!(matches!(err, Err(pkglab_common::RegistryError::ArtifactUnknown)));
}

#[tokio::test]
async fn cross_format_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let m = SqliteArtifactStore::open(&dir.path().join("meta.sqlite")).unwrap();

    // Same repo+version under two formats must coexist.
    m.put(Artifact {
        format: "pypi".into(),
        repository: "ops-e2e-pkg".into(),
        version: "0.2.0".into(),
        blobs: vec![Descriptor {
            digest: "sha256:pypi-blob".into(),
            size: 1,
            ..Default::default()
        }],
        ..Default::default()
    })
    .await
    .unwrap();
    m.put(Artifact {
        format: "cargo".into(),
        repository: "ops-e2e-pkg".into(),
        version: "0.2.0".into(),
        blobs: vec![Descriptor {
            digest: "sha256:cargo-blob".into(),
            size: 1,
            ..Default::default()
        }],
        ..Default::default()
    })
    .await
    .unwrap();

    let pv = m.list_versions("pypi", "ops-e2e-pkg").await.unwrap();
    let cv = m.list_versions("cargo", "ops-e2e-pkg").await.unwrap();
    assert_eq!(pv, vec!["0.2.0".to_string()]);
    assert_eq!(cv, vec!["0.2.0".to_string()]);

    let pa = m.get("pypi", "ops-e2e-pkg", "0.2.0").await.unwrap();
    let ca = m.get("cargo", "ops-e2e-pkg", "0.2.0").await.unwrap();
    assert_eq!(pa.blobs[0].digest, "sha256:pypi-blob");
    assert_eq!(ca.blobs[0].digest, "sha256:cargo-blob");
}

async fn open_registry(root: &std::path::Path) -> Arc<crate::Registry> {
    crate::Registry::open(root).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn gc_reclaims_orphan() {
    let dir = tempfile::tempdir().unwrap();
    let r = open_registry(dir.path()).await;

    let b = Arc::new(FsBlobStore::new(&dir.path().join("blobs").join("sha256")).unwrap());
    let referenced = put_blob(&b, b"referenced-layer").await;
    let orphan = put_blob(&b, b"orphan-layer").await;

    r.meta
        .put(Artifact {
            repository: "repo/a".into(),
            version: "latest".into(),
            blobs: vec![Descriptor { digest: referenced.clone(), size: 16, ..Default::default() }],
            ..Default::default()
        })
        .await
        .unwrap();

    let n = crate::gc::run(&r.meta, &r.blobs).await.unwrap();
    assert_eq!(n, 1, "expected 1 reclaimed");
    assert!(stat_ok(&b, &referenced).await, "referenced should survive");
    assert!(!stat_ok(&b, &orphan).await, "orphan should be reclaimed");
}

#[tokio::test(flavor = "multi_thread")]
async fn gc_retains_shared_blob() {
    let dir = tempfile::tempdir().unwrap();
    let r = open_registry(dir.path()).await;
    let b = Arc::new(FsBlobStore::new(&dir.path().join("blobs").join("sha256")).unwrap());

    let shared = put_blob(&b, b"shared").await;
    let only_a = put_blob(&b, b"only-a").await;
    let only_b = put_blob(&b, b"only-b").await;

    r.meta
        .put(Artifact {
            repository: "repo/x".into(),
            version: "v1".into(),
            blobs: vec![
                Descriptor { digest: shared.clone(), size: 6, ..Default::default() },
                Descriptor { digest: only_a.clone(), size: 6, ..Default::default() },
            ],
            ..Default::default()
        })
        .await
        .unwrap();
    r.meta
        .put(Artifact {
            repository: "repo/y".into(),
            version: "v1".into(),
            blobs: vec![
                Descriptor { digest: shared.clone(), size: 6, ..Default::default() },
                Descriptor { digest: only_b.clone(), size: 6, ..Default::default() },
            ],
            ..Default::default()
        })
        .await
        .unwrap();

    // Delete repo/x (v1) so only shared+b remain referenced.
    r.meta.delete("oci", "repo/x", "v1").await.unwrap();

    let n = crate::gc::run(&r.meta, &r.blobs).await.unwrap();
    assert_eq!(n, 1, "expected 1 reclaimed (only-a)");
    for d in [shared, only_b] {
        assert!(stat_ok(&b, &d).await, "blob {d} should survive");
    }
    assert!(!stat_ok(&b, &only_a).await, "only-a should be reclaimed");
}

#[tokio::test]
async fn upload_session_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let m = SqliteArtifactStore::open(&dir.path().join("meta.sqlite")).unwrap();

    let rec = pkglab_common::blob::UploadRecord {
        session_id: "sess-1".into(),
        repo: "repo/a".into(),
        tmp_path: "/tmp/x".into(),
        size: 42,
    };
    m.save_upload(rec.clone()).await.unwrap();
    let got = m.get_upload("sess-1").await.unwrap();
    assert_eq!(got.repo, "repo/a");
    assert_eq!(got.size, 42);

    let ids = m.list_uploads().await.unwrap();
    assert_eq!(ids, vec!["sess-1".to_string()]);

    m.delete_upload("sess-1").await.unwrap();
    assert!(matches!(
        m.get_upload("sess-1").await,
        Err(pkglab_common::RegistryError::UploadUnknown)
    ));
}

#[tokio::test]
async fn meta_kv_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let m = SqliteArtifactStore::open(&dir.path().join("meta.sqlite")).unwrap();
    assert_eq!(m.get_meta("nope").await.unwrap(), None);
    m.set_meta("swift-git:a.b", b"https://github.com/a/b.git").await.unwrap();
    assert_eq!(
        m.get_meta("swift-git:a.b").await.unwrap(),
        Some(b"https://github.com/a/b.git".to_vec())
    );
}
