//! Round-trip tests for the unified facade against the SQLite/Fs substrate.

use pkglab_api::types::{FileSpec, PublishRequest};
use pkglab_api::RegistryApi;
use std::sync::Arc;

fn substrate(dir: &std::path::Path) -> Arc<pkglab_common::Registry> {
    let reg = pkglab_core::Registry::open(dir).unwrap();
    // pkglab_core::Registry wraps blobs/meta/upstreams; rewrap into the common
    // surface the facade consumes.
    Arc::new(pkglab_common::Registry::new(
        reg.blobs.clone(),
        reg.meta.clone(),
        reg.upstreams.clone(),
    ))
}

#[tokio::test]
async fn facade_publish_fetch_delete() {
    let dir = tempfile::tempdir().unwrap();
    let api = RegistryApi::new(substrate(dir.path()));
    let npm = pkglab_api::client(api, "npm");

    npm.publish(PublishRequest {
        format: "npm".into(),
        name: "@scope/demo".into(),
        version: "1.0.0".into(),
        files: vec![FileSpec {
            name: String::new(),
            data: b"fake-tarball".to_vec(),
            media_type: "application/octet-stream".into(),
        }],
        metadata: serde_json::json!({"name": "@scope/demo", "version": "1.0.0"}),
    })
    .await
    .unwrap();

    // Blob name follows the npm convention.
    assert_eq!(npm.versions("@scope/demo").await.unwrap(), vec!["1.0.0".to_string()]);
    let blobs = npm.fetch("@scope/demo", "1.0.0").await.unwrap();
    assert_eq!(blobs.len(), 1);
    assert_eq!(blobs[0].name, "demo-1.0.0.tgz");
    assert_eq!(blobs[0].data, b"fake-tarball");

    // Delete removes metadata and blobs.
    npm.delete("@scope/demo", "1.0.0").await.unwrap();
    assert!(npm.artifact("@scope/demo", "1.0.0").await.is_err());
}

#[tokio::test]
async fn facade_neutral_repository_enumerates() {
    let dir = tempfile::tempdir().unwrap();
    let api = RegistryApi::new(substrate(dir.path()));

    api.put(pkglab_common::Artifact {
        format: "generic".into(),
        repository: "team/tool".into(),
        version: "2.0.0".into(),
        source: "push".into(),
        ..Default::default()
    })
    .await
    .unwrap();

    let pkgs = api.packages().await.unwrap();
    assert_eq!(pkgs.len(), 1);
    assert_eq!(pkgs[0].format, "generic");
    assert_eq!(pkgs[0].repository, "team/tool");
    assert_eq!(pkgs[0].versions, vec!["2.0.0".to_string()]);
}

#[tokio::test]
async fn facade_republish_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let api = RegistryApi::new(substrate(dir.path()));
    let npm = pkglab_api::client(api.clone(), "npm");

    let req = |bytes: &'static [u8]| PublishRequest {
        format: "npm".into(),
        name: "@scope/demo".into(),
        version: "1.0.0".into(),
        files: vec![FileSpec {
            name: String::new(),
            data: bytes.to_vec(),
            media_type: String::new(),
        }],
        metadata: serde_json::json!({"name": "@scope/demo"}),
    };

    npm.publish(req(b"v1")).await.unwrap();
    npm.publish(req(b"v2")).await.unwrap();

    let blobs = npm.fetch("@scope/demo", "1.0.0").await.unwrap();
    assert_eq!(blobs.len(), 1);
    assert_eq!(blobs[0].data, b"v2");
}
