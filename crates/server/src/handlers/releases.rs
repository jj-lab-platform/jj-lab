use crate::*;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use pkglab_common::blob::BlobStore;
use serde_json::{json, Value};

fn release_json(r: &jjlab_core::db::ReleaseRow, assets: Vec<jjlab_core::db::ReleaseAssetRow>) -> Value {
    let items: Vec<Value> = assets
        .into_iter()
        .map(|a| {
            json!({
                "name": a.name, "size": a.size, "digest": a.digest,
                "content_type": a.content_type,
                "browser_download_url": format!("/api/v1/repos/{}/releases/{}/assets/{}", r.repo_id, r.tag_name, a.name),
            })
        })
        .collect();
    json!({
        "id": r.id,
        "tag_name": r.tag_name,
        "name": r.name,
        "body": r.body,
        "draft": r.draft,
        "prerelease": r.prerelease,
        "assets": items,
    })
}

pub async fn create_release(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<ReleaseBody>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let db = state.db.clone();
    {
        let (org2, repo2, rid) = (org.clone(), repo.clone(), repo_id.clone());
        if let Err(resp) = db_run(&db, move |db| -> jjlab_core::Result<()> {
            db.upsert_org(&org2, &org2)?;
            db.upsert_repo(&rid, &org2, &repo2, "main", None)
        })
        .await
        {
            return resp;
        }
    }
    let store = state.store.clone();
    let db2 = state.db.clone();
    let tag = body.tag_name.clone();
    if let Err(resp) = run_jj(move || {
        pollster::block_on(async {
            let repo_arc = jjlab_git::read::open(&store, &org, &repo).await?;
            match jjlab_git::read::resolve_commit(&repo_arc, &tag) {
                Ok(_) => Ok(()),
                Err(_) => {
                    let head = jjlab_git::read::head_sha(&store, &org, &repo).await?;
                    jjlab_git::mutation::set_tag(&store, &db2, &org, &repo, &tag, &head)
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

    let (tag_name, name, body_, draft, prerelease) =
        (body.tag_name, body.name, body.body, body.draft, body.prerelease);
    let rid = repo_id.clone();
    let r = match db_run(&db, move |db| {
        db.create_release(&rid, &tag_name, &name, &body_, draft, prerelease)
    })
    .await
    {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let assets = db_run(&db, move |db| db.list_release_assets(r.id))
        .await
        .unwrap_or_default();
    (StatusCode::CREATED, Json(release_json(&r, assets))).into_response()
}

pub async fn list_releases(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let db = state.db.clone();
    let rows = match db_run(&db, move |db| db.list_releases(&repo_id)).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let mut items = Vec::new();
    for r in rows {
        let db = db.clone();
        let assets = db_run(&db, move |db| db.list_release_assets(r.id))
            .await
            .unwrap_or_default();
        items.push(release_json(&r, assets));
    }
    Json(json!({ "releases": items })).into_response()
}

pub async fn get_release(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let db = state.db.clone();
    let tag2 = tag.clone();
    let r = match db_run(&db, move |db| db.get_release_by_tag(&repo_id, &tag2)).await {
        Ok(Some(r)) => r,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, format!("release {tag} not found")),
        Err(resp) => return resp,
    };
    let assets = db_run(&db, move |db| db.list_release_assets(r.id))
        .await
        .unwrap_or_default();
    Json(release_json(&r, assets)).into_response()
}

pub async fn latest_release(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let db = state.db.clone();
    let all = match db_run(&db, move |db| db.list_releases(&repo_id)).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(first) = all.into_iter().find(|r| !r.draft && !r.prerelease) else {
        return json_err(StatusCode::NOT_FOUND, "no releases".into());
    };
    let assets = db_run(&db, move |db| db.list_release_assets(first.id))
        .await
        .unwrap_or_default();
    Json(release_json(&first, assets)).into_response()
}

pub async fn delete_release(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let db = state.db.clone();
    let tag2 = tag.clone();
    let r = match db_run(&db, move |db| db.get_release_by_tag(&repo_id, &tag2)).await {
        Ok(Some(r)) => r,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, format!("release {tag} not found")),
        Err(resp) => return resp,
    };
    match db_run(&db, move |db| db.delete_release(r.id)).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(resp) => resp,
    }
}

pub async fn upload_asset(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
    mut multipart: axum::extract::Multipart,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let db = state.db.clone();
    let tag2 = tag.clone();
    let rel = match db_run(&db, move |db| db.get_release_by_tag(&repo_id, &tag2)).await {
        Ok(Some(r)) => r,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, format!("release {tag} not found")),
        Err(resp) => return resp,
    };

    let mut saved = Vec::new();
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.file_name().unwrap_or("asset.bin").to_string();
        let ct = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let Ok(data) = field.bytes().await else {
            return json_err(StatusCode::BAD_REQUEST, "failed to read upload".into());
        };
        let digest = pkglab_common::artifact::sha256_hex(&data);
        let full_digest = format!("sha256:{digest}");
        {
            let mut cursor = std::io::Cursor::new(data.as_ref());
            if let Err(e) = state.assets.put_if_absent(&full_digest, &mut cursor).await {
                return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("store asset: {e}"));
            }
        }
        let db = db.clone();
        let rel_id = rel.id;
        let (nm, dg, size, ct2) = (name.clone(), digest.clone(), data.len() as i64, ct.clone());
        let a = match db_run(&db, move |db| db.add_release_asset(rel_id, &nm, size, &dg, &ct2)).await {
            Ok(a) => a,
            Err(resp) => return resp,
        };
        saved.push(a);
    }
    if saved.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "multipart body required".into());
    }
    (
        StatusCode::CREATED,
        Json(json!({ "assets": saved.iter().map(|a| json!({
            "name": a.name, "size": a.size, "digest": a.digest,
        })).collect::<Vec<_>>() })),
    )
        .into_response()
}

pub async fn download_asset(
    State(state): State<AppState>,
    Path((org, repo, tag, name)): Path<(String, String, String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let db = state.db.clone();
    let tag2 = tag.clone();
    let rel = match db_run(&db, move |db| db.get_release_by_tag(&repo_id, &tag2)).await {
        Ok(Some(r)) => r,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, format!("release {tag} not found")),
        Err(resp) => return resp,
    };
    let nm2 = name.clone();
    let asset = match db_run(&db, move |db| db.get_release_asset(rel.id, &nm2)).await {
        Ok(Some(a)) => a,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, format!("asset {name} not found")),
        Err(resp) => return resp,
    };
    let blob_key = format!("sha256:{}", asset.digest);
    let stream = match state.assets.open_file(&blob_key) {
        Ok(Some(f)) => tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(f)),
        Ok(None) => return json_err(StatusCode::NOT_FOUND, "asset blob missing".into()),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("open blob: {e}")),
    };
    (
        StatusCode::OK,
        [
            ("content-type", asset.content_type.as_str()),
            ("etag", format!("\"{}\"", asset.digest).as_str()),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response()
}

pub async fn delete_asset(
    State(state): State<AppState>,
    Path((org, repo, tag, name)): Path<(String, String, String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let db = state.db.clone();
    let tag2 = tag.clone();
    let rel = match db_run(&db, move |db| db.get_release_by_tag(&repo_id, &tag2)).await {
        Ok(Some(r)) => r,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, format!("release {tag} not found")),
        Err(resp) => return resp,
    };
    let nm2 = name.clone();
    match db_run(&db, move |db| db.delete_release_asset(rel.id, &nm2)).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(resp) => resp,
    }
}