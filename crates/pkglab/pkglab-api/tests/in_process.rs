//! In-process smoke test: the facade and the HTTP adapters share the SAME
//! substrate and MUST read each other's writes byte-for-byte, without ever
//! opening a socket. We drive the real `router()`s with
//! `tower::ServiceExt::oneshot`.

use http_body_util::BodyExt;
use pkglab_api::types::{FileSpec, PublishRequest};
use pkglab_api::RegistryApi;
use std::sync::Arc;
use tower::ServiceExt;

fn substrate(dir: &std::path::Path) -> Arc<pkglab_common::Registry> {
    let reg = pkglab_core::Registry::open(dir).unwrap();
    Arc::new(pkglab_common::Registry::new(
        reg.blobs.clone(),
        reg.meta.clone(),
        reg.upstreams.clone(),
    ))
}

/// Build the OCI router bound to the same substrate the facade uses.
fn oci_router(common: &Arc<pkglab_common::Registry>) -> axum::Router {
    pkglab_oci::router(Arc::new(pkglab_oci::OciState {
        blobs: common.blobs.clone(),
        meta: common.meta.clone(),
        upstreams: common.upstreams.clone(),
        auth: None,
        default_upstream: None,
        self_base: "http://127.0.0.1:0".into(),
    }))
}

/// Build the Conan router bound to the same substrate the facade uses.
fn conan_router(common: &Arc<pkglab_common::Registry>) -> axum::Router {
    pkglab_conan::router(Arc::new(pkglab_conan::ConanState {
        registry: common.clone(),
        auth: None,
        self_base: "http://127.0.0.1:0/pkgs/conan".into(),
    }))
}

#[tokio::test]
async fn oci_facade_write_http_read() {
    let dir = tempfile::tempdir().unwrap();
    let common = substrate(dir.path());
    let api = RegistryApi::new(common.clone());

    let config = b"{\"architecture\":\"amd64\",\"os\":\"linux\"}";
    let config_digest = pkglab_oci::manifest::sha256_digest(config);

    // Push the config blob + a manifest via the facade.
    let oci = pkglab_api::pkgs::oci::OciClient::new(api.clone());
    oci.push_blob(&config_digest, config).await.unwrap();

    let manifest = format!(
        "{{\"schemaVersion\":2,\"mediaType\":\"application/vnd.oci.image.manifest.v1+json\",\"config\":{{\"mediaType\":\"application/vnd.oci.image.config.v1+json\",\"digest\":\"{config_digest}\",\"size\":{}}},\"layers\":[]}}",
        config.len()
    );
    let client = pkglab_api::client(api.clone(), "oci");
    client
        .publish(PublishRequest {
            format: "oci".into(),
            name: "e2e/img".into(),
            version: "latest".into(),
            files: vec![FileSpec {
                name: String::new(),
                data: manifest.clone().into_bytes(),
                media_type: "application/vnd.oci.image.manifest.v1+json".into(),
            }],
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();

    // Read it back through the REAL HTTP router, in-process.
    let router = oci_router(&common);
    let resp = router
        .oneshot(
            axum::http::Request::builder()
                .uri("/v2/e2e/img/manifests/latest")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], manifest.as_bytes());
}

#[tokio::test]
async fn oci_http_write_facade_read() {
    let dir = tempfile::tempdir().unwrap();
    let common = substrate(dir.path());
    let api = RegistryApi::new(common.clone());

    let manifest = b"{\"schemaVersion\":2,\"mediaType\":\"application/vnd.oci.image.manifest.v1+json\",\"config\":{\"mediaType\":\"application/vnd.oci.image.config.v1+json\",\"digest\":\"sha256:0\",\"size\":0},\"layers\":[]}";
    let router = oci_router(&common);

    // Write via the HTTP router (PUT manifest by tag).
    let resp = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("PUT")
                .uri("/v2/e2e/img/manifests/v1")
                .header("Content-Type", "application/vnd.oci.image.manifest.v1+json")
                .body(axum::body::Body::from(manifest.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Read via the facade.
    let oci = pkglab_api::pkgs::oci::OciClient::new(api);
    let (bytes, media_type) = oci.get_manifest("e2e/img", "v1").await.unwrap();
    assert_eq!(bytes, manifest);
    assert_eq!(media_type, "application/vnd.oci.image.manifest.v1+json");
}

#[tokio::test]
async fn conan_facade_write_http_read() {
    let dir = tempfile::tempdir().unwrap();
    let common = substrate(dir.path());
    let api = RegistryApi::new(common.clone());

    // Publish a recipe via the facade (reusing pkglab_conan helpers).
    let client = pkglab_api::client(api, "conan");
    client
        .publish(PublishRequest {
            format: "conan".into(),
            name: "zlib".into(),
            version: "1.3.1".into(),
            files: vec![
                FileSpec {
                    name: "conanfile.py".into(),
                    data: b"from conan import ConanFile".to_vec(),
                    media_type: String::new(),
                },
                FileSpec {
                    name: "conanmanifest.txt".into(),
                    data: b"manifest".to_vec(),
                    media_type: String::new(),
                },
            ],
            metadata: serde_json::json!({"revision": "cafebabe"}),
        })
        .await
        .unwrap();

    // Read the files back through the REAL Conan HTTP router.
    let router = conan_router(&common);
    let resp = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/v2/conans/zlib/1.3.1/_/_/revisions/cafebabe/files/conanfile.py")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"from conan import ConanFile");
}
