use crate::*;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;


pub async fn create_repo(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<CreateRepoBody>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    let author = server_author();
    let (o2, r2) = (org.clone(), repo.clone());
    match tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::mutation::create_repo(
            &store, &db, &org, &repo, &body.default_branch, author,
        ))
    })
    .await
    {
        Ok(Ok(())) => (
            StatusCode::CREATED,
            Json(json!({ "full_name": format!("{o2}/{r2}") })),
        )
            .into_response(),
        Ok(Err(e)) => json_err(StatusCode::CONFLICT, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn delete_repo(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    let repo_id = format!("{org}/{repo}");
    match tokio::task::spawn_blocking(move || {
        // Remove the on-disk store first; treat "not found" as already-gone
        // so that a re-DELETE also cleans any orphaned DB rows.
        match pollster::block_on(jjlab_git::mutation::delete_repo(&store, &org, &repo)) {
            Ok(()) => {}
            Err(jjlab_git::repo::RepoError::NotFound { .. }) => {}
            Err(e) => return Err(e),
        }
        db.delete_repo(&repo_id)
            .map_err(|e| jjlab_git::repo::RepoError::Other(e.to_string()))
    })
    .await
    {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn rename_repo(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<RenameRepoBody>,
) -> Response {
    let new_name = body.name;
    if let Err(e) = jjlab_core::validate_segment(&new_name, "repo") {
        return json_err(StatusCode::BAD_REQUEST, e);
    }
    let old_id = format!("{org}/{repo}");
    let new_id = format!("{org}/{new_name}");

    // DB rename first (single UPDATE, cascades repo_id everywhere).
    let outcome = state.db.rename_repo(&old_id, &new_id);
    let outcome = match outcome {
        Ok(o) => o,
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    match outcome {
        jjlab_core::db::RenameOutcome::NotFound => {
            return json_err(StatusCode::NOT_FOUND, format!("{old_id} not found"));
        }
        jjlab_core::db::RenameOutcome::Conflict => {
            return json_err(StatusCode::CONFLICT, format!("{new_id} already exists"));
        }
        jjlab_core::db::RenameOutcome::Ok => {}
    }

    let old_dir = state.store.repo_dir_checked(&org, &repo);
    let new_dir = state.store.repo_dir_checked(&org, &new_name);
    let (old_dir, new_dir) = match (old_dir, new_dir) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            let _ = state.db.rename_repo(&new_id, &old_id);
            return json_err(StatusCode::BAD_REQUEST, e.to_string());
        }
    };
    match tokio::fs::rename(&old_dir, &new_dir).await {
        Ok(()) => Json(json!({ "full_name": new_id })).into_response(),
        Err(e) => {
            // Roll back the DB rename so metadata stays consistent.
            let _ = state.db.rename_repo(&new_id, &old_id);
            json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("rename: {e}"))
        }
    }
}

pub async fn set_branch_handler(
    State(state): State<AppState>,
    Path((org, repo, name)): Path<(String, String, String)>,
    Json(body): Json<BranchBody>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    let n2 = name.clone();
    match tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::mutation::set_branch(
            &store, &db, &org, &repo, &name, &body.target,
        ))
    })
    .await
    {
        Ok(Ok(sha)) => Json(json!({ "name": n2, "sha": sha })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn delete_branch_handler(
    State(state): State<AppState>,
    Path((org, repo, name)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    match tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::mutation::delete_branch(&store, &db, &org, &repo, &name))
    })
    .await
    {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn set_tag_handler(
    State(state): State<AppState>,
    Path((org, repo, name)): Path<(String, String, String)>,
    Json(body): Json<TagBody>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    let n2 = name.clone();
    match tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::mutation::set_tag(
            &store, &db, &org, &repo, &name, &body.target,
        ))
    })
    .await
    {
        Ok(Ok(sha)) => Json(json!({ "name": n2, "sha": sha })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn delete_tag_handler(
    State(state): State<AppState>,
    Path((org, repo, name)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    match tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::mutation::delete_tag(&store, &db, &org, &repo, &name))
    })
    .await
    {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn create_file(
    State(state): State<AppState>,
    Path((org, repo, path)): Path<(String, String, String)>,
    Json(body): Json<FileBody>,
) -> Response {
    let content = match file_content(&body) {
        Ok(c) => c,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e),
    };
    let message = if body.message.is_empty() {
        format!("add {path}")
    } else {
        body.message.clone()
    };
    let store = state.store.clone();
    let db = state.db.clone();
    let author = server_author();
    let branch = body.branch.clone();
    let amend = body.amend;
    match tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::mutation::write_file(
            &store,
            &db,
            &org,
            &repo,
            &branch,
            &path,
            &content,
            &message,
            author,
            amend,
        ))
    })
    .await
    {
        Ok(Ok(outcome)) => (
            StatusCode::CREATED,
            Json(json!({ "sha": outcome.sha, "change_id": outcome.change_id })),
        )
            .into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn update_file(
    State(state): State<AppState>,
    Path((org, repo, path)): Path<(String, String, String)>,
    Json(body): Json<FileBody>,
) -> Response {
    let content = match file_content(&body) {
        Ok(c) => c,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e),
    };
    let message = if body.message.is_empty() {
        format!("update {path}")
    } else {
        body.message.clone()
    };
    let store = state.store.clone();
    let db = state.db.clone();
    let author = server_author();
    let branch = body.branch.clone();
    let amend = body.amend;
    match tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::mutation::write_file(
            &store,
            &db,
            &org,
            &repo,
            &branch,
            &path,
            &content,
            &message,
            author,
            amend,
        ))
    })
    .await
    {
        Ok(Ok(outcome)) => Json(json!({ "sha": outcome.sha, "change_id": outcome.change_id })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn delete_file_handler(
    State(state): State<AppState>,
    Path((org, repo, path)): Path<(String, String, String)>,
    Json(body): Json<FileBody>,
) -> Response {
    let message = if body.message.is_empty() {
        format!("delete {path}")
    } else {
        body.message.clone()
    };
    let store = state.store.clone();
    let db = state.db.clone();
    let author = server_author();
    let branch = body.branch.clone();
    let amend = body.amend;
    match tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::mutation::delete_file(
            &store, &db, &org, &repo, &branch, &path, &message, author, amend,
        ))
    })
    .await
    {
        Ok(Ok(outcome)) => Json(json!({ "sha": outcome.sha, "change_id": outcome.change_id })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
