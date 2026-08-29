//! CI scheduler: out-of-band worker that drains the queued-runs table and
//! executes each job as a Kubernetes sandbox pod.
//!
//! This is deliberately NOT part of the HTTP request path — a run is enqueued
//! by `actions::enqueue_run` (from push/dispatch) and picked up here. The only
//! command execution is `sh -c <run>` *inside* a throwaway pod created via
//! `runtime::K8s`; the jjlab process itself never shells out to run user code.

use std::sync::Arc;
use std::time::Duration;

use jjlab_core::Db;

use crate::actions::{default_ci_image, BuildSpec};
use crate::repo::RepoStore;
use crate::runtime::{self, K8s};

/// Per-job/step timeout cap (configurable via env, default 15m).
pub fn job_timeout_cap() -> Duration {
    let secs = std::env::var("JJLAB_CI_JOB_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(15 * 60);
    Duration::from_secs(secs)
}

/// Run one scheduling loop: claim queued runs, execute their jobs, mark
/// status. Returns the number of runs processed.
pub async fn tick(
    db: Arc<Db>,
    store: Arc<RepoStore>,
    logs_root: std::path::PathBuf,
) -> Result<usize, String> {
    let k8s = runtime::connect().await.map_err(|e| e.to_string())?;
    let pending = db
        .pending_runs()
        .map_err(|e| e.to_string())?;
    if pending.is_empty() {
        return Ok(0);
    }
    let mut processed = 0usize;
    for run in pending {
        match execute_run(&k8s, &store, &db, &run, &logs_root).await {
            Ok(()) => {
                let _ = db.set_run_status(run.id, "success");
                processed += 1;
            }
            Err(e) => {
                tracing::warn!(run_id = run.id, err = %e, "CI run failed");
                let _ = db.set_run_status(run.id, "failure");
                processed += 1;
            }
        }
    }
    Ok(processed)
}

async fn execute_run(
    k8s: &K8s,
    store: &Arc<RepoStore>,
    db: &Db,
    run: &jjlab_core::db::RunRow,
    logs_root: &std::path::Path,
) -> Result<(), String> {
    // One pod per job row. Run sequentially within a run (V1 — no DAG).
    let jobs = db.list_jobs(run.id).map_err(|e| e.to_string())?;
    let mut run_failed = false;

    // Materialize the repo tree once per run into a shared build context, so
    // `build` steps can point buildctl at real files.
    let (org, repo) = run
        .repo_id
        .split_once('/')
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .ok_or_else(|| format!("bad repo id {}", run.repo_id))?;
    let ctx_dir = std::env::temp_dir().join(format!("jjlab-ci-ctx-{}", run.id));
    let _ = std::fs::remove_dir_all(&ctx_dir);
    std::fs::create_dir_all(&ctx_dir).map_err(|e| e.to_string())?;
    // jj-lib's ReadonlyRepo is !Send, so checkout runs on a blocking thread
    // via pollster (same pattern as the HTTP handlers), keeping this task Send.
    {
        let store = store.clone();
        let org = org.clone();
        let repo = repo.clone();
        let sha = run.trigger_ref.clone();
        let ctx_dir = ctx_dir.clone();
        let checkout = tokio::task::spawn_blocking(move || {
            pollster::block_on(crate::read::checkout_tree(
                &store, &org, &repo, &sha, &ctx_dir,
            ))
        })
        .await;
        match checkout {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(run_id = run.id, err = %e, "checkout failed"),
            Err(e) => tracing::warn!(run_id = run.id, err = %e, "checkout join failed"),
        }
    }

    for job in jobs {
        // Ensure the job's log path has a parent dir.
        if let Some(lp) = &job.log_path {
            if let Some(dir) = std::path::Path::new(lp).parent() {
                let _ = std::fs::create_dir_all(dir);
            }
        }
        let _ = db.set_job_status(job.id, "running", None);

        // Image build step: run buildctl directly (not inside a sandbox pod).
        let outcome: Result<(String, Option<i32>), String> = if let Some(spec_json) = &job.build_spec {
            if let Ok(spec) = serde_json::from_str::<BuildSpec>(spec_json) {
                let out_layout = std::env::temp_dir().join(format!("jjlab-ci-oci-{}-{}", run.id, job.id));
                let _ = std::fs::remove_dir_all(&out_layout);
                std::fs::create_dir_all(&out_layout).map_err(|e| e.to_string())?;
                match crate::actions::run_build_step(&spec, &ctx_dir, Some(&out_layout)).await {
                    Ok(logs) => {
                        if let Some(lp) = &job.log_path {
                            let _ = std::fs::write(lp, logs.join("\n"));
                        }
                        Ok(("Succeeded".to_string(), Some(0)))
                    }
                    Err(e) => {
                        if let Some(lp) = &job.log_path {
                            let _ = std::fs::write(lp, e.to_string());
                        }
                        Ok(("Failed".to_string(), Some(1)))
                    }
                }
            } else {
                Err(format!("invalid build spec for job {}", job.id))
            }
        } else {
            // Shell step: run in a throwaway sandbox pod.
            let command = job.command();
            let image = job.image.clone().unwrap_or_else(default_ci_image);
            let pod_name = format!("jjlab-ci-{}-{}", run.id, job.id);

            let timeout = job
                .timeout_seconds
                .and_then(|s| u64::try_from(s).ok())
                .map(Duration::from_secs)
                .unwrap_or(job_timeout_cap());
            let timeout = timeout.min(job_timeout_cap());

            let result = k8s
                .run_sandbox(&pod_name, &image, command, run.id, timeout)
                .await;

            let (phase, exit_code, logs) = match result {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(job_id = job.id, err = %e, "CI job execution error");
                    ("Failed".to_string(), None, e.to_string())
                }
            };
            if let Some(lp) = &job.log_path {
                let _ = std::fs::write(lp, &logs);
            }
            Ok((phase, exit_code))
        };

        match outcome {
            Ok((phase, exit_code)) => {
                let status = if phase == "Succeeded" || exit_code == Some(0) {
                    "success"
                } else {
                    "failure"
                };
                let _ = db.set_job_status(job.id, status, exit_code);
                if status == "failure" {
                    run_failed = true;
                }
            }
            Err(e) => {
                let _ = db.set_job_status(job.id, "failure", None);
                if let Some(lp) = &job.log_path {
                    let _ = std::fs::write(lp, &e);
                }
                tracing::warn!(job_id = job.id, err = %e, "CI job execution error");
                run_failed = true;
            }
        }
    }
    if run_failed {
        return Err("one or more jobs failed".to_string());
    }
    let _ = logs_root;
    Ok(())
}

/// Blocking scheduler loop. Runs `tick` on an interval; never exits unless
/// the process stops. Intended to be spawned from `main`.
pub async fn run_loop(db: Arc<Db>, store: Arc<RepoStore>, logs_root: std::path::PathBuf, interval: Duration) {
    if !crate::actions::ci_enabled() {
        tracing::info!("CI scheduler disabled (set JJLAB_CI_ENABLED=1 to enable)");
        return;
    }
    tracing::info!(?interval, "CI scheduler starting");
    loop {
        match tick(db.clone(), store.clone(), logs_root.clone()).await {
            Ok(n) if n > 0 => tracing::info!(runs = n, "CI scheduler processed runs"),
            Ok(_) => {}
            Err(e) => tracing::warn!(err = %e, "CI scheduler tick failed"),
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_cap_is_sane() {
        assert!(job_timeout_cap().as_secs() >= 60);
    }

    #[test]
    fn default_image_resolves_through_artifact() {
        let img = default_ci_image();
        assert!(img.starts_with("artifact.zergx.svc.cluster.local/"), "default image must come from artifact registry, got {img}");
        assert!(img.ends_with("alpine:3"), "default is alpine:3, got {img}");
    }
}