//! Helm lifecycle primitive for the `/ops` surface (subprocess `helm`).
//!
//! jjlab owns the generic "install/upgrade/list/status/values/history/
//! rollback/uninstall" operations against the cluster; the ops-extension owns
//! chart selection, value rendering, and what a release *means*.

use std::time::Duration;

use serde::Deserialize;

use crate::repo::{RepoError, RepoResult};

pub const HELM_TIMEOUT: Duration = Duration::from_secs(600);

/// One helm release summary (from `helm list -o json`).
#[derive(Debug, Clone, serde::Deserialize)]
struct HelmListEntry {
    name: String,
    namespace: String,
    revision: Option<String>,
    status: String,
    chart: Option<String>,
}

/// Arguments to install/upgrade a chart.
#[derive(Debug, Clone, Deserialize)]
pub struct HelmInstallRequest {
    pub release_name: String,
    /// Chart reference (path/URL `helm` can locate directly).
    #[serde(default)]
    pub chart: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub values: serde_json::Value,
    #[serde(default)]
    pub namespace: Option<String>,
}

/// Run `helm <args...>` and return stdout on success.
pub async fn run(args: &[String], timeout: Duration) -> RepoResult<String> {
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = crate::runtime::run_cli("helm", &refs, timeout).await?;
    if out.code != 0 {
        return Err(RepoError::Other(format!("helm {refs:?} exited {}: {}", out.code, out.stderr)));
    }
    Ok(out.stdout)
}

/// Materialize a JSON `values` object into a temp file and return its path;
/// `None` when values is null. The file is leaked into the system temp dir
/// (small; a restart sweeps it) so the async task outlives this frame.
fn values_file(values: &serde_json::Value) -> RepoResult<Option<String>> {
    if values.is_null() {
        return Ok(None);
    }
    let path = std::env::temp_dir().join(format!(
        "jjlab-helm-values-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, values.to_string())
        .map_err(|e| RepoError::Other(format!("write helm values: {e}")))?;
    Ok(Some(path.to_string_lossy().to_string()))
}

pub async fn install_or_upgrade(req: &HelmInstallRequest, chart_path: &str) -> RepoResult<String> {
    let mut args: Vec<String> = vec!["upgrade".into(), "--install".into(), req.release_name.clone(), chart_path.into()];
    if !req.version.is_empty() {
        args.push(format!("--version={}", req.version));
    }
    if let Some(ns) = &req.namespace {
        args.push(format!("--namespace={ns}"));
    }
    if let Some(path) = values_file(&req.values)? {
        args.push(format!("--values={path}"));
    }
    args.push("--wait".into());
    run(&args, HELM_TIMEOUT).await
}

pub async fn list(namespace: &str) -> RepoResult<Vec<serde_json::Value>> {
    let out = run(
        &["list".into(), "--all".into(), "--output".into(), "json".into(), format!("--namespace={namespace}")],
        Duration::from_secs(120),
    )
    .await?;
    let parsed: Vec<HelmListEntry> =
        serde_json::from_str(&out).map_err(|e| RepoError::Other(format!("helm list parse: {e}")))?;
    Ok(parsed
        .into_iter()
        .map(|e| serde_json::json!({
            "name": e.name, "namespace": e.namespace, "revision": e.revision,
            "status": e.status, "chart": e.chart,
        }))
        .collect())
}

pub async fn status(release_name: &str, namespace: Option<&str>) -> RepoResult<String> {
    let mut args: Vec<String> = vec!["status".into(), release_name.into(), "--output".into(), "json".into()];
    if let Some(ns) = namespace {
        args.push(format!("--namespace={ns}"));
    }
    run(&args, Duration::from_secs(120)).await
}

pub async fn uninstall(release_name: &str, namespace: Option<&str>) -> RepoResult<String> {
    let mut args: Vec<String> = vec!["uninstall".into(), release_name.into()];
    if let Some(ns) = namespace {
        args.push(format!("--namespace={ns}"));
    }
    run(&args, Duration::from_secs(300)).await
}

pub async fn rollback(release_name: &str, revision: Option<u32>, namespace: Option<&str>) -> RepoResult<String> {
    let rev = revision.map(|r| r.to_string()).unwrap_or_else(|| "0".to_string());
    let mut args: Vec<String> = vec!["rollback".into(), release_name.into(), rev];
    if let Some(ns) = namespace {
        args.push(format!("--namespace={ns}"));
    }
    run(&args, Duration::from_secs(300)).await
}