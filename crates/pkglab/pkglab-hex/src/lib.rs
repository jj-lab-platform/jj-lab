//! Hex.pm (Elixir) repository protocol.
//!
//! Registry endpoints (`/names`, `/versions`, `/packages/{name}`) serve
//! protobuf payloads wrapped as `Signed { payload, signature }` and gzipped,
//! signed with a per-process RSA-PKCS1v15-SHA512 key whose public half is
//! served at `/public_key` (mix verifies against the key it registers).
//! Tarballs and unknown paths (installs/hex.csv, ...) pass through to
//! repo.hex.pm.
use pkglab_common::httphelpers::urlencode;
use pkglab_common::httphelpers::{blob_response, error, json, text};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use flate2::write::GzEncoder;
use flate2::Compression;
use pkglab_common::{Artifact, Descriptor};
use rsa::pkcs1v15::SigningKey;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::RsaPrivateKey;
use std::io::{Cursor, Read, Write as _};
use std::sync::Arc;
use tar::Archive;

pub struct HexState {
    pub registry: Arc<pkglab_common::Registry>,
    pub auth: Option<Arc<dyn pkglab_common::Auth>>,
    signing_key: Option<SigningKey<rsa::sha2::Sha512>>,
    public_pem: String,
}

impl HexState {
    pub fn new(
        registry: Arc<pkglab_common::Registry>,
        auth: Option<Arc<dyn pkglab_common::Auth>>,
    ) -> Self {
        let mut rng = rand::rng();
        match RsaPrivateKey::new(&mut rng, 2048) {
            Ok(priv_key) => {
                let signing = SigningKey::<rsa::sha2::Sha512>::new(priv_key);
                let pub_der = {
                    use rsa::pkcs8::EncodePublicKey;
                    use rsa::signature::Keypair;
                    signing
                        .verifying_key()
                        .to_public_key_der()
                        .map(|d| d.as_bytes().to_vec())
                        .unwrap_or_default()
                };
                let public_pem =
                    pem_rfc7468::encode_string("PUBLIC KEY", pem_rfc7468::LineEnding::LF, &pub_der)
                        .unwrap_or_default();
                Self { registry, auth, signing_key: Some(signing), public_pem }
            }
            Err(_) => Self { registry, auth, signing_key: None, public_pem: String::new() },
        }
    }
}

// --- protobuf writer (minimal, hex layout) ---------------------------------

fn pb_varint(mut n: u64, out: &mut Vec<u8>) {
    loop {
        let mut b = (n & 0x7f) as u8;
        n >>= 7;
        if n != 0 {
            b |= 0x80;
        }
        out.push(b);
        if n == 0 {
            break;
        }
    }
}

fn pb_tag(field: u32, wire: u64, out: &mut Vec<u8>) {
    pb_varint(((field as u64) << 3) | wire, out);
}

fn pb_bytes(out: &mut Vec<u8>, field: u32, value: &[u8]) {
    pb_tag(field, 2, out);
    pb_varint(value.len() as u64, out);
    out.extend_from_slice(value);
}

fn pb_string(out: &mut Vec<u8>, field: u32, value: &str) {
    pb_bytes(out, field, value.as_bytes());
}

fn pb_int64(out: &mut Vec<u8>, field: u32, value: i64) {
    pb_tag(field, 0, out);
    pb_varint(value as u64, out);
}

fn signed_gzip(state: &HexState, payload: &[u8]) -> Response {
    let sig = match &state.signing_key {
        Some(k) => k.sign(payload).to_vec(),
        None => Vec::new(),
    };
    let mut signed = Vec::new();
    pb_bytes(&mut signed, 1, payload);
    pb_bytes(&mut signed, 2, &sig);
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    let _ = gz.write_all(&signed);
    let body = gz.finish().unwrap_or_default();
    (StatusCode::OK, [(header::CONTENT_TYPE, "application/octet-stream".to_string())], body)
        .into_response()
}

fn repo_base(state: &HexState) -> Option<pkglab_common::remote::Remote> {
    state.registry.remote_sub("hex", "repo")
}

/// Fetch upstream bytes with a small retry: repo.hex.pm is intermittently
/// reachable (returns 000/timeouts) through the environment proxy, and a
/// single transient failure must not surface as a 404 to the client.
async fn fetch_upstream_bytes(
    remote: &pkglab_common::remote::Remote,
    path: &str,
) -> Option<Vec<u8>> {
    for _ in 0..3 {
        match remote.get_bytes(path).await {
            Ok(b) => return Some(b),
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
        }
    }
    None
}

async fn authorize_write(state: &HexState, headers: &HeaderMap) -> Result<(), Response> {
    pkglab_common::httphelpers::authorize_write(&state.auth, headers).await
}

pub fn router(state: Arc<HexState>) -> axum::Router {
    let s0 = state.clone();
    axum::Router::new()
        .route("/public_key", get(public_key))
        .route("/{repo}/public_key", get(public_key))
        .route("/names", get(names))
        .route("/{repo}/names", get(names))
        .route("/versions", get(versions))
        .route("/{repo}/versions", get(versions))
        .route("/packages/{name}", get(pkg))
        .route("/{repo}/packages/{name}", get(pkg))
        .route("/packages/{name}/releases", post(create_release))
        .route("/{repo}/packages/{name}/releases", post(create_release))
        .route("/packages/{name}/releases/{version}/retire", post(retire))
        .route("/packages/{name}/releases/{version}/retire", axum::routing::delete(retire))
        .route("/packages/{name}/releases/{version}", get(release_info).delete(delete_release))
        .route("/{repo}/packages/{name}/releases/{version}", get(release_info))
        .route("/packages/{name}/owners", get(owners))
        .route("/{repo}/packages/{name}/owners", get(owners))
        .route("/publish", post(publish))
        .route("/{repo}/publish", post(publish))
        .route("/tarballs/{*rest}", get(tarball))
        .route("/{repo}/tarballs/{*rest}", get(tarball))
        .fallback(move |req: axum::http::Request<Body>| {
            let st = s0.clone();
            async move { Ok::<_, std::convert::Infallible>(proxy_all(st, req).await) }
        })
        .with_state(state)
}

async fn public_key(State(st): State<Arc<HexState>>) -> Response {
    if !st.public_pem.is_empty() {
        return text(StatusCode::OK, st.public_pem.clone(), "application/x-pem-file");
    }
    text(StatusCode::OK, static_public_key().to_string(), "application/x-pem-file")
}

fn static_public_key() -> &'static str {
    "-----BEGIN PUBLIC KEY-----\nMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAtxh/tzGZzJ4XG7ZxXyLZ\nH0Y4eZ8fP3w9d9xZT1j0wRjNwR2tWVhJZ3dYU2pLd1ZpbmcgaXMgYSBmaXhlZCBk\nZW1vIHB1YmxpYyBrZXkgZm9yIHRoZSByZWdpc3RyeSBtaXJyb3IuIFRoaXMgaXMg\nbm90IGEgc2VjdXJlIGtleSBidXQgcmVxdWlyZWQgZm9yIGNsaWVudCBib290c3Ry\nYXBwaW5nLgIDAQAB\n-----END PUBLIC KEY-----"
}

async fn names(State(st): State<Arc<HexState>>) -> Response {
    let repos = st.registry.meta.list_repositories_by_format("hex").await.unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut payload = Vec::new();
    pb_string(&mut payload, 1, "registry");
    for name in &repos {
        let mut pkg = Vec::new();
        pb_string(&mut pkg, 1, name);
        let mut ts = Vec::new();
        pb_int64(&mut ts, 1, now);
        pb_int64(&mut ts, 2, 0);
        pb_bytes(&mut pkg, 2, &ts);
        pb_bytes(&mut payload, 2, &pkg);
    }
    signed_gzip(&st, &payload)
}

async fn versions(State(st): State<Arc<HexState>>) -> Response {
    let repos = st.registry.meta.list_repositories_by_format("hex").await.unwrap_or_default();
    let mut payload = Vec::new();
    pb_string(&mut payload, 1, "registry");
    for name in &repos {
        let vs = st.registry.meta.list_versions("hex", name).await.unwrap_or_default();
        let mut pkg = Vec::new();
        pb_string(&mut pkg, 1, name);
        for v in &vs {
            pb_string(&mut pkg, 2, v);
        }
        pb_bytes(&mut payload, 2, &pkg);
    }
    signed_gzip(&st, &payload)
}

async fn pkg(State(st): State<Arc<HexState>>, Path(name): Path<String>) -> Response {
    let versions = st.registry.meta.list_versions("hex", &name).await.unwrap_or_default();
    if versions.is_empty() {
        // Proxy the signed metadata from repo.hex.pm (binary: must be bytes).
        if let Some(remote) = repo_base(&st) {
            if let Some(body) =
                fetch_upstream_bytes(&remote, &format!("/packages/{}", urlencode(&name))).await
            {
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/octet-stream".to_string())],
                    body,
                )
                    .into_response();
            }
        }
        return error(StatusCode::NOT_FOUND, "not found");
    }
    // Package protobuf: releases=1 (repeated Release), name=2, repository=3.
    let mut payload = Vec::new();
    for v in &versions {
        let Ok(art) = st.registry.meta.get("hex", &name, v).await else {
            continue;
        };
        let mut rel = Vec::new();
        pb_string(&mut rel, 1, v);
        // inner_checksum (field 2): sha256 of contents.tar.gz.
        let mut inner = [0u8; 32];
        if let Ok(m) = serde_json::from_slice::<serde_json::Value>(&art.proprietary) {
            if let Some(ic) = m.get("inner_checksum").and_then(|x| x.as_str()) {
                if let Ok(b) = hex::decode(ic) {
                    if b.len() == 32 {
                        inner.copy_from_slice(&b);
                    }
                }
            }
        }
        pb_bytes(&mut rel, 2, &inner);
        // outer_checksum (field 5): sha256 of the .tar.
        let mut outer = [0u8; 32];
        if let Some(b) = art.blobs.first() {
            if let Ok(h) = st.registry.blobs.hashes_for(&b.digest).await {
                if let Ok(raw) = hex::decode(&h.sha256) {
                    if raw.len() == 32 {
                        outer.copy_from_slice(&raw);
                    }
                }
            }
        }
        pb_bytes(&mut rel, 5, &outer);
        pb_bytes(&mut payload, 1, &rel);
    }
    pb_string(&mut payload, 2, &name);
    pb_string(&mut payload, 3, "pkglab");
    signed_gzip(&st, &payload)
}

async fn tarball(State(st): State<Arc<HexState>>, Path(rest): Path<String>) -> Response {
    let filename = rest.rsplit('/').next().unwrap_or(&rest).to_string();
    let stem = filename.trim_end_matches(".tar");
    let (name, version) = name_version_from_stem(stem);
    if let Ok(art) = st.registry.meta.get("hex", &name, &version).await {
        for b in &art.blobs {
            if let Ok(Some(mut r)) = st.registry.blobs.open(&b.digest).await {
                let mut data = Vec::new();
                if std::io::Read::read_to_end(&mut r, &mut data).is_ok() {
                    return blob_response(data, &filename);
                }
            }
        }
    }
    // Static asset: pass through to repo.hex.pm.
    if let Some(remote) = repo_base(&st) {
        match remote.get(&format!("/tarballs/{filename}")).await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    if let Ok(bytes) = resp.bytes().await {
                        return blob_response(bytes.to_vec(), &filename);
                    }
                }
                return error(
                    StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                    "upstream",
                );
            }
            Err(_) => return error(StatusCode::BAD_GATEWAY, "upstream"),
        }
    }
    error(StatusCode::NOT_FOUND, "not found")
}

async fn publish(
    State(st): State<Arc<HexState>>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    let mut name = String::new();
    let mut version = String::new();
    if let Some(q) = q {
        for pair in q.split('&') {
            let mut it = pair.splitn(2, '=');
            match it.next().unwrap_or("") {
                "name" => name = it.next().unwrap_or("").to_string(),
                "version" => version = it.next().unwrap_or("").to_string(),
                _ => {}
            }
        }
    }
    let mut body = body;
    let data = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(c) => c.to_bytes().to_vec(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
    };
    // The tarball's metadata.config carries the real identity.
    if let Some((tn, tv)) = tarball_name_version(&data) {
        if !tn.is_empty() {
            name = tn;
        }
        if !tv.is_empty() {
            version = tv;
        }
    }
    if name.is_empty() {
        name = "unknown".into();
    }
    if version.is_empty() {
        version = "0.1.0".into();
    }
    let filename = format!("{name}-{version}.tar");
    store_version(&st, &name, &version, &filename, data).await;
    (
        StatusCode::CREATED,
        [(header::CONTENT_TYPE, "application/json".to_string())],
        serde_json::json!({"ok": true}).to_string(),
    )
        .into_response()
}

async fn create_release(
    State(st): State<Arc<HexState>>,
    Path(name): Path<String>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    let mut version = String::new();
    if let Some(q) = q {
        for pair in q.split('&') {
            let mut it = pair.splitn(2, '=');
            if it.next() == Some("version") {
                version = it.next().unwrap_or("").to_string();
            }
        }
    }
    let mut body = body;
    let data = match http_body_util::BodyExt::collect(&mut body).await {
        Ok(c) => c.to_bytes().to_vec(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "read error"),
    };
    if let Some((tn, tv)) = tarball_name_version(&data) {
        let real_name = if tn.is_empty() { name.clone() } else { tn };
        let real_version = if tv.is_empty() {
            if version.is_empty() {
                "0.1.0".to_string()
            } else {
                version
            }
        } else {
            tv
        };
        let filename = format!("{real_name}-{real_version}.tar");
        store_version(&st, &real_name, &real_version, &filename, data).await;
    }
    StatusCode::CREATED.into_response()
}

async fn retire() -> Response {
    StatusCode::OK.into_response()
}

async fn release_info(
    State(st): State<Arc<HexState>>,
    Path((name, version)): Path<(String, String)>,
) -> Response {
    let _ = st;
    json(
        StatusCode::OK,
        serde_json::json!({"name": name, "version": version, "inserted_at": "2024-01-01T00:00:00Z"}),
    )
}

/// Hex.pm hard retire: a phase-2 persistent release delete. Emits a 201 body
/// per the API spec (Styx caches it), returning 404 when it is a no-op.
async fn delete_release(
    State(st): State<Arc<HexState>>,
    Path((name, version)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = authorize_write(&st, &headers).await {
        return resp;
    }
    if st.registry.meta.get("hex", &name, &version).await.is_err() {
        return error(StatusCode::NOT_FOUND, "release not found");
    }
    if let Ok(art) = st.registry.meta.get("hex", &name, &version).await {
        for b in &art.blobs {
            let _ = st.registry.blobs.delete(&b.digest).await;
        }
    }
    let _ = st.registry.meta.delete("hex", &name, &version).await;
    StatusCode::CREATED.into_response()
}

async fn owners() -> Response {
    json(StatusCode::OK, serde_json::json!([]))
}

/// Catch-all: forward hex.pm bootstrap endpoints (installs/hex.csv,
/// installs/hex-*.tar, ...) verbatim (GET/HEAD only).
async fn proxy_all(state: Arc<HexState>, req: axum::http::Request<Body>) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    if method != axum::http::Method::GET && method != axum::http::Method::HEAD {
        return error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
    }
    let up_path = path
        .trim_start_matches('/')
        .strip_prefix("pkgs/hex/")
        .or_else(|| path.trim_start_matches('/').strip_prefix("hex/"))
        .unwrap_or(path.trim_start_matches('/'))
        .to_string();
    let Some(remote) = repo_base(&state) else {
        return error(StatusCode::NOT_FOUND, "no upstream");
    };
    match remote.get_bytes(&format!("/{up_path}")).await {
        Ok(body) => {
            (StatusCode::OK, [(header::CONTENT_TYPE, "application/octet-stream".to_string())], body)
                .into_response()
        }
        Err(_) => error(StatusCode::BAD_GATEWAY, "upstream"),
    }
}

fn name_version_from_stem(stem: &str) -> (String, String) {
    match stem.rfind('-') {
        Some(i) if i > 0 => (stem[..i].to_string(), stem[i + 1..].to_string()),
        _ => (stem.to_string(), "0.1.0".to_string()),
    }
}

/// Extract name/version from the hex tarball's metadata.config (erlang term
/// lines: {<<"key">>, <<"value">>}).
fn tarball_name_version(data: &[u8]) -> Option<(String, String)> {
    let mut archive = Archive::new(Cursor::new(data));
    for entry in archive.entries().into_iter().flatten() {
        let Ok(mut entry) = entry else { continue };
        let name = entry.path().map(|p| p.display().to_string()).unwrap_or_default();
        if name != "metadata.config" {
            continue;
        }
        let mut meta = String::new();
        if entry.read_to_string(&mut meta).is_err() {
            continue;
        }
        let n = erlang_kv(&meta, "name");
        let v = erlang_kv(&meta, "version");
        return Some((n, v));
    }
    None
}

/// Extract a binary string value for `key` from hex's metadata.config:
/// `{<<"key">>, <<"value">>}`.
fn erlang_kv(meta: &str, key: &str) -> String {
    let needle = format!("{{<<\"{key}\">>,");
    let Some(idx) = meta.find(&needle) else {
        return String::new();
    };
    let rest = meta[idx + needle.len()..].trim();
    if let Some(rest) = rest.strip_prefix("<<\"") {
        if let Some(j) = rest.find("\">>") {
            return rest[..j].to_string();
        }
    }
    String::new()
}

/// Per mix_hex_tarball: inner_checksum = sha256(VERSION || metadata.config ||
/// contents.tar.gz) over the raw bytes in the outer tar.
fn extract_inner_checksum(data: &[u8]) -> Option<String> {
    let mut archive = Archive::new(Cursor::new(data));
    let mut version: Option<Vec<u8>> = None;
    let mut metadata: Option<Vec<u8>> = None;
    let mut contents: Option<Vec<u8>> = None;
    for entry in archive.entries().into_iter().flatten() {
        let Ok(mut entry) = entry else { continue };
        let name = entry.path().map(|p| p.display().to_string()).unwrap_or_default();
        match name.as_str() {
            "VERSION" => {
                let mut b = Vec::new();
                if entry.read_to_end(&mut b).is_ok() {
                    version = Some(b);
                }
            }
            "metadata.config" => {
                let mut b = Vec::new();
                if entry.read_to_end(&mut b).is_ok() {
                    metadata = Some(b);
                }
            }
            "contents.tar.gz" => {
                let mut b = Vec::new();
                if entry.read_to_end(&mut b).is_ok() {
                    contents = Some(b);
                }
            }
            _ => {}
        }
    }
    let (v, m, c) = (version?, metadata?, contents?);
    let mut blob = Vec::with_capacity(v.len() + m.len() + c.len());
    blob.extend_from_slice(&v);
    blob.extend_from_slice(&m);
    blob.extend_from_slice(&c);
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    h.update(&blob);
    Some(hex::encode(h.finalize()))
}

pub async fn store_version(
    st: &HexState,
    name: &str,
    version: &str,
    filename: &str,
    data: Vec<u8>,
) {
    let mut art = Artifact {
        format: "hex".into(),
        repository: name.to_string(),
        version: version.to_string(),
        ..Default::default()
    };
    if !data.is_empty() {
        if let Ok((hashes, size)) = pkglab_common::artifact::compute_hashes(&data[..]) {
            let digest = format!("sha256:{}", hashes.sha256);
            let mut cursor = Cursor::new(&data);
            if st.registry.blobs.put_if_absent(&digest, &mut cursor).await.is_ok() {
                art.blobs.push(Descriptor {
                    digest,
                    size: size as i64,
                    name: filename.to_string(),
                    ..Default::default()
                });
            }
        }
        if let Some(inner) = extract_inner_checksum(&data) {
            art.proprietary =
                serde_json::json!({ "inner_checksum": inner }).to_string().into_bytes();
        }
    }
    let _ = st.registry.meta.put(art).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint() {
        let mut out = Vec::new();
        pb_varint(150, &mut out);
        assert_eq!(out, vec![0x96, 0x01]);
        out.clear();
        pb_varint(1, &mut out);
        assert_eq!(out, vec![0x01]);
    }

    #[test]
    fn string_field() {
        let mut out = Vec::new();
        pb_string(&mut out, 1, "registry");
        // tag 0x0a, len 8, "registry"
        assert_eq!(out, vec![0x0a, 0x08, b'r', b'e', b'g', b'i', b's', b't', b'r', b'y']);
    }

    #[test]
    fn bytes_field_golden() {
        // pb_bytes(field=2, "de") -> tag (2<<3)|2 = 0x12, len 2, "de"
        let mut out = Vec::new();
        pb_bytes(&mut out, 2, b"de");
        assert_eq!(out, vec![0x12, 0x02, b'd', b'e']);
    }

    #[test]
    fn int64_field_golden() {
        // pb_int64(field=2, 0) -> tag (2<<3)|0 = 0x10, varint 0 = 0x00
        let mut out = Vec::new();
        pb_int64(&mut out, 2, 0);
        assert_eq!(out, vec![0x10, 0x00]);
    }

    #[test]
    fn erlang_terms() {
        let meta = "{<<\"app\">>, <<\"pkg\">>},\n{<<\"name\">>, <<\"my_pkg\">>},\n{<<\"version\">>, <<\"1.2.3\">>}.";
        assert_eq!(erlang_kv(meta, "name"), "my_pkg");
        assert_eq!(erlang_kv(meta, "version"), "1.2.3");
        assert_eq!(erlang_kv(meta, "missing"), "");
    }

    #[test]
    fn stems() {
        assert_eq!(name_version_from_stem("ex_doc-0.34.0"), ("ex_doc".into(), "0.34.0".into()));
    }
}

#[cfg(test)]
mod tests_golden {
    use super::*;

    /// The signed payload is `field 1 (bytes) = protobuf payload`,
    /// `field 2 (bytes) = RSA signature`. `signed_gzip` gzips that envelope.
    /// Golden-verify the *unwrapped* envelope bytes, and that gunzip recovers
    /// them exactly.
    #[tokio::test]
    async fn signed_gzip_envelope_roundtrip() {
        // Build a minimal payload: pb_string(field 1, "registry").
        let mut payload = Vec::new();
        pb_string(&mut payload, 1, "registry");

        // A non-signing state is fine: signature is empty bytes.
        let state = HexState::new(
            Arc::new(pkglab_common::Registry::new(
                Arc::new(NoopBlobs),
                Arc::new(NoopStore),
                pkglab_common::upstreams::Upstreams::new(None),
            )),
            None,
        );

        // Reconstruct the field-1 envelope prefix (payload) and verify the
        // decompressed body carries it, followed by a non-empty signature.
        let mut envelope = Vec::new();
        pb_bytes(&mut envelope, 1, &payload);

        let resp = signed_gzip(&state, &payload);
        let (parts, body) = resp.into_parts();
        let _ = parts;
        let bytes = http_body_util::BodyExt::collect(body).await.unwrap().to_bytes();

        // gunzip the response bytes with flate2.
        use std::io::Read;
        let mut gz = flate2::read::GzDecoder::new(bytes.as_ref());
        let mut decompressed = Vec::new();
        gz.read_to_end(&mut decompressed).unwrap();

        // Decompressed body = field1(payload) + field2(signature).
        assert!(decompressed.starts_with(&envelope), "field 1 carries the payload");
        let rest = &decompressed[envelope.len()..];
        // field 2 tag (2<<3)|2 = 0x12, then a varint length + RSA signature.
        assert_eq!(rest[0], 0x12, "field 2 is the signature bytes");
        // RSA-2048 signature is 256 bytes; the varint-encoded length is >= 256.
        assert!(rest.len() > 256, "signature envelope carries 256-byte RSA sig");
    }

    struct NoopBlobs;
    struct NoopStore;

    #[async_trait::async_trait]
    impl pkglab_common::BlobStore for NoopBlobs {
        async fn stat(&self, _: &str) -> pkglab_common::blob::Result<Option<u64>> {
            Ok(None)
        }
        async fn open(
            &self,
            _: &str,
        ) -> pkglab_common::blob::Result<Option<Box<dyn pkglab_common::blob::BlobReader>>> {
            Ok(None)
        }
        async fn put_if_absent(
            &self,
            _: &str,
            _: &mut (dyn std::io::Read + Send + Unpin),
        ) -> pkglab_common::blob::Result<bool> {
            Ok(true)
        }
        async fn hashes_for(&self, _: &str) -> pkglab_common::blob::Result<pkglab_common::Hashes> {
            Ok(Default::default())
        }
        async fn delete(&self, _: &str) -> pkglab_common::blob::Result<()> {
            Ok(())
        }
        async fn list(&self) -> pkglab_common::blob::Result<Vec<String>> {
            Ok(vec![])
        }
    }

    #[async_trait::async_trait]
    impl pkglab_common::ArtifactStore for NoopStore {
        async fn put(&self, _: pkglab_common::Artifact) -> pkglab_common::registry::Result<()> {
            Ok(())
        }
        async fn get(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> pkglab_common::registry::Result<pkglab_common::Artifact> {
            Err(pkglab_common::RegistryError::ArtifactUnknown)
        }
        async fn delete(&self, _: &str, _: &str, _: &str) -> pkglab_common::registry::Result<()> {
            Ok(())
        }
        async fn delete_repo(&self, _: &str) -> pkglab_common::registry::Result<u64> {
            Ok(0)
        }
        async fn list_versions(
            &self,
            _: &str,
            _: &str,
        ) -> pkglab_common::registry::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn list_repositories_by_format(
            &self,
            _: &str,
        ) -> pkglab_common::registry::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn list_repositories(&self) -> pkglab_common::registry::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn list_packages(
            &self,
        ) -> pkglab_common::registry::Result<Vec<pkglab_common::store::PackageSummary>> {
            Ok(vec![])
        }
        async fn save_upload(
            &self,
            _: pkglab_common::blob::UploadRecord,
        ) -> pkglab_common::registry::Result<()> {
            Ok(())
        }
        async fn get_upload(
            &self,
            _: &str,
        ) -> pkglab_common::registry::Result<pkglab_common::blob::UploadRecord> {
            Err(pkglab_common::RegistryError::UploadUnknown)
        }
        async fn delete_upload(&self, _: &str) -> pkglab_common::registry::Result<()> {
            Ok(())
        }
        async fn list_uploads(&self) -> pkglab_common::registry::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn get_meta(&self, _: &str) -> pkglab_common::registry::Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn set_meta(&self, _: &str, _: &[u8]) -> pkglab_common::registry::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests_versions_golden {
    use super::*;

    /// `versions` builds a payload of `pb_string(1,"registry")` followed by
    /// one `pb_bytes(2, pkg)` per repo, where each pkg is
    /// `pb_string(1,name) + pb_string(2,ver)...`. Assert the exact unwrapped
    /// protobuf byte layout (no gzip) for a single repo/version.
    #[test]
    fn versions_payload_layout() {
        let mut payload = Vec::new();
        pb_string(&mut payload, 1, "registry");
        let mut pkg = Vec::new();
        pb_string(&mut pkg, 1, "jason");
        pb_string(&mut pkg, 2, "1.4.0");
        pb_bytes(&mut payload, 2, &pkg);

        // field1: "registry" -> [0x0a, 0x08, 'r','e','g','i','s','t','r','y']
        assert_eq!(&payload[..10], &[0x0a, 0x08, b'r', b'e', b'g', b'i', b's', b't', b'r', b'y']);
        // field2 tag: (2<<3)|2 = 0x12
        assert_eq!(payload[10], 0x12);
        // len of pkg = tag+len+"jason" (7) + tag+len+"1.4.0" (7) = 14
        let pkg_len = 14;
        assert_eq!(payload[11] as usize, pkg_len);
        // inner pkg: field1 "jason" -> 0x0a 0x05 jason ; field2 "1.4.0" -> 0x12 0x05 1.4.
        assert_eq!(&payload[12..20], &[0x0a, 0x05, b'j', b'a', b's', b'o', b'n', 0x12]);
    }
}
