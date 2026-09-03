use crate::*;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

/// Create an empty org (a first-class resource, independent of any repo).
pub async fn create_org(
    State(state): State<AppState>,
    Json(body): Json<CreateOrgBody>,
) -> Response {
    let db = state.db.clone();
    let name = body.name.clone();
    if let Err(msg) = jjlab_core::validate_segment(&name, "org") {
        return json_err(StatusCode::BAD_REQUEST, msg);
    }
    let n = name.clone();
    match db_run(&db, move |db| db.create_org(&n)).await {
        Ok(()) => (
            StatusCode::CREATED,
            Json(json!({ "org": { "name": name, "repo_count": 0 } })),
        )
            .into_response(),
        Err(resp) => resp,
    }
}

/// List every org with its repo count.
pub async fn list_orgs_rich(State(state): State<AppState>) -> Response {
    let db = state.db.clone();
    match db_run(&db, |db| db.list_orgs_with_counts()).await {
        Ok(orgs) => {
            let items: Vec<Value> = orgs
                .into_iter()
                .map(|(_id, name, count)| json!({ "name": name, "repo_count": count }))
                .collect();
            Json(json!({ "orgs": items })).into_response()
        }
        Err(resp) => resp,
    }
}

/// Fetch one org with its repo count.
pub async fn get_org(
    State(state): State<AppState>,
    Path(org): Path<String>,
) -> Response {
    let db = state.db.clone();
    let o = org.clone();
    match db_run(&db, move |db| db.get_org(&o)).await {
        Ok(Some((_id, name))) => {
            let id2 = name.clone();
            let repo_count: i64 = db_run(&db, move |db| db.count_org_repos(&id2))
                .await
                .unwrap_or(0);
            Json(json!({ "org": { "name": name, "repo_count": repo_count } })).into_response()
        }
        Ok(None) => json_err(StatusCode::NOT_FOUND, format!("org {org} not found")),
        Err(resp) => resp,
    }
}

/// Rename an org (name == id, cascading `org_id` to every child repo).
pub async fn rename_org(
    State(state): State<AppState>,
    Path(org): Path<String>,
    Json(body): Json<RenameOrgBody>,
) -> Response {
    let db = state.db.clone();
    let new = body.name.clone();
    if let Err(msg) = jjlab_core::validate_segment(&new, "org") {
        return json_err(StatusCode::BAD_REQUEST, msg);
    }
    let o = org.clone();
    let n2 = new.clone();
    match db_run(&db, move |db| db.rename_org(&o, &n2)).await {
        Ok(Some(())) => Json(json!({ "org": { "name": new } })).into_response(),
        Ok(None) => json_err(StatusCode::NOT_FOUND, format!("org {org} not found")),
        Err(resp) => resp,
    }
}

/// Delete an org, refusing when it still holds repos.
pub async fn delete_org(
    State(state): State<AppState>,
    Path(org): Path<String>,
) -> Response {
    let db = state.db.clone();
    let o = org.clone();
    match db_run(&db, move |db| db.delete_org(&o)).await {
        Ok(Some(())) => Json(json!({ "ok": true })).into_response(),
        Ok(None) => json_err(StatusCode::NOT_FOUND, format!("org {org} not found")),
        Err(resp) => resp,
    }
}

#[derive(Deserialize)]
pub struct CreateOrgBody {
    name: String,
}

#[derive(Deserialize)]
pub struct RenameOrgBody {
    name: String,
}
