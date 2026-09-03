use crate::*;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// `POST /repos/{org}/{repo}/rebase` — rebase `source` (snapshot) onto `dest`
/// (snapshot), advancing the dest bookmark. Conflicts are carried natively.
pub async fn rebase_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<RebaseBody>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    let (source, dest) = (body.source.clone(), body.dest.clone());
    let outcome = match run_jj(move || {
        pollster::block_on(jjlab_git::mutation::rebase_branch(
            &store, &db, &org, &repo, &source, &dest,
        ))
    })
    .await
    {
        Ok(o) => o,
        Err(resp) => return resp,
    };
    Json(json!({ "rebase": outcome })).into_response()
}

/// `POST /repos/{org}/{repo}/commits` — atomic commit carrying one or more
/// file actions (create/update/delete), GitLab Repository Commits API style.
/// Each action may carry an optimistic-lock base blob `sha`; a mismatch on any
/// action rejects the WHOLE commit (409) and nothing is written. On success a
/// single change id is returned.
pub async fn commits(State(state): State<AppState>, Path((org, repo)): Path<(String, String)>, Json(body): Json<CommitsBody>) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    let author = server_author();
    let branch = body.branch.clone();
    let message = if body.message.is_empty() {
        "commit".to_string()
    } else {
        body.message.clone()
    };
    let amend = body.amend;

    // Split actions into writes (create/update) and deletes (delete).
    let mut edits: Vec<jjlab_git::mutation::BatchEdit> = Vec::new();
    let mut deletes: Vec<jjlab_git::mutation::BatchDelete> = Vec::new();
    for a in &body.actions {
        use base64::Engine as _;
        let content = base64::engine::general_purpose::STANDARD
            .decode(&a.content_base64)
            .unwrap_or_default();
        let base_sha = if a.sha.is_empty() { None } else { Some(a.sha.clone()) };
        match a.action.as_str() {
            "delete" => deletes.push(jjlab_git::mutation::BatchDelete { path: a.path.clone(), base_sha }),
            _ => edits.push(jjlab_git::mutation::BatchEdit { path: a.path.clone(), content, base_sha }),
        }
    }
    if edits.is_empty() && deletes.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "no actions".into());
    }

    // Writes and deletes each run in their own atomic transaction; apply them
    // in order. Each closure gets its OWN clone of the captured state.
    let mut outcome: Option<jjlab_git::mutation::EditOutcome> = None;
    if !edits.is_empty() {
        let s2 = store.clone(); let d2 = db.clone();
        let o2 = org.clone(); let r2 = repo.clone(); let b2 = branch.clone();
        let m2 = message.clone(); let a2 = author.clone();
        let e = match run_jj(move || {
            pollster::block_on(jjlab_git::mutation::write_files(
                &s2, &d2, &o2, &r2, &b2, &edits, &m2, a2, amend,
            ))
        })
        .await {
            Ok(o) => o,
            Err(resp) => return resp,
        };
        outcome = Some(e);
    }
    if !deletes.is_empty() {
        let s2 = store.clone(); let d2 = db.clone();
        let o2 = org.clone(); let r2 = repo.clone(); let b2 = branch.clone();
        let m2 = message.clone(); let a2 = author.clone();
        let e = match run_jj(move || {
            pollster::block_on(jjlab_git::mutation::delete_files(
                &s2, &d2, &o2, &r2, &b2, &deletes, &m2, a2, amend,
            ))
        })
        .await {
            Ok(o) => o,
            Err(resp) => return resp,
        };
        outcome = Some(e);
    }
    let o = outcome.unwrap();
    Json(json!({ "sha": o.sha, "change_id": o.change_id })).into_response()
}

pub async fn create_repo(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<CreateRepoBody>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    let author = server_author();
    let (o2, r2) = (org.clone(), repo.clone());
    let default_branch = body.default_branch.clone();
    let res = run_jj(move || {
        pollster::block_on(jjlab_git::mutation::create_repo(
            &store, &db, &org, &repo, &default_branch, author,
        ))
    })
    .await;
    match res {
        Ok(()) => (
            StatusCode::CREATED,
            Json(json!({ "full_name": format!("{o2}/{r2}") })),
        )
            .into_response(),
        Err(resp) => resp,
    }
}

pub async fn delete_repo(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    let repo_id = format!("{org}/{repo}");
    match run_jj(move || {
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
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(resp) => resp,
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
    let db = state.db.clone();
    let (oid, nid) = (old_id.clone(), new_id.clone());
    let outcome = match db.run(move |db| db.rename_repo(&oid, &nid)).await {
        Ok(o) => o,
        Err(e) => return json_err(error_status(&e), e.to_string()),
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
            let (nid, oid) = (new_id.clone(), old_id.clone());
            let _ = db.run(move |db| db.rename_repo(&nid, &oid)).await;
            return json_err(StatusCode::BAD_REQUEST, e.to_string());
        }
    };
    match tokio::fs::rename(&old_dir, &new_dir).await {
        Ok(()) => Json(json!({ "full_name": new_id })).into_response(),
        Err(e) => {
            // Roll back the DB rename so metadata stays consistent.
            let (nid, oid) = (new_id.clone(), old_id.clone());
            let _ = db.run(move |db| db.rename_repo(&nid, &oid)).await;
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
    let target = body.target.clone();
    let change = body.change.clone();
    let sha = match run_jj(move || {
        pollster::block_on(jjlab_git::mutation::set_branch(
            &store, &db, &org, &repo, &name, &target, &change,
        ))
    })
    .await
    {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    Json(json!({ "name": n2, "sha": sha })).into_response()
}

pub async fn delete_branch_handler(
    State(state): State<AppState>,
    Path((org, repo, name)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    match run_jj(move || {
        pollster::block_on(jjlab_git::mutation::delete_branch(&store, &db, &org, &repo, &name))
    })
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(resp) => resp,
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
    let target = body.target.clone();
    let change = body.change.clone();
    let sha = match run_jj(move || {
        pollster::block_on(jjlab_git::mutation::set_tag(
            &store, &db, &org, &repo, &name, &target, &change,
        ))
    })
    .await
    {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    Json(json!({ "name": n2, "sha": sha })).into_response()
}

pub async fn delete_tag_handler(
    State(state): State<AppState>,
    Path((org, repo, name)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    match run_jj(move || {
        pollster::block_on(jjlab_git::mutation::delete_tag(&store, &db, &org, &repo, &name))
    })
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(resp) => resp,
    }
}

/// One commit action (GitLab Repository Commits API style).
#[derive(serde::Deserialize)]
pub struct CommitAction {
    pub action: String, // create | update | delete
    pub path: String,
    #[serde(default)]
    pub content_base64: String,
    #[serde(default)]
    pub sha: String,
}

/// Request body for `POST /repos/{org}/{repo}/commits`.
#[derive(serde::Deserialize)]
pub struct CommitsBody {
    #[serde(default = "default_branch")]
    pub branch: String,
    #[serde(default)]
    pub message: String,
    #[serde(default = "default_amend")]
    pub amend: bool,
    pub actions: Vec<CommitAction>,
}