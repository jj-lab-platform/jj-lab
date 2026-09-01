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

/// Max runs processed concurrently (default 2). A cap of 0 disables the cap.
pub fn max_concurrency() -> usize {
    std::env::var("JJLAB_CI_MAX_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(2)
}

/// The archive URL a CI pod fetches its repo snapshot from (in-cluster self
/// address), or None when the scheduler must not inject a snapshot.
pub fn snapshot_base() -> Option<String> {
    std::env::var("JJLAB_SELF_URL").ok().filter(|s| !s.is_empty())
}

/// Run one scheduling loop: claim queued runs, execute their jobs, mark
/// status. Returns the number of runs processed.
pub async fn tick(
    db: Arc<Db>,
    store: Arc<RepoStore>,
    logs_root: std::path::PathBuf,
) -> Result<usize, String> {
    tick_schedules(&db, &store, &logs_root).await;

    let k8s = runtime::connect().await.map_err(|e| e.to_string())?;
    let mut pending = db
        .pending_runs()
        .map_err(|e| e.to_string())?;
    if pending.is_empty() {
        return Ok(0);
    }
    // Global concurrency cap (B3): process at most `max_concurrency` runs per
    // tick; the rest stay queued for the next tick.
    let cap = max_concurrency();
    pending.truncate(cap);
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
    db: &Arc<Db>,
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

    // Run the run's jobs in parallel (B3): each job is an independent
    // single-step unit; a run fails when any of its jobs fails.
    let mut set: tokio::task::JoinSet<(i64, String, bool)> = tokio::task::JoinSet::new();
    for job in jobs {
        // Ensure the job's log path has a parent dir.
        if let Some(lp) = &job.log_path {
            if let Some(dir) = std::path::Path::new(lp).parent() {
                let _ = std::fs::create_dir_all(dir);
            }
        }
        let _ = db.set_job_status(job.id, "running", None);

        let job_id = job.id;
        let job2 = job.clone();
        let run_id = run.id;
        let k8s2 = k8s.clone_inner();
        let ctx_dir2 = ctx_dir.clone();
        let cap = job_timeout_cap();
        let snapshot = snapshot_base().map(|base| {
            format!(
                "{base}/api/v1/repos/{org}/{repo}/archive/tarball/{}",
                urlencoding_lite(&run.trigger_ref)
            )
        });
        set.spawn(async move {
            // Image build step: buildctl (host-side, no pod).
            if let Some(spec_json) = &job2.build_spec {
                let out = match serde_json::from_str::<BuildSpec>(spec_json) {
                    Ok(spec) => {
                        let out_layout = std::env::temp_dir()
                            .join(format!("jjlab-ci-oci-{}-{}", run_id, job_id));
                        let _ = std::fs::remove_dir_all(&out_layout);
                        if let Err(e) = std::fs::create_dir_all(&out_layout) {
                            Err(format!("mkdir oci layout: {e}"))
                        } else {
                            match crate::actions::run_build_step(&spec, &ctx_dir2, Some(&out_layout)).await {
                                Ok(logs) => {
                                    if let Some(lp) = &job2.log_path {
                                        let _ = std::fs::write(lp, logs.join("\n"));
                                    }
                                    Ok(("Succeeded".to_string(), Some(0)))
                                }
                                Err(e) => {
                                    if let Some(lp) = &job2.log_path {
                                        let _ = std::fs::write(lp, e.to_string());
                                    }
                                    Ok(("Failed".to_string(), Some(1)))
                                }
                            }
                        }
                    }
                    Err(e) => Err(format!("invalid build spec for job {job_id}: {e}")),
                };
                let (phase, code) = out.unwrap_or_else(|_| ("Failed".to_string(), None));
                let ok = phase == "Succeeded" || code == Some(0);
                return (job_id, phase, ok);
            }
            // Shell step: throwaway sandbox pod (optionally with a snapshot).
            let command = job2.command().to_string();
            let image = job2.image.clone().unwrap_or_else(default_ci_image);
            let pod_name = format!("jjlab-ci-{}-{}", run_id, job_id);
            let timeout = job2
                .timeout_seconds
                .and_then(|s| u64::try_from(s).ok())
                .map(Duration::from_secs)
                .unwrap_or(cap);
            let timeout = timeout.min(cap);

            let sink: String = job2.log_path.clone().unwrap_or_else(|| {
                std::env::temp_dir()
                    .join(format!("jjlab-ci-{}.log", job_id))
                    .to_string_lossy()
                    .to_string()
            });
            // Streaming run (B4): logs append to the sink while the pod runs;
            // the job-logs endpoint tails this file live.
            let pod = runtime::sandbox_pod_full(
                &pod_name,
                &image,
                &command,
                run_id,
                &[],
                snapshot.as_deref(),
            );
            match k8s2.run_pod_streaming(pod, sink.into(), timeout).await {
                Ok((phase, exit_code)) => {
                    let ok = phase == "Succeeded" || exit_code == Some(0);
                    (job_id, phase, ok)
                }
                Err(e) => {
                    tracing::warn!(job_id, err = %e, "CI job execution error");
                    if let Some(lp) = &job2.log_path {
                        let _ = std::fs::write(lp, &e.to_string());
                    }
                    (job_id, "Failed".to_string(), false)
                }
            }
        });
    }
    while let Some(res) = set.join_next().await {
        let (job_id, _phase, ok) = match res {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(err = %e, "CI job task panicked");
                continue;
            }
        };
        let status = if ok { "success" } else { "failure" };
        let _ = db.set_job_status(job_id, &status, None);
        if !ok {
            run_failed = true;
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

/// Minimal percent-encoding for a rev embedded in an archive URL path.
fn urlencoding_lite(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Minimal 5-field cron matcher (minute hour dom month dow; `*` and `*/n`
/// only — sufficient for CI schedules). Local time is not considered; the
/// host runs UTC.
pub fn cron_matches(expr: &str, t: jiff::Zoned) -> bool {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    let dom_any = fields[2] == "*";
    let dow_any = fields[4] == "*";
    let checks = [
        field_matches(fields[0], t.minute() as i32),
        field_matches(fields[1], t.hour() as i32),
        dom_any || field_matches(fields[2], t.day() as i32),
        field_matches(fields[3], t.month() as i32),
        dow_any || {
            let zero = t.date().weekday().to_monday_zero_offset() as i32;
            field_matches(fields[4], (zero + 1) % 7)
        },
    ];
    // cron ORs dom/dow when both are restricted; here one is always `*`.
    checks.into_iter().all(|ok| ok)
}

/// `*`, `*/n`, `a-b`, or a comma list of plain numbers.
fn field_matches(field: &str, value: i32) -> bool {
    field.split(',').any(|part| {
        let part = part.trim();
        if part == "*" {
            return true;
        }
        if let Some(step) = part.strip_prefix("*/") {
            if let Ok(n) = step.parse::<i32>() {
                return n > 0 && value % n == 0;
            }
            return false;
        }
        if let Some((a, b)) = part.split_once('-') {
            return a.trim().parse::<i32>().map(|a| b.trim().parse::<i32>().map(|b| value >= a && value <= b).unwrap_or(false)).unwrap_or(false);
        }
        part.parse::<i32>().map(|n| n == value).unwrap_or(false)
    })
}

/// Enqueue runs for schedule-triggered workflows whose cron is due this
/// minute. Best-effort: per-workflow failures are logged, never fatal.
async fn tick_schedules(db: &Arc<Db>, store: &Arc<RepoStore>, logs_root: &std::path::Path) {
    let due = match db.scheduled_workflows() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(err = %e, "schedule scan failed");
            return;
        }
    };
    if due.is_empty() {
        return;
    }
    let now = jiff::Zoned::now();
    // Deduplicate: one run per workflow per minute.
    let minute_key = now.strftime("%Y-%m-%dT%H:%M").to_string();
    for (wf_id, repo_id, _path, expr) in due {
        if !cron_matches(&expr, now.clone()) {
            continue;
        }
        let Some((org, repo)) = repo_id.split_once('/') else { continue };
        let (org, repo) = (org.to_string(), repo.to_string());
        // Head of the repo default branch drives a scheduled run.
        let bm = match db.get_repo(&repo_id) {
            Ok(Some(r)) => r.default_bookmark,
            _ => "main".to_string(),
        };
        let head = match default_head(store.clone(), org.to_string(), repo.to_string(), bm).await {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(repo_id, err = %e, "schedule head resolve failed");
                continue;
            }
        };
        match crate::actions::dispatch_scheduled(
            store.clone(),
            db.clone(),
            org,
            repo,
            wf_id,
            head.clone(),
            logs_root.to_path_buf(),
            &minute_key,
        )
        .await
        {
            Ok(true) => tracing::info!(repo_id, workflow = wf_id, "scheduled run enqueued"),
            Ok(false) => {} // already ran this minute
            Err(e) => tracing::warn!(repo_id, err = %e, "scheduled dispatch failed"),
        }
    }
}

/// The head commit of a repo's default bookmark (schedule runs pin there).
/// jj types are !Send, so the resolution runs on the blocking pool (same
/// pattern as the HTTP handlers) and only the sha crosses back.
async fn default_head(
    store: Arc<RepoStore>,
    org: String,
    repo: String,
    bookmark: String,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        pollster::block_on(async {
            let branches = crate::read::branches(&store, &org, &repo)
                .await
                .map_err(|e| e.to_string())?;
            branches
                .into_iter()
                .find(|b| b.name == bookmark)
                .map(|b| b.sha)
                .ok_or_else(|| format!("default bookmark {bookmark} not found"))
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
pub mod scheduler_tests_cron { 
    pub use super::cron_matches;
}
