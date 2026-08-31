use crate::*;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

fn job_json(j: &jjlab_core::db::JobRow) -> Value {
    json!({
        "id": j.id,
        "run_id": j.run_id,
        "name": j.name,
        "status": j.status,
        "exit_code": j.exit_code,
    })
}

pub async fn list_workflows(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    match state.db.list_workflows(&repo_id) {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .into_iter()
                .map(|w| {
                    json!({
                        "id": w.id, "name": w.name, "path": w.path,
                        "trigger": w.trigger, "enabled": w.enabled,
                    })
                })
                .collect();
            Json(json!({ "workflows": items })).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn dispatch_workflow(
    State(state): State<AppState>,
    Path((org, repo, id)): Path<(String, String, i64)>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    let logs_root = std::path::PathBuf::from(
        std::env::var("JJLAB_LOGS").unwrap_or_else(|_| "/data/logs".to_string()),
    );
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::actions::dispatch(&store, &db, &org, &repo, id, &logs_root))
    })
    .await;
    match r {
        Ok(Ok(run_ids)) => Json(json!({ "run_ids": run_ids })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn list_runs(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    match state.db.list_runs(&repo_id) {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    json!({
                        "id": r.id, "workflow_id": r.workflow_id,
                        "trigger_ref": r.trigger_ref, "status": r.status,
                    })
                })
                .collect();
            Json(json!({ "runs": items })).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn list_run_jobs(
    State(state): State<AppState>,
    Path((org, repo, run_id)): Path<(String, String, i64)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let run = match state.db.get_run(run_id) {
        Ok(Some(r)) => r,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, format!("run {run_id} not found")),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    if run.repo_id != repo_id {
        return json_err(StatusCode::NOT_FOUND, "run not in this repo".into());
    }
    match state.db.list_jobs(run_id) {
        Ok(rows) => {
            let items: Vec<Value> = rows.iter().map(job_json).collect();
            Json(json!({ "jobs": items })).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn job_logs(
    State(state): State<AppState>,
    Path((org, repo, job_id)): Path<(String, String, i64)>,
) -> Response {
    let job = match state.db.get_job(job_id) {
        Ok(Some(j)) => j,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, format!("job {job_id} not found")),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    match state.db.get_run(job.run_id) {
        Ok(Some(run)) if run.repo_id == format!("{org}/{repo}") => {}
        _ => return json_err(StatusCode::NOT_FOUND, "job not in this repo".into()),
    }
    let bytes = jjlab_git::actions::job_log(job.log_path.as_deref().unwrap_or_default());
    (StatusCode::OK, [("content-type", "text/plain")], bytes).into_response()
}
