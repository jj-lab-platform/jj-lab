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
    let db = state.db.clone();
    let rows = match db_run(&db, move |db| db.list_workflows(&repo_id)).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
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

pub async fn dispatch_workflow(
    State(state): State<AppState>,
    Path((org, repo, id)): Path<(String, String, i64)>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    let logs_root = std::path::PathBuf::from(
        std::env::var("JJLAB_LOGS").unwrap_or_else(|_| "/data/logs".to_string()),
    );
    let run_ids = match run_jj(move || {
        pollster::block_on(jjlab_git::actions::dispatch(&store, &db, &org, &repo, id, &logs_root))
    })
    .await
    {
        Ok(ids) => ids,
        Err(resp) => return resp,
    };
    Json(json!({ "run_ids": run_ids })).into_response()
}

pub async fn list_runs(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let db = state.db.clone();
    let rows = match db_run(&db, move |db| db.list_runs(&repo_id)).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
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

pub async fn list_run_jobs(
    State(state): State<AppState>,
    Path((org, repo, run_id)): Path<(String, String, i64)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let db = state.db.clone();
    let run = match db.run(move |db| db.get_run(run_id)).await {
        Ok(Some(r)) => r,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, format!("run {run_id} not found")),
        Err(e) => return json_err(error_status(&e), e.to_string()),
    };
    if run.repo_id != repo_id {
        return json_err(StatusCode::NOT_FOUND, "run not in this repo".into());
    }
    let rows = match db.run(move |db| db.list_jobs(run_id)).await {
        Ok(r) => r,
        Err(e) => return json_err(error_status(&e), e.to_string()),
    };
    let items: Vec<Value> = rows.iter().map(job_json).collect();
    Json(json!({ "jobs": items })).into_response()
}

pub async fn job_logs(
    State(state): State<AppState>,
    Path((org, repo, job_id)): Path<(String, String, i64)>,
) -> Response {
    let db = state.db.clone();
    let job = match db.run(move |db| db.get_job(job_id)).await {
        Ok(Some(j)) => j,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, format!("job {job_id} not found")),
        Err(e) => return json_err(error_status(&e), e.to_string()),
    };
    let repo_id = format!("{org}/{repo}");
    match db.run(move |db| db.get_run(job.run_id)).await {
        Ok(Some(run)) if run.repo_id == repo_id => {}
        _ => return json_err(StatusCode::NOT_FOUND, "job not in this repo".into()),
    }
    let bytes = jjlab_git::actions::job_log(job.log_path.as_deref().unwrap_or_default());
    (StatusCode::OK, [("content-type", "text/plain")], bytes).into_response()
}

/// `GET .../actions/jobs/{job_id}/logs/stream` — SSE tail of the job's log
/// file: replays existing content, then keeps streaming appended lines until
/// the job leaves `running` and the file goes quiet (B4).
pub async fn job_logs_stream(
    State(state): State<AppState>,
    Path((org, repo, job_id)): Path<(String, String, i64)>,
) -> Response {
    use axum::response::sse::{Event, KeepAlive};
    use axum::response::{Sse, IntoResponse as _};

    let db = state.db.clone();
    let job = match db.run(move |db| db.get_job(job_id)).await {
        Ok(Some(j)) => j,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, format!("job {job_id} not found")),
        Err(e) => return json_err(error_status(&e), e.to_string()),
    };
    let repo_id = format!("{org}/{repo}");
    let run_repo = db.run(move |db| db.get_run(job.run_id)).await;
    match run_repo {
        Ok(Some(run)) if run.repo_id == repo_id => {}
        _ => return json_err(StatusCode::NOT_FOUND, "job not in this repo".into()),
    }
    let path = job.log_path.clone().unwrap_or_default();
    let mut sent = 0usize; // bytes already streamed

    let stream = async_stream::stream! {
        let mut quiet = 0usize;
        loop {
            match tokio::fs::read(&path).await {
                Ok(bytes) if bytes.len() > sent => {
                    let chunk = String::from_utf8_lossy(&bytes[sent..]).to_string();
                    sent = bytes.len();
                    for line in chunk.lines() {
                        yield Ok::<_, std::convert::Infallible>(
                            Event::default().event("log").data(line.to_string()),
                        );
                    }
                    quiet = 0;
                }
                _ => {
                    quiet += 1;
                }
            }
            let done = job.status != "running";
            if done && quiet >= 2 {
                yield Ok::<_, std::convert::Infallible>(
                    Event::default().event("state").data(job.status.clone()),
                );
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}
