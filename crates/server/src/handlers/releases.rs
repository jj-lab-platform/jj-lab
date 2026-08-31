use crate::*;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
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
    let _ = state.db.upsert_org(&org, &org);
    let _ = state.db.upsert_repo(&repo_id, &org, &repo, "main", None);
    let store = state.store.clone();
    let db2 = state.db.clone();
    let tag = body.tag_name.clone();
    let ensure = tokio::task::spawn_blocking(move || {
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
    .await;
    match ensure {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
    match state
        .db
        .create_release(&repo_id, &body.tag_name, &body.name, &body.body, body.draft, body.prerelease)
    {
        Ok(r) => (
            StatusCode::CREATED,
            Json(release_json(&r, state.db.list_release_assets(r.id).unwrap_or_default())),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn list_releases(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    match state.db.list_releases(&repo_id) {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    let assets = state.db.list_release_assets(r.id).unwrap_or_default();
                    release_json(&r, assets)
                })
                .collect();
            Json(json!({ "releases": items })).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn get_release(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    match state.db.get_release_by_tag(&repo_id, &tag) {
        Ok(Some(r)) => {
            let assets = state.db.list_release_assets(r.id).unwrap_or_default();
            Json(release_json(&r, assets)).into_response()
        }
        Ok(None) => json_err(StatusCode::NOT_FOUND, format!("release {tag} not found")),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn latest_release(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let all = match state.db.list_releases(&repo_id) {
        Ok(v) => v,
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let Some(first) = all.into_iter().find(|r| !r.draft && !r.prerelease) else {
        return json_err(StatusCode::NOT_FOUND, "no releases".into());
    };
    let assets = state.db.list_release_assets(first.id).unwrap_or_default();
    Json(release_json(&first, assets)).into_response()
}

pub async fn delete_release(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    match state.db.get_release_by_tag(&repo_id, &tag) {
        Ok(Some(r)) => match state.db.delete_release(r.id) {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        },
        Ok(None) => json_err(StatusCode::NOT_FOUND, format!("release {tag} not found")),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn upload_asset(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
    mut multipart: axum::extract::Multipart,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let Some(rel) = (match state.db.get_release_by_tag(&repo_id, &tag) {
        Ok(Some(r)) => Some(r),
        Ok(None) => None,
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }) else {
        return json_err(StatusCode::NOT_FOUND, format!("release {tag} not found"));
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
        let digest = match state.assets.put(&data) {
            Ok(d) => d,
            Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("store asset: {e}")),
        };
        match state
            .db
            .add_release_asset(rel.id, &name, data.len() as i64, &digest, &ct)
        {
            Ok(a) => saved.push(a),
            Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        }
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
    let asset = match state
        .db
        .get_release_by_tag(&repo_id, &tag)
        .ok()
        .flatten()
        .and_then(|r| state.db.get_release_asset(r.id, &name).ok().flatten())
    {
        Some(a) => a,
        None => return json_err(StatusCode::NOT_FOUND, format!("asset {name} not found")),
    };
    let file = match state.assets.open(&asset.digest) {
        Ok(Some(f)) => f,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, "asset blob missing".into()),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("open blob: {e}")),
    };
    let stream = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file));
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
    let Some(rel) = state.db.get_release_by_tag(&repo_id, &tag).ok().flatten() else {
        return json_err(StatusCode::NOT_FOUND, format!("release {tag} not found"));
    };
    match state.db.delete_release_asset(rel.id, &name) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
