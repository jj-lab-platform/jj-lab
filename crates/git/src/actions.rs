//! jjlab native CI (k8s-native, replaces the old in-process executor).
//!
//! A workflow is a `.jjlab-ci.yml` (or legacy `.github/workflows/*.yml`) file
//! at the repo root with k8s-native semantics — no `runs-on` VM labels, each
//! job declares its own container `image`, `resources` and `timeout`:
//!
//! ```yaml
//! name: CI
//! on: push
//! jobs:
//!   test:
//!     steps:
//!       - name: cargo test
//!         image: rust:1-bookworm
//!         run: cargo test
//!         timeout: 10m
//!         cpu: 1
//!         memory: 2Gi
//! ```
//!
//! Flow (no command ever runs in the jjlab process):
//!
//!   push / dispatch
//!     -> `on_push`/`dispatch` parse the workflow and insert runs/jobs with
//!        status `queued` (the run is *registered*, never executed here)
//!   scheduler (server main, env-gated)
//!     -> polls `pending_runs`, creates a Kubernetes sandbox Pod per job via
//!        `runtime::K8s::run_sandbox`, streams logs, writes status/exit_code.
//!
//! The scheduler is the ONLY thing that talks to Kubernetes, and it does so
//! out-of-band (never inside an HTTP request handler).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use jjlab_core::Db;

use crate::read;
use crate::repo::{RepoError, RepoStore};

/// Trigger set (subset): push + manual dispatch.
#[derive(Debug, Deserialize)]
pub struct WorkflowFile {
    pub name: Option<String>,
    #[serde(default)]
    pub on: serde_yaml::Value,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub jobs: serde_yaml::Mapping,
}

/// One CI job = one step (k8s-native: single container, one command).
#[derive(Debug, Deserialize)]
pub struct JobSpec {
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub steps: Vec<StepSpec>,
}

#[derive(Debug, Deserialize)]
pub struct StepSpec {
    #[serde(default)]
    pub name: Option<String>,

    /// Container image for this step.
    #[serde(default)]
    pub image: Option<String>,

    /// Shell command to run (single string; `sh -c` is the container's entrypoint).
    #[serde(default)]
    pub run: Option<String>,

    /// Image build spec — when set, jjlab invokes `buildctl` against the
    /// configured buildkitd to build (and optionally push) an image from the
    /// repository tree. Mutually exclusive with `run`.
    #[serde(default)]
    pub build: Option<BuildSpec>,

    /// Job hard timeout, e.g. `"10m"` / `"600s"`.
    #[serde(default)]
    pub timeout: Option<String>,

    /// CPU request/limit (e.g. `"1"`), optional.
    #[serde(default)]
    pub cpu: Option<String>,

    /// Memory request/limit (e.g. `"2Gi"`), optional.
    #[serde(default)]
    pub memory: Option<String>,
}

/// k8s-native image build step (buildkitd via `buildctl`).
#[derive(Debug, Deserialize, Serialize)]
pub struct BuildSpec {
    /// Containerfile path relative to the repo root (default `Dockerfile`).
    #[serde(default)]
    pub dockerfile: Option<String>,

    /// Build context path relative to the repo root (default `.`).
    #[serde(default)]
    pub context: Option<String>,

    /// Target stage (dockerfile `--target`). Optional.
    #[serde(default)]
    pub target: Option<String>,

    /// Image ref to export to, e.g. `artifact.example.com/team/app:v1`.
    #[serde(default)]
    pub image: Option<String>,

    /// Whether to push the built image to the registry (requires `image`).
    #[serde(default)]
    pub push: bool,

    /// Extra build args (`build-arg:foo=bar` style). Optional.
    #[serde(default)]
    pub build_args: Vec<String>,

    /// Disable buildkit layer cache.
    #[serde(default)]
    pub no_cache: bool,
}

/// Run an image build step via `buildctl`, exporting to `output_dir` (oci
/// layout) or pushing to a registry. Returns buildctl's stdout/stderr.
pub async fn run_build_step(
    build: &BuildSpec,
    context_dir: &Path,
    output_dir: Option<&Path>,
) -> Result<Vec<String>, RepoError> {
    let addr = std::env::var("JJLAB_BUILDKIT_ADDR")
        .unwrap_or_else(|_| "tcp://buildkitd.temp.svc.cluster.local:1234".to_string());

    let mut args: Vec<String> = vec![
        "--addr".into(),
        addr,
        "build".into(),
        "--frontend".into(),
        "dockerfile.v0".into(),
        "--progress".into(),
        "plain".into(),
    ];

    // Build context must be the repo snapshot directory.
    let context = build.context.as_deref().unwrap_or(".");
    args.push("--local".into());
    args.push(format!("context={}", context_dir.join(context).to_string_lossy()));
    // Dockerfile dir defaults to the repo root so `dockerfile:` may refer to it.
    let dockerfile_dir = build
        .dockerfile
        .as_deref()
        .and_then(|d| {
            let p = context_dir.join(d);
            p.parent().map(|p| p.to_path_buf())
        })
        .unwrap_or_else(|| context_dir.to_path_buf());
    args.push("--local".into());
    if build.dockerfile.is_some() {
        args.push(format!("dockerfile={}", dockerfile_dir.to_string_lossy()));
    } else {
        args.push(format!("dockerfile={}", context_dir.to_string_lossy()));
    }

    if let Some(target) = &build.target {
        args.push("--opt".into());
        args.push(format!("target={target}"));
    }
    for ba in &build.build_args {
        args.push("--opt".into());
        args.push(format!("build-arg:{ba}"));
    }
    if build.no_cache {
        args.push("--no-cache".into());
    }

    // Output: push to registry if `image`+`push`, else export oci layout to a
    // temp dir (or a provided dir).
    if let Some(img) = &build.image {
        let push = if build.push { ",push=true" } else { "" };
        args.push("--output".into());
        args.push(format!("type=image,name={img}{push}"));
    } else if let Some(out) = output_dir {
        args.push("--output".into());
        args.push(format!("type=oci,dest={}", out.to_string_lossy()));
    }

    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = crate::runtime::run_cli("buildctl", &arg_refs, Duration::from_secs(3600)).await?;
    let mut logs = Vec::new();
    if !out.stdout.is_empty() {
        logs.push(out.stdout);
    }
    if !out.stderr.is_empty() {
        logs.push(out.stderr);
    }
    if out.code != 0 {
        return Err(RepoError::Other(format!(
            "buildctl exited with status {}",
            out.code
        )));
    }
    Ok(logs)
}

/// Default sandbox image when a job omits `image`. Resolved through the
/// in-cluster artifact registry (never public docker.io — the sandbox pod
/// cannot reach the internet), overridable via `JJLAB_CI_IMAGE`.
pub fn default_ci_image() -> String {
    std::env::var("JJLAB_CI_IMAGE")
        .unwrap_or_else(|_| "artifact.zergx.svc.cluster.local/library/alpine:3".to_string())
}

/// Whether CI scheduling is enabled for this process (read from env, default
/// off — a single `JJLAB_CI_ENABLED=1` in the deployment turns it on).
pub fn ci_enabled() -> bool {
    std::env::var("JJLAB_CI_ENABLED")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Which triggers the workflow declares (subset: push, workflow_dispatch).
pub fn triggers_of(wf: &WorkflowFile) -> Vec<String> {
    match &wf.on {
        serde_yaml::Value::String(s) => vec![s.clone()],
        serde_yaml::Value::Sequence(seq) => seq
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        serde_yaml::Value::Mapping(m) => m
            .keys()
            .filter_map(|k| k.as_str().map(str::to_string))
            .collect(),
        _ => vec!["push".to_string()],
    }
}

/// Parse a timeout string like "10m"/"300" (seconds) into seconds.
pub fn parse_timeout(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(num) = s.strip_suffix('m') {
        return num.trim().parse::<u64>().ok().map(|n| n * 60);
    }
    if let Some(num) = s.strip_suffix('s') {
        return num.trim().parse::<u64>().ok();
    }
    s.parse::<u64>().ok()
}

/// Scan `.jjlab-ci.yml` (plus legacy `.github/workflows/*.yml`) at `head_sha`
/// and register workflows. If a workflow declares a `push` trigger, enqueue a
/// run (execution is deferred to the CI scheduler).
pub async fn on_push(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    head_sha: &str,
    logs_root: &Path,
) -> Result<Vec<i64>, RepoError> {
    let repo_id = format!("{org}/{repo}");
    let mut enqueued = Vec::new();
    // Legacy path: `.github/workflows/*.yml`.
    let entries = read::contents_dir_at(store, org, repo, head_sha, ".github/workflows")
        .await
        .unwrap_or_default();
    for entry in entries {
        if entry["type"] != "file" {
            continue;
        }
        let name = entry["path"].as_str().unwrap_or_default().to_string();
        if !(name.ends_with(".yml") || name.ends_with(".yaml")) {
            continue;
        }
        let full = format!(".github/workflows/{name}");
        let raw = read::read_file_at(store, org, repo, head_sha, &full).await?;
        let Ok(wf) = serde_yaml::from_str::<WorkflowFile>(&String::from_utf8_lossy(&raw)) else {
            continue;
        };
        let wf_name = wf.name.clone().unwrap_or_else(|| name.clone());
        let triggers = triggers_of(&wf);
        let wf_id = db.upsert_workflow(&repo_id, &wf_name, &full, "push", true)
            .map_err(|e| RepoError::Other(e.to_string()))?;
        if triggers.iter().any(|t| t == "push") {
            enqueued.push(enqueue_run(db, org, repo, wf_id, head_sha, &wf, logs_root)?);
        }
    }

    // Native path: `.jjlab-ci.yml` at the root.
    if let Ok(raw) = read::read_file_at(store, org, repo, head_sha, ".jjlab-ci.yml").await {
        if let Ok(wf) = serde_yaml::from_str::<WorkflowFile>(&String::from_utf8_lossy(&raw)) {
            let wf_name = wf.name.clone().unwrap_or_else(|| ".jjlab-ci.yml".to_string());
            let triggers = triggers_of(&wf);
            let wf_id = db
                .upsert_workflow_cron(
                    &repo_id,
                    &wf_name,
                    ".jjlab-ci.yml",
                    "push",
                    true,
                    schedule_cron(&wf).as_deref(),
                )
                .map_err(|e| RepoError::Other(e.to_string()))?;
            if triggers.iter().any(|t| t == "push") {
                enqueued.push(enqueue_run(db, org, repo, wf_id, head_sha, &wf, logs_root)?);
            }
        }
    }

    Ok(enqueued)
}

/// Enqueue runs for a workflow by id. Creates one run + one queued job per
/// step that has a `run` command. Execution is deferred to the scheduler.
pub fn enqueue_run(
    db: &Db,
    org: &str,
    repo: &str,
    workflow_id: i64,
    trigger_ref: &str,
    wf: &WorkflowFile,
    logs_root: &Path,
) -> Result<i64, RepoError> {
    let repo_id = format!("{org}/{repo}");
    let run_id = db
        .create_run(&repo_id, workflow_id, trigger_ref)
        .map_err(|e| RepoError::Other(e.to_string()))?;

    let mut any_job = false;
    for (key, job_value) in &wf.jobs {
        let Some(job_name) = key.as_str() else { continue };
        let Ok(spec) = serde_yaml::from_value::<JobSpec>(job_value.clone()) else {
            continue;
        };
        // One job row per step that has a `run` command or a `build` spec.
        let mut step_index = 0usize;
        for step in &spec.steps {
            if step.run.is_none() && step.build.is_none() {
                step_index += 1;
                continue;
            }
            let label = step
                .name
                .clone()
                .unwrap_or_else(|| format!("{job_name}:{}", step_index + 1));
            let log_path = logs_root
                .join(format!("run{run_id}"))
                .join(format!("{}.log", sanitize(&label)));
            if let Some(dir) = log_path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let timeout_seconds = step.timeout.as_deref().and_then(parse_timeout).map(|s| s as i64);
            // Serialize the build spec into a job column so the scheduler can
            // rehydrate it and run `buildctl` (execution is deferred to pods/
            // buildkitd, never run here).
            let build_spec = step
                .build
                .as_ref()
                .map(|b| serde_json::to_string(b).unwrap_or_default());
            // B6: merge workflow-level env + job-level env + repo secrets
            // into the step's shell environment (exports prefix the run).
            let mut merged_env = wf.env.clone();
            for (k, v) in &spec.env {
                merged_env.insert(k.clone(), v.clone());
            }
            for (k, v) in repo_secrets() {
                merged_env.insert(k, v);
            }
            let run_cmd: Option<String> = step.run.as_ref().map(|cmd| {
                if merged_env.is_empty() {
                    cmd.clone()
                } else {
                    let exports: Vec<String> = merged_env
                        .iter()
                        .map(|(k, v)| format!("export {}={};", shell_safe(k), shell_safe(v)))
                        .collect();
                    format!("{} {}", exports.join(" "), cmd)
                }
            });
            let _ = db
                .create_job_detail(
                    run_id,
                    &label,
                    &log_path.to_string_lossy(),
                    step.image.as_deref(),
                    run_cmd.as_deref(),
                    timeout_seconds,
                    build_spec.as_deref(),
                )
                .map_err(|e| RepoError::Other(e.to_string()))?;
            any_job = true;
            step_index += 1;
        }
    }
    if !any_job {
        // A workflow with no runnable steps is skipped: mark the run as
        // skipped so it does not linger in the queue.
        let _ = db.set_run_status(run_id, "skipped");
    }
    Ok(run_id)
}

/// Manual dispatch: verify ownership, load the workflow from a branch tip,
/// then enqueue a run (execution deferred to the scheduler).
pub async fn dispatch(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    workflow_id: i64,
    logs_root: &Path,
) -> Result<Vec<i64>, RepoError> {
    let Some(wf_row) = db
        .get_workflow(workflow_id)
        .map_err(|e| RepoError::Other(e.to_string()))?
    else {
        return Err(RepoError::Other(format!("workflow {workflow_id} not found")));
    };
    let repo_id = format!("{org}/{repo}");
    if wf_row.repo_id != repo_id {
        return Err(RepoError::Other(format!("workflow {workflow_id} not found")));
    }

    // Find the workflow file on some branch tip.
    let mut found = None;
    for (_name, sha) in read::branch_tips(store, org, repo).await? {
        if let Ok(raw) = read::read_file_at(store, org, repo, &sha, &wf_row.path).await {
            found = Some((sha, raw));
            break;
        }
    }
    let Some((head, raw)) = found else {
        return Err(RepoError::Other(format!(
            "workflow file {} not found on any branch",
            wf_row.path
        )));
    };
    let wf = serde_yaml::from_str::<WorkflowFile>(&String::from_utf8_lossy(&raw))
        .map_err(|e| RepoError::Other(format!("parse workflow: {e}")))?;
    let run_id = enqueue_run(db, org, repo, workflow_id, &head, &wf, logs_root)?;
    Ok(vec![run_id])
}

/// Read a job's log file from disk.
pub fn job_log(log_path: &str) -> Vec<u8> {
    std::fs::read(log_path).unwrap_or_default()
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wf(yaml: &str) -> WorkflowFile {
        serde_yaml::from_str(yaml).expect("parse workflow")
    }

    #[test]
    fn parses_k8s_native_job_shape() {
        let w = wf(
            "name: CI\non: push\njobs:\n  test:\n    steps:\n      - name: build\n        image: rust:1\n        run: cargo build\n        timeout: 10m\n        cpu: '1'\n        memory: 2Gi\n",
        );
        assert_eq!(triggers_of(&w), vec!["push".to_string()]);
        let job = w.jobs.get(serde_yaml::Value::String("test".into())).unwrap();
        let spec: JobSpec = serde_yaml::from_value(job.clone()).unwrap();
        assert_eq!(spec.steps.len(), 1);
        let s = &spec.steps[0];
        assert_eq!(s.image.as_deref(), Some("rust:1"));
        assert_eq!(s.run.as_deref(), Some("cargo build"));
        assert_eq!(parse_timeout("10m"), Some(600));
        assert_eq!(parse_timeout("300s"), Some(300));
        assert_eq!(parse_timeout("90"), Some(90));
        assert_eq!(parse_timeout("nonsense"), None);
    }

    #[test]
    fn parses_on_sequence_and_mapping_forms() {
        let a = wf("on: [push, workflow_dispatch]\njobs: {}\n");
        assert!(triggers_of(&a).contains(&"workflow_dispatch".to_string()));
        let b = wf("on:\n  push:\n    branches: [main]\njobs: {}\n");
        assert!(triggers_of(&b).contains(&"push".to_string()));
    }

    #[test]
    fn parses_build_spec() {
        let w = wf(
            "on: push\njobs:\n  img:\n    steps:\n      - name: build image\n        build:\n          dockerfile: Dockerfile\n          context: .\n          image: artifact.example.com/team/app:v1\n          push: true\n          build_args:\n            - FOO=bar\n",
        );
        let job = w.jobs.get(serde_yaml::Value::String("img".into())).unwrap();
        let spec: JobSpec = serde_yaml::from_value(job.clone()).unwrap();
        let b = spec.steps[0].build.as_ref().expect("build spec parsed");
        assert_eq!(b.dockerfile.as_deref(), Some("Dockerfile"));
        assert_eq!(b.image.as_deref(), Some("artifact.example.com/team/app:v1"));
        assert!(b.push);
        assert_eq!(b.build_args, vec!["FOO=bar".to_string()]);
        assert!(spec.steps[0].run.is_none());
    }

    #[test]
    fn sanitize_replaces_slashes() {
        assert_eq!(sanitize("build/test 1"), "build_test_1");
    }
}

/// `on: pull_request` — scan workflows at `head_sha` and enqueue runs for
/// those declaring a `pull_request` trigger (best-effort per file).
pub async fn on_pull_request(
    store: &Arc<RepoStore>,
    db: &Db,
    org: &str,
    repo: &str,
    head_sha: &str,
    logs_root: &Path,
) -> Result<Vec<i64>, RepoError> {
    let repo_id = format!("{org}/{repo}");
    let mut enqueued = Vec::new();
    if let Ok(raw) = read::read_file_at(store, org, repo, head_sha, ".jjlab-ci.yml").await {
        if let Ok(wf) = serde_yaml::from_str::<WorkflowFile>(&String::from_utf8_lossy(&raw)) {
            let wf_name = wf.name.clone().unwrap_or_else(|| ".jjlab-ci.yml".to_string());
            let triggers = triggers_of(&wf);
            let wf_id = db
                .upsert_workflow(&repo_id, &wf_name, ".jjlab-ci.yml", "pull_request", true)
                .map_err(|e| RepoError::Other(e.to_string()))?;
            if triggers.iter().any(|t| t == "pull_request") {
                enqueued.push(enqueue_run(db, org, repo, wf_id, head_sha, &wf, logs_root)?);
            }
        }
    }
    Ok(enqueued)
}

/// Dispatch a scheduled workflow run at `head` (dedup: one run per workflow
/// per minute — enforced by the caller's minute key in the run's trigger_ref
/// annotation; here we simply enqueue).
pub async fn dispatch_scheduled(
    store: Arc<RepoStore>,
    db: Arc<jjlab_core::Db>,
    org: String,
    repo: String,
    workflow_id: i64,
    head: String,
    logs_root: std::path::PathBuf,
    _minute_key: &str,
) -> Result<bool, RepoError> {
    // jj types are !Send; run the whole read on the blocking pool (the
    // pollster pattern used by the HTTP handlers) so this future stays Send.
    tokio::task::spawn_blocking(move || {
        pollster::block_on(async {
            let Some(wf_row) = db
                .get_workflow(workflow_id)
                .map_err(|e| RepoError::Other(e.to_string()))?
            else {
                return Err(RepoError::Other(format!("workflow {workflow_id} not found")));
            };
            let raw = read::read_file_at(&store, &org, &repo, &head, &wf_row.path)
                .await
                .map_err(|e| {
                    RepoError::Other(format!("workflow file {}/{} unread: {}", org, repo, e))
                })?;
            let wf = serde_yaml::from_str::<WorkflowFile>(&String::from_utf8_lossy(&raw))
                .map_err(|e| RepoError::Other(format!("parse workflow: {e}")))?;
            enqueue_run(&db, &org, &repo, workflow_id, &head, &wf, &logs_root)?;
            Ok(true)
        })
    })
    .await
    .map_err(|e| RepoError::Other(format!("schedule task join: {e}")))?
}

/// Extract the first `on: {schedule: [{cron: "…"}]}` expression, if any.
fn schedule_cron(wf: &WorkflowFile) -> Option<String> {
    let serde_yaml::Value::Mapping(m) = &wf.on else {
        return None;
    };
    let sched = m.get(serde_yaml::Value::String("schedule".into()))?;
    let seq = sched.as_sequence()?;
    for item in seq {
        if let Some(cron) = item.get(serde_yaml::Value::String("cron".into())) {
            if let Some(s) = cron.as_str() {
                return Some(s.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod schedule_tests {
    use super::*;
    use crate::scheduler::cron_matches;

    fn wf(yaml: &str) -> WorkflowFile {
        serde_yaml::from_str(yaml).expect("parse workflow")
    }

    #[test]
    fn schedule_cron_extracted_from_mapping_trigger() {
        let w = wf(
            "name: Nightly\non:\n  schedule:\n    - cron: \"17 3 * * *\"\njobs:\n  a:\n    steps:\n      - run: echo hi\n",
        );
        assert_eq!(schedule_cron(&w).as_deref(), Some("17 3 * * *"));
    }

    #[test]
    fn schedule_cron_none_for_push() {
        let w = wf("name: CI\non: push\njobs:\n  a:\n    steps:\n      - run: echo hi\n");
        assert_eq!(schedule_cron(&w), None);
    }

    #[test]
    fn cron_matcher_handles_star_and_steps() {
        let t = jiff::Zoned::now()
            .with()
            .year(2026)
            .month(9)
            .day(1)
            .hour(3)
            .minute(17)
            .second(0)
            .build()
            .unwrap();
        assert!(cron_matches("* * * * *", t.clone()));
        assert!(cron_matches("17 3 * * *", t.clone()));
        assert!(!cron_matches("18 3 * * *", t.clone()));
        assert!(cron_matches("*/15 * * * *", t.with().minute(30).build().unwrap()));
        assert!(!cron_matches("*/15 * * * *", t.with().minute(17).build().unwrap()));
    }
}

/// Repo-level secrets injected into every CI job (B6): parsed from the
/// JJLAB_CI_SECRETS env var as `KEY=value` comma entries, or a JSON object
/// file path in JJLAB_CI_SECRETS_FILE. Missing config = no secrets.
fn repo_secrets() -> Vec<(String, String)> {
    if let Ok(file) = std::env::var("JJLAB_CI_SECRETS_FILE") {
        if let Ok(raw) = std::fs::read_to_string(&file) {
            if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(&raw) {
                return map
                    .into_iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                    .collect();
            }
        }
    }
    if let Ok(raw) = std::env::var("JJLAB_CI_SECRETS") {
        return raw
            .split(',')
            .filter_map(|pair| pair.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
    }
    Vec::new()
}

/// Quote a value so it survives a double-quoted shell embedding safely.
fn shell_safe(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.,/:=".contains(c)) && !s.is_empty() {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod env_tests {
    use super::*;

    fn wf(yaml: &str) -> WorkflowFile {
        serde_yaml::from_str(yaml).expect("parse workflow")
    }

    #[test]
    fn shell_safe_quotes_spaces_and_quotes() {
        assert_eq!(shell_safe("plain-value"), "plain-value");
        assert_eq!(shell_safe("has space"), "'has space'");
        let quoted = shell_safe("o\u{27}brien");
        // Must round-trip through a real shell evaluation.
        let script = format!("V={}; printf %s \"$V\"", quoted);
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .output()
            .expect("spawn sh");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "o\u{27}brien");
    }

    #[test]
    fn workflow_and_job_env_parsed() {
        let w = wf(
            "name: E\non: push\nenv:\n  GLOBAL: g\njobs:\n  a:\n    env:\n      LOCAL: l\n    steps:\n      - run: echo hi\n",
        );
        assert_eq!(w.env.get("GLOBAL").map(String::as_str), Some("g"));
        let job = w.jobs.get(serde_yaml::Value::String("a".into())).unwrap();
        let spec: JobSpec = serde_yaml::from_value(job.clone()).unwrap();
        assert_eq!(spec.env.get("LOCAL").map(String::as_str), Some("l"));
    }
}
