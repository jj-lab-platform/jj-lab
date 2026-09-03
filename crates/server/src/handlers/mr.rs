use crate::*;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

fn mr_json(mr: &jjlab_core::db::MrRow, review_state: String) -> Value {
    json!({
        "number": mr.number,
        "title": mr.title,
        "body": mr.description,
        "author": mr.author,
        "state": mr.state,
        "head_change_id": mr.head_change_id,
        "head_sha": mr.head_sha,
        "head_branch": mr.head_branch,
        "base": mr.base_rev,
        "review_state": review_state,
    })
}

#[allow(clippy::result_large_err)]
fn load_mr(state: &AppState, org: &str, repo: &str, number: i64) -> Result<jjlab_core::db::MrRow, Response> {
    let repo_id = format!("{org}/{repo}");
    state
        .db
        .get_mr_by_number(&repo_id, number)
        .map_err(|e| gitea_err(error_status(&e), e.to_string()))?
        .ok_or_else(|| gitea_err(StatusCode::NOT_FOUND, format!("pull request {number} not found")))
}

pub async fn create_mr_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<CreateMrBody>,
) -> Response {
    // Resolve head rev to its change-id + sha so force-pushes can re-associate.
    let store = state.store.clone();
    let head = body.head.clone();
    let (o2, r2) = (org.clone(), repo.clone());
    let (head_change_id, head_sha) = match run_jj(move || {
        pollster::block_on(jjlab_git::read::resolve_rev(&store, &o2, &r2, &head))
    })
    .await
    {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let repo_id = format!("{org}/{repo}");
    let (org2, r2b, sha2) = (org.clone(), repo.clone(), head_sha.clone());
    let anchor_org = org.clone();
    let anchor_repo = repo.clone();
    let db = state.db.clone();
    let (title, descr, head, base, author) =
        (body.title, body.body, body.head, body.base, server_author().0);
    match db_run(&db, move |db| {
        db.upsert_org(&org, &org)?;
        db.upsert_repo(&repo_id, &org, &repo, "main", None)?;
        db.create_mr(
            &repo_id,
            &title,
            &descr,
            &author,
            &head_change_id,
            Some(&head_sha),
            Some(&head),
            &base,
        )
    })
    .await
    {
        Ok(mr) => {
            // Pin the GC anchor so the reviewed head snapshot outlives the
            // source bookmark (amend/rebase/delete would otherwise sweep it).
            let a = state.store.clone();
            let anchor_sha = mr.head_sha.clone().unwrap_or_default();
            let anchor_fut = jjlab_git::mr_anchor::set_mr_head(&a, &anchor_org, &anchor_repo, mr.number, &anchor_sha);
            if let Err(e) = anchor_fut.await {
                tracing::warn!(err = %e, mr = mr.number, "set MR head GC anchor failed");
            }
            // `on: pull_request` — enqueue CI runs at the MR's head snapshot.
            trigger_pull_request(&state, org2, r2b, sha2).await;
            (StatusCode::CREATED, Json(mr_json(&mr, "pending".to_string()))).into_response()
        }
        Err(resp) => resp,
    }
}

pub async fn list_mrs(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let db = state.db.clone();
    let state_filter = q.get("state").cloned();
    let mrs = match db_run(&db, move |db| db.list_mrs(&repo_id, state_filter.as_deref())).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let mut items = Vec::new();
    for mr in mrs {
        let db = db.clone();
        let rs = db_run(&db, move |db| db.mr_review_state(mr.id)).await.unwrap_or_default();
        items.push(mr_json(&mr, rs));
    }
    Json(json!({ "pull_requests": items })).into_response()
}

pub async fn get_mr_handler(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    let db = state.db.clone();
    let rs = db_run(&db, move |db| db.mr_review_state(mr.id)).await.unwrap_or_default();
    Json(mr_json(&mr, rs)).into_response()
}

pub async fn update_mr_handler(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<UpdateMrBody>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    let db = state.db.clone();
    let stat = body.state;
    let id = mr.id;
    let stat2 = stat.clone();
    let updated = match db_run(&db, move |db| db.update_mr(id, Some(&stat2), None, None)).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    // Terminal states release the GC anchor (merged / closed / reopen are all
    // reflected here; deleted MRs are pruned by the reconciler).
    if matches!(stat.as_str(), "merged" | "closed") {
        if let Err(e) = jjlab_git::mr_anchor::clear_mr_head(&state.store, &org, &repo, number).await {
            tracing::warn!(err = %e, mr = number, "clear MR head GC anchor failed");
        }
    }
    let rs = db_run(&db, move |db| db.mr_review_state(id)).await.unwrap_or_default();
    Json(mr_json(&updated, rs)).into_response()
}

pub async fn update_mr_head_handler(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<UpdateMrHeadBody>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    let store = state.store.clone();
    let head = body.head.clone();
    let (o2, r2) = (org.clone(), repo.clone());
    let (_, head_sha) = match run_jj(move || {
        pollster::block_on(jjlab_git::read::resolve_rev(&store, &o2, &r2, &head))
    })
    .await
    {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let db = state.db.clone();
    let id = mr.id;
    let sha_for_ci = head_sha.clone();
    let sha_update = head_sha.clone();
    let updated =
        match db_run(&db, move |db| db.update_mr(id, None, Some(&sha_update), Some(&sha_update))).await {
            Ok(u) => u,
            Err(resp) => return resp,
        };
    // Force-push refresh: move the GC anchor to the new head.
    if let Err(e) = jjlab_git::mr_anchor::set_mr_head(&state.store, &org, &repo, number, &head_sha).await {
        tracing::warn!(err = %e, mr = number, "re-anchor MR head GC anchor failed");
    }
    trigger_pull_request(&state, org, repo, sha_for_ci).await;
    let rs = db_run(&db, move |db| db.mr_review_state(id)).await.unwrap_or_default();
    Json(mr_json(&updated, rs)).into_response()
}

/// Fire `on: pull_request` workflows at `head_sha` (best-effort; failures
/// are logged and never fail the MR operation).
pub(crate) async fn trigger_pull_request(state: &AppState, org: String, repo: String, head_sha: String) {
    let store = state.store.clone();
    let db = state.db.clone();
    let logs_root = std::path::PathBuf::from(
        std::env::var("JJLAB_LOGS").unwrap_or_else(|_| "/data/logs".to_string()),
    );
    tokio::task::spawn_blocking(move || {
        if let Err(e) = pollster::block_on(jjlab_git::actions::on_pull_request(
            &store, &db, &org, &repo, &head_sha, &logs_root,
        )) {
            tracing::warn!(err = %e, "pull_request CI trigger failed");
        }
    });
}

pub async fn add_review(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<ReviewBody>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    let db = state.db.clone();
    let (author, st, body_, head_sha, id) =
        (server_author().0, body.state, body.body, mr.head_sha.clone(), mr.id);
    match db_run(&db, move |db| {
        db.add_mr_review(id, &author, &st, &body_, head_sha.as_deref())
    })
    .await
    {
        Ok(_) => {}
        Err(resp) => return resp,
    }
    let rs = db_run(&db, move |db| db.mr_review_state(id)).await.unwrap_or_default();
    Json(json!({ "review_state": rs })).into_response()
}

pub async fn list_reviews(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    let db = state.db.clone();
    let id = mr.id;
    let rows = match db_run(&db, move |db| db.list_mr_reviews(id)).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "reviewer": r.reviewer,
                "state": r.state,
                "body": r.body,
                "commit_sha": r.commit_sha,
            })
        })
        .collect();
    Json(json!({ "reviews": items })).into_response()
}

pub async fn add_comment(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<CommentBody>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    let db = state.db.clone();
    let (author, body_, path, head_sha, id) =
        (server_author().0, body.body, body.path, mr.head_sha.clone(), mr.id);
    match db_run(&db, move |db| {
        db.add_mr_comment(id, &author, &body_, path.as_deref(), head_sha.as_deref())
    })
    .await
    {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(resp) => resp,
    }
}

pub async fn list_comments(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    let db = state.db.clone();
    let id = mr.id;
    let rows = match db_run(&db, move |db| db.list_mr_comments(id)).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "author": r.author,
                "body": r.body,
                "path": r.path,
                "commit_sha": r.commit_sha,
            })
        })
        .collect();
    Json(json!({ "comments": items })).into_response()
}

/// Unified diff between MR base and head.
pub async fn mr_diff(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    let Some(head_sha) = mr.head_sha.clone() else {
        return json_err(StatusCode::CONFLICT, "mr has no head sha".into());
    };
    let store = state.store.clone();
    let base = mr.base_rev.clone();
    let patch = match run_jj(move || {
        pollster::block_on(jjlab_git::read::compare_patch(&store, &org, &repo, &base, &head_sha))
    })
    .await
    {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    Json(json!({ "diff": patch })).into_response()
}