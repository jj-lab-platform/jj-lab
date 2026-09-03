//! Git release lifecycle backed entirely by the pkglab artifact store.
//!
//! A release is stored as an [`Artifact`] with `format = "release"`,
//! `repository = "<org>/<repo>"`, `version = "<tag>"`, and its `proprietary`
//! payload carrying [`ReleaseMeta`]; the binaries are referenced as `blobs`
//! (content-addressed via the blob store). This unifies releases with every
//! other package/artifact (generic, oci, npm, ...) so a single
//! `packages-search` can enumerate them and `packages-delete`/`packages-yank`
//! can manage them — no separate `releases`/`release_assets` tables.

use crate::*;

use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use pkglab_common::artifact::{sha256_hex, Descriptor, ReleaseMeta, FORMAT_RELEASE};
use pkglab_common::blob::{BlobReader, BlobStore};
use pkglab_common::{Artifact, Registry as CommonRegistry};
use std::io::Read as _;
use std::sync::Arc;
use serde_json::{json, Value};

// TODO(owning-metadata): the /releases/{tag}/assets/{name} form keeps a single
// asset. Assets are Descriptors on the artifact.

fn release_json(a: &Artifact, tag: &str) -> Value {
    let meta: ReleaseMeta = serde_json::from_slice(&a.proprietary).unwrap_or_default();
    let repo = a.repository.clone();
    let items: Vec<Value> = a
        .blobs
        .iter()
        .map(|b| {
            json!({
                "name": b.name, "size": b.size, "digest": b.hex(),
                "content_type": b.media_type,
                "browser_download_url": format!("/api/v1/repos/{repo}/releases/{tag}/assets/{}", b.name),
            })
        })
        .collect();
    json!({
        "id": a.version.clone(),
        "tag_name": tag,
        "name": meta.name,
        "body": meta.body,
        "draft": meta.draft,
        "prerelease": meta.prerelease,
        "assets": items,
    })
}

fn reg(state: &AppState) -> Result<Arc<CommonRegistry>, Response> {
    state
        .registry
        .clone()
        .ok_or_else(|| json_err(StatusCode::SERVICE_UNAVAILABLE, "package registry is not enabled".to_string()))
}

fn meta_of(reg: &Arc<CommonRegistry>) -> Arc<dyn pkglab_common::ArtifactStore> {
    reg.meta.clone()
}

fn blobs_of(reg: &Arc<CommonRegistry>) -> Arc<dyn BlobStore> {
    reg.blobs.clone()
}

fn release_meta_bytes(m: &ReleaseMeta) -> Vec<u8> {
    serde_json::to_vec(m).unwrap_or_default()
}

fn release_meta_of(a: &Artifact) -> ReleaseMeta {
    serde_json::from_slice(&a.proprietary).unwrap_or_default()
}

pub async fn create_release(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<ReleaseBody>,
) -> Response {
    let reg = match reg(&state) { Ok(r) => r, Err(e) => return e };
    let repo_id = format!("{org}/{repo}");
    let store = state.store.clone();
    let db = state.db.clone();
    let tag = body.tag_name.clone();
    let tag_for_git = tag.clone();

    // Ensure the git tag exists (mirrors the old behavior).
    if let Err(resp) = run_jj(move || {
        pollster::block_on(async {
            let repo_arc = jjlab_git::read::open(&store, &org, &repo).await?;
            match jjlab_git::read::resolve_snapshot(&repo_arc, &tag_for_git) {
                Ok(_) => Ok(()),
                Err(_) => {
                    let head = jjlab_git::read::head_sha(&store, &org, &repo).await?;
                    jjlab_git::mutation::set_tag(&store, &db, &org, &repo, &tag_for_git, &head, "")
                        .await
                        .map(|_| ())
                }
            }
        })
    })
    .await
    {
        return resp;
    }

    let meta = ReleaseMeta { name: body.name, body: body.body, draft: body.draft, prerelease: body.prerelease };
    let art = Artifact {
        format: FORMAT_RELEASE.into(),
        repository: repo_id.clone(),
        version: tag.clone(),
        proprietary: release_meta_bytes(&meta),
        source: "push".into(),
        repo: repo_id.clone(),
        bookmark: "main".into(),
        ..Default::default()
    };
    let m = meta_of(&reg);
    let put_res = m.put(art).await.map_err(|e| e.to_string());
    if let Err(msg) = put_res {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("store release: {msg}"));
    }
    // Fetch back the full artifact for the response.
    let got = match m.get(FORMAT_RELEASE, &repo_id, &tag).await {
        Ok(a) => a,
        Err(_) => Artifact {
            format: FORMAT_RELEASE.into(),
            repository: repo_id.clone(),
            version: tag.clone(),
            proprietary: release_meta_bytes(&meta),
            source: "push".into(),
            repo: repo_id.clone(),
            bookmark: "main".into(),
            ..Default::default()
        },
    };
    (StatusCode::CREATED, Json(release_json(&got, &tag))).into_response()
}

pub async fn list_releases(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let reg = match reg(&state) { Ok(r) => r, Err(e) => return e };
    let repo_id = format!("{org}/{repo}");
    let arts = match meta_of(&reg).list_artifacts(FORMAT_RELEASE, &repo_id).await {
        Ok(v) => v,
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let items: Vec<Value> = arts.iter().map(|a| release_json(a, &a.version)).collect();
    Json(json!({ "releases": items })).into_response()
}

pub async fn get_release(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
) -> Response {
    let reg = match reg(&state) { Ok(r) => r, Err(e) => return e };
    let repo_id = format!("{org}/{repo}");
    let a = match meta_of(&reg).get(FORMAT_RELEASE, &repo_id, &tag).await {
        Ok(a) => a,
        Err(pkglab_common::registry::RegistryError::ArtifactUnknown) => {
            return json_err(StatusCode::NOT_FOUND, format!("release {tag} not found"));
        }
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    Json(release_json(&a, &tag)).into_response()
}

pub async fn latest_release(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let reg = match reg(&state) { Ok(r) => r, Err(e) => return e };
    let repo_id = format!("{org}/{repo}");
    let arts = match meta_of(&reg).list_artifacts(FORMAT_RELEASE, &repo_id).await {
        Ok(v) => v,
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let Some(first) = arts.into_iter().find(|a| {
        let m = release_meta_of(a);
        !m.draft && !m.prerelease
    }) else {
        return json_err(StatusCode::NOT_FOUND, "no releases".into());
    };
    let tag = first.version.clone();
    Json(release_json(&first, &tag)).into_response()
}

pub async fn delete_release(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
) -> Response {
    let reg = match reg(&state) { Ok(r) => r, Err(e) => return e };
    let repo_id = format!("{org}/{repo}");
    // Remove the referenced blobs, then the artifact.
    let m = meta_of(&reg);
    let b = blobs_of(&reg);
    if let Ok(a) = m.get(FORMAT_RELEASE, &repo_id, &tag).await {
        for blob in &a.blobs {
            let _ = b.delete(&blob.digest).await;
        }
    }
    match m.delete(FORMAT_RELEASE, &repo_id, &tag).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn upload_asset(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
    mut multipart: Multipart,
) -> Response {
    let reg = match reg(&state) { Ok(r) => r, Err(e) => return e };
    let repo_id = format!("{org}/{repo}");

    let mut saved = Vec::new();
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.file_name().unwrap_or("asset.bin").to_string();
        let ct = field.content_type().unwrap_or("application/octet-stream").to_string();
        let Ok(data) = field.bytes().await else {
            return json_err(StatusCode::BAD_REQUEST, "failed to read upload".into());
        };
        let digest = sha256_hex(&data);
        let full_digest = format!("sha256:{digest}");
        {
            let mut cursor = std::io::Cursor::new(data.as_ref());
            if let Err(e) = blobs_of(&reg).put_if_absent(&full_digest, &mut cursor).await {
                return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("store asset: {e}"));
            }
        }
        // Append the asset to the release artifact's blobs.
        let m = meta_of(&reg);
        let mut art = match m.get(FORMAT_RELEASE, &repo_id, &tag).await {
            Ok(a) => a,
            Err(_) => {
                return json_err(StatusCode::NOT_FOUND, format!("release {tag} not found"));
            }
        };
        art.blobs.push(Descriptor {
            digest: full_digest.clone(),
            media_type: ct.clone(),
            size: data.len() as i64,
            name: name.clone(),
        });
        if let Err(e) = m.put(art).await {
            return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("update release: {e}"));
        }
        saved.push(json!({ "name": name, "size": data.len() as i64, "digest": digest }));
    }
    if saved.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "multipart body required".into());
    }
    (StatusCode::CREATED, Json(json!({ "assets": saved }))).into_response()
}

pub async fn download_asset(
    State(state): State<AppState>,
    Path((org, repo, tag, name)): Path<(String, String, String, String)>,
) -> Response {
    let reg = match reg(&state) { Ok(r) => r, Err(e) => return e };
    let repo_id = format!("{org}/{repo}");
    let m = meta_of(&reg);
    let art = match m.get(FORMAT_RELEASE, &repo_id, &tag).await {
        Ok(a) => a,
        Err(_) => return json_err(StatusCode::NOT_FOUND, format!("release {tag} not found")),
    };
    let Some(blob) = art.blobs.iter().find(|b| b.name == name).cloned() else {
        return json_err(StatusCode::NOT_FOUND, format!("asset {name} not found"));
    };
    let data = match blobs_of(&reg).open(&blob.digest).await {
        Ok(Some(mut r)) => {
            let mut buf = Vec::new();
            if r.read_to_end(&mut buf).is_ok() { buf } else { Vec::new() }
        }
        _ => {
            return json_err(StatusCode::NOT_FOUND, "asset blob missing".into());
        }
    };
    let ct = blob.media_type.clone();
    (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, ct)], data).into_response()
}

pub async fn delete_asset(
    State(state): State<AppState>,
    Path((org, repo, tag, name)): Path<(String, String, String, String)>,
) -> Response {
    let reg = match reg(&state) { Ok(r) => r, Err(e) => return e };
    let repo_id = format!("{org}/{repo}");
    let m = meta_of(&reg);
    let mut art = match m.get(FORMAT_RELEASE, &repo_id, &tag).await {
        Ok(a) => a,
        Err(_) => return json_err(StatusCode::NOT_FOUND, format!("release {tag} not found")),
    };
    let removed: Vec<Descriptor> = art.blobs.iter().filter(|b| b.name != name).cloned().collect();
    art.blobs = removed;
    if let Err(e) = m.put(art).await {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("update release: {e}"));
    }
    StatusCode::NO_CONTENT.into_response()
}

// Keep the trait import reachable for the BlobReader bound in older code paths.
fn _reader_marker(_: Box<dyn BlobReader>) {}
