//! Kubernetes + CLI execution runtime for jjlab's native CI (single-image,
//! no external Go services).
//!
//! Split on purpose:
//!   - Kubernetes object lifecycle (create/wait/log/delete sandbox pods) is
//!     done with the mature [`kube`] crate (strongly-typed API, no kubectl).
//!   - Tool CLIs that have no first-class Rust library (helm, buildctl) are
//!     spawned as subprocesses, exactly like the rest of jjlab already spawns
//!     `git` for the smart-HTTP transport.
//!
//! Execution NEVER happens in the jjlab process: a run becomes a Kubernetes
//! Pod whose container runs the step's `run` command.

use std::process::Stdio;
use std::time::Duration;

use k8s_openapi::api::core::v1::Pod;
use kube::api::{DeleteParams, ListParams, LogParams, PostParams};
use kube::client::Client;
use kube::config::Config;
use kube::Api;

use crate::repo::{RepoError, RepoResult};

/// The runtime namespace the sandbox pods are created in (write RBAC must be
/// granted here). Defaults to the in-cluster / kubeconfig current namespace.
pub fn runtime_namespace() -> String {
    std::env::var("JJLAB_CI_NAMESPACE").unwrap_or_default()
}

/// Connect to the cluster: in-cluster service account first, kubeconfig
/// fallback (mirrors `kubectl` resolution). `JJLAB_CI_NAMESPACE` selects the
/// target namespace when overridden from the cluster default.
pub async fn connect() -> Result<K8s, RepoError> {
    let (client, target_ns) = connect_client().await?;
    Ok(K8s { client, default_ns: target_ns })
}

/// Build just the kube `Client` (no default-namespace logic) — used by the
/// `/ops` handlers which resolve namespaces per-request via the registry.
pub async fn connect_client() -> Result<(Client, String), RepoError> {
    let mut config = Config::infer()
        .await
        .map_err(|e| RepoError::Other(format!("kube config: {e}")))?;
    // The in-cluster k3s API is on the pod network and must NOT be routed
    // through the ambient HTTP(S)_PROXY (mihomo). `kube` picks up that proxy
    // verbatim and would otherwise require the http-proxy feature + a proxy
    // hop that can't reach the cluster. Force a direct connection.
    config.proxy_url = None;
    let mut target_ns = config.default_namespace.clone();
    let override_ns = runtime_namespace();
    if !override_ns.is_empty() {
        target_ns = override_ns;
    }
    let client = Client::try_from(config)
        .map_err(|e| RepoError::Other(format!("kube client: {e}")))?;
    Ok((client, target_ns))
}

/// Thin wrapper over a configured kube client + target namespace.
pub struct K8s {
    client: Client,
    default_ns: String,
}

impl K8s {
    /// Wrap an already-connected kube `Client` (per-request namespace use).
    pub fn from_client(client: Client) -> Self {
        K8s { client, default_ns: "default".to_string() }
    }

    pub fn namespace(&self) -> &str {
        if self.default_ns.is_empty() {
            "default"
        } else {
            &self.default_ns
        }
    }

    /// The raw kube client (cloned) — the `/ops` service layer builds its own
    /// per-request namespaced `Api`s for other resource kinds (Deployment,
    /// Service) from this.
    pub fn client(&self) -> Client {
        self.client.clone()
    }

    fn pods(&self) -> Api<Pod> {
        Api::namespaced(self.client.clone(), self.namespace())
    }

    /// A `Pod` api scoped to an explicit namespace (per-request override).
    pub fn pods_in(&self, ns: &str) -> Api<Pod> {
        Api::namespaced(self.client.clone(), ns)
    }

    /// Create a one-shot sandbox pod running `sh -c <run>` in `image`, and
    /// block until it reaches a terminal phase or `timeout` elapses.
    ///
    /// Returns the final `(phase, exit_code)` — `exit_code` is `None` on
    /// timeout or when the pod has no container status.
    pub async fn run_sandbox(
        &self,
        name: &str,
        image: &str,
        run: &str,
        run_id: i64,
        timeout: Duration,
    ) -> Result<(String, Option<i32>, String), RepoError> {
        self.run_sandbox_with(name, image, run, run_id, timeout, &[]).await
    }

    /// `run_sandbox` with extra pod env vars (workflow `env`/secrets).
    pub async fn run_sandbox_with(
        &self,
        name: &str,
        image: &str,
        run: &str,
        run_id: i64,
        timeout: Duration,
        env: &[(String, String)],
    ) -> Result<(String, Option<i32>, String), RepoError> {
        let pod = sandbox_pod_full(name, image, run, run_id, env, None);
        self.pods()
            .create(&PostParams::default(), &pod)
            .await
            .map_err(|e| RepoError::Other(format!("create sandbox pod: {e}")))?;

        let _ = self.wait_terminal(name, timeout).await;
        let phase = self.pod_phase(name).await?;
        let exit_code = self.pod_exit_code(name).await?;
        // Read logs BEFORE deleting the pod (kube drops them with the pod).
        let logs = self.pod_logs(name).await.unwrap_or_default();
        let _ = self.delete_pod(name).await;
        Ok((phase, exit_code, logs))
    }

    /// Cheap handle clone for spawning per-job tasks.
    pub fn clone_inner(&self) -> K8s {
        K8s { client: self.client.clone(), default_ns: self.default_ns.clone() }
    }

    /// Create a fully-formed pod, wait for terminal phase, collect logs, delete.
    pub async fn run_pod(
        &self,
        pod: Pod,
        name: &str,
        timeout: Duration,
    ) -> Result<(String, Option<i32>, String), RepoError> {
        self.pods()
            .create(&PostParams::default(), &pod)
            .await
            .map_err(|e| RepoError::Other(format!("create sandbox pod: {e}")))?;
        let _ = self.wait_terminal(name, timeout).await;
        let phase = self.pod_phase(name).await?;
        let exit_code = self.pod_exit_code(name).await?;
        let logs = self.pod_logs(name).await.unwrap_or_default();
        let _ = self.delete_pod(name).await;
        Ok((phase, exit_code, logs))
    }

    /// Create a pod and stream its logs into `log_sink` (typically a file on
    /// /data) while it runs — the CI job-logs endpoint can then tail live
    /// output instead of waiting for the pod to finish (B4).
    pub async fn run_pod_streaming(
        &self,
        pod: Pod,
        log_sink: std::path::PathBuf,
        timeout: Duration,
    ) -> Result<(String, Option<i32>), RepoError> {
        let name = pod.metadata.name.clone().unwrap_or_default();
        self.pods()
            .create(&PostParams::default(), &pod)
            .await
            .map_err(|e| RepoError::Other(format!("create sandbox pod: {e}")))?;

        // Log writer task: reads the stream incrementally, appending to the
        // sink file. Ends when the pod's stream closes (termination).
        let pods = self.pods();
        let pod_name = name.clone();
        let writer = tokio::spawn(async move {
            use futures_util::AsyncBufReadExt as _;
            use tokio::io::AsyncWriteExt as _;
            // The kube log stream can fail while the container is still being
            // created (no logs yet); retry until it opens, then follow it.
            let mut buf = loop {
                match pods.log_stream(&pod_name, &{
                let mut lp = LogParams::default();
                lp.follow = true;
                lp
            }).await {
                    Ok(b) => break b,
                    Err(_) => tokio::time::sleep(Duration::from_millis(300)).await,
                }
            };
            if let Ok(mut f) = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_sink)
                .await
            {
                let mut line = String::new();
                loop {
                    line.clear();
                    match buf.read_line(&mut line).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let _ = f.write_all(line.as_bytes()).await;
                        }
                    }
                }
                let _ = f.flush().await;
            }
        });

        let _ = self.wait_terminal(&name, timeout).await;
        let _ = writer.await;
        let phase = self.pod_phase(&name).await?;
        let exit_code = self.pod_exit_code(&name).await?;
        let _ = self.delete_pod(&name).await;
        Ok((phase, exit_code))
    }

    /// Stream the pod's container logs (one container per sandbox pod).
    pub async fn pod_logs(&self, name: &str) -> Result<String, RepoError> {
        let buf = self
            .pods()
            .log_stream(name, &LogParams::default())
            .await
            .map_err(|e| RepoError::Other(format!("pod logs: {e}")))?;
        use futures_util::AsyncBufReadExt as _;
        let mut reader = buf;
        let mut out = String::new();
        reader
            .read_line(&mut out)
            .await
            .map_err(|e| RepoError::Other(format!("read pod logs: {e}")))?;
        Ok(out)
    }

    /// Poll until the pod phase is terminal (Succeeded/Failed) or timebox.
    async fn wait_terminal(&self, name: &str, timeout: Duration) -> Result<(), RepoError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let phase = self.pod_phase(name).await?;
            if matches!(phase.as_str(), "Succeeded" | "Failed") {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(()); // caller reports the (still-running) phase
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn pod_phase(&self, name: &str) -> Result<String, RepoError> {
        let pod = self
            .pods()
            .get(name)
            .await
            .map_err(|e| RepoError::Other(format!("get pod {name}: {e}")))?;
        Ok(pod
            .status
            .and_then(|s| s.phase)
            .unwrap_or_else(|| "Pending".to_string()))
    }

    async fn pod_exit_code(&self, name: &str) -> Result<Option<i32>, RepoError> {
        let pod = self
            .pods()
            .get(name)
            .await
            .map_err(|e| RepoError::Other(format!("get pod {name}: {e}")))?;
        Ok(pod.status.and_then(|s| {
            s.container_statuses.and_then(|cs| {
                cs.into_iter()
                    .next()
                    .and_then(|c| c.state.and_then(|st| st.terminated.map(|t| t.exit_code)))
            })
        }))
    }

    async fn delete_pod(&self, name: &str) -> Result<(), RepoError> {
        let _ = self
            .pods()
            .delete(name, &DeleteParams::default())
            .await
            .map_err(|e| RepoError::Other(format!("delete pod {name}: {e}")))?;
        Ok(())
    }

    /// List sandbox pods created by jjlab CI (diagnostics).
    pub async fn list_sandboxes(&self) -> Result<Vec<String>, RepoError> {
        let lp = ListParams::default().labels("app=jjlab-ci");
        let pods = self
            .pods()
            .list(&lp)
            .await
            .map_err(|e| RepoError::Other(format!("list pods: {e}")))?;
        Ok(pods.items.into_iter().map(|p| p.metadata.name.unwrap_or_default()).collect())
    }
}

/// Run a pod to completion in a given namespace, capturing the final phase,
/// exit code and container logs. Used by the `/ops/runs` primitive (a generic
/// debug pod, vs the CI-specific `run_sandbox`).
pub async fn run_pod_to_completion(
    client: Client,
    ns: &str,
    pod: Pod,
    timeout: Duration,
) -> RepoResult<(String, Option<i32>, String)> {
    let name = pod.metadata.name.clone().unwrap_or_default();
    let pods = Api::<Pod>::namespaced(client, ns);
    pods.create(&PostParams::default(), &pod)
        .await
        .map_err(|e| RepoError::Other(format!("create run pod {name}: {e}")))?;

    let deadline = tokio::time::Instant::now() + timeout;
    let final_phase = loop {
        let p = pods
            .get(&name)
            .await
            .map_err(|e| RepoError::Other(format!("get run pod {name}: {e}")))?;
        let phase = p
            .status
            .as_ref()
            .and_then(|s| s.phase.clone())
            .unwrap_or_else(|| "Pending".into());
        let terminal = matches!(phase.as_str(), "Succeeded" | "Failed");
        if terminal || tokio::time::Instant::now() >= deadline {
            break phase;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };

    let exit_code = pods.get(&name).await.ok().and_then(|p| {
        p.status.and_then(|s| s.container_statuses).and_then(|cs| {
            cs.into_iter()
                .next()
                .and_then(|c| c.state.and_then(|st| st.terminated.map(|t| t.exit_code)))
        })
    });

    let logs = read_pod_logs(&pods, &name).await.unwrap_or_default();

    let _ = pods.delete(&name, &DeleteParams::default()).await;
    Ok((final_phase, exit_code, logs))
}

/// Read a pod's container logs into a String (best-effort).
pub async fn read_pod_logs(pods: &Api<Pod>, name: &str) -> RepoResult<String> {
    use futures_util::AsyncBufReadExt as _;
    let mut buf = pods
        .log_stream(name, &LogParams::default())
        .await
        .map_err(|e| RepoError::Other(format!("pod logs: {e}")))?;
    let mut out = String::new();
    loop {
        match buf.read_line(&mut out).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
    Ok(out)
}

/// Serialize a sandbox Pod spec from JSON — keeps the pod shape auditable
/// without hand-building ~20 nested k8s-openapi structs.
///
/// When `snapshot_url` is set, an initContainer fetches the repo tarball from
/// jjlab's archive endpoint and unpacks it into the shared `workspace`
/// emptyDir, so the job's `run` executes against real repo content at
/// `/workspace` (empty when no snapshot was requested — e.g. synthetic repos).
pub fn sandbox_pod(name: &str, image: &str, run: &str, run_id: i64) -> Pod {
    sandbox_pod_full(name, image, run, run_id, &[], None)
}

/// Full-shape sandbox pod: extra env vars + an optional repo snapshot URL.
pub fn sandbox_pod_full(
    name: &str,
    image: &str,
    run: &str,
    run_id: i64,
    env: &[(String, String)],
    snapshot_url: Option<&str>,
) -> Pod {
    let env_json: Vec<serde_json::Value> = env
        .iter()
        .map(|(k, v)| serde_json::json!({ "name": k, "value": v }))
        .collect();
    let base = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": name,
            "labels": { "app": "jjlab-ci", "zergx/run": run_id.to_string() },
        },
        "spec": {
            "restartPolicy": "Never",
            "terminationGracePeriodSeconds": 5,
            "automountServiceAccountToken": false,
            "securityContext": { "runAsNonRoot": true, "runAsUser": 1000, "runAsGroup": 1000 },
            "containers": [{
                "name": "ci",
                "image": image,
                "command": ["sh", "-c", run],
                "workingDir": "/workspace",
                "env": env_json,
                "resources": {
                    "requests": { "cpu": "250m", "memory": "256Mi" },
                    "limits": { "cpu": "1", "memory": "1Gi" },
                },
                "securityContext": {
                    "allowPrivilegeEscalation": false,
                    "capabilities": { "drop": ["ALL"] },
                },
            }],
        },
    });

    let spec = match snapshot_url {
        Some(url) => {
            let mut spec = base;
            // The busybox image must come from the in-cluster registry too —
            // derived from the job image's registry host when possible, else
            // the known artifact default.
            let init_image = std::env::var("JJLAB_CI_INIT_IMAGE").unwrap_or_else(|_| {
                default_init_image_for(image)
            });
            spec["spec"]["initContainers"] = serde_json::json!([{
                "name": "repo-snapshot",
                "image": init_image,
                "command": ["sh", "-ec", format!(
                    "wget -qO- --header=\"Authorization: token $JJLAB_TOKEN\" '{url}' | tar -xzf - -C /workspace"
                )],
                "env": [{ "name": "JJLAB_TOKEN", "valueFrom": { "secretKeyRef": { "name": "jjlab-ci-token", "key": "token", "optional": true } } }],
                "resources": {
                    "requests": { "cpu": "50m", "memory": "64Mi" },
                    "limits": { "cpu": "500m", "memory": "512Mi" },
                },
            }]);
            spec["spec"]["volumes"] = serde_json::json!([{ "name": "workspace", "emptyDir": {} }]);
            spec["spec"]["initContainers"][0]["volumeMounts"] =
                serde_json::json!([{ "name": "workspace", "mountPath": "/workspace" }]);
            spec["spec"]["containers"][0]["volumeMounts"] =
                serde_json::json!([{ "name": "workspace", "mountPath": "/workspace" }]);
            spec
        }
        None => base,
    };
    serde_json::from_value(spec).expect("sandbox pod spec is well-formed")
}

/// Derive the registry host from the job image (same host, busybox:1.36) so
/// the init container resolves without public-internet access. Falls back to
/// the artifact registry when the image has no recognizable host.
fn default_init_image_for(job_image: &str) -> String {
    let artifact_default = "artifact.zergx.svc.cluster.local/library/busybox:1.36";
    // A host is the part before the first '/' when it contains '.' or ':'.
    match job_image.split_once('/') {
        Some((host, _)) if host.contains('.') || host.contains(':') => {
            format!("{host}/library/busybox:1.36")
        }
        _ => artifact_default.to_string(),
    }
}

/// Run a CI job pod to completion: `run_sandbox_with` plus an optional repo
/// snapshot URL (init container fetches the tarball into /workspace).
pub async fn run_pod_full(
    k8s: &K8s,
    name: &str,
    image: &str,
    run: &str,
    run_id: i64,
    timeout: Duration,
    env: &[(String, String)],
    snapshot_url: Option<&str>,
) -> Result<(String, Option<i32>, String), RepoError> {
    let pod = sandbox_pod_full(name, image, run, run_id, env, snapshot_url);
    k8s.run_pod(pod, name, timeout).await
}

/// A generic one-shot run pod (the `/ops/runs` primitive): a command argv
/// (not a shell string), env, and explicit resource limits.
pub fn generic_run_pod(
    name: &str,
    image: &str,
    argv: &[String],
    env: &std::collections::BTreeMap<String, String>,
    cpu: &str,
    memory: &str,
    timeout_secs: u64,
) -> Pod {
    let env_vars: Vec<serde_json::Value> = env
        .iter()
        .map(|(k, v)| serde_json::json!({ "name": k, "value": v }))
        .collect();
    let spec = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": name,
            "labels": { "app": "jjlab-run" },
        },
        "spec": {
            "restartPolicy": "Never",
            "activeDeadlineSeconds": timeout_secs,
            "terminationGracePeriodSeconds": 5,
            "automountServiceAccountToken": false,
            "containers": [{
                "name": "run",
                "image": image,
                "command": argv,
                "env": env_vars,
                "resources": {
                    "requests": { "cpu": cpu, "memory": memory },
                    "limits": { "cpu": "1", "memory": "1Gi" },
                },
                "securityContext": {
                    "allowPrivilegeEscalation": false,
                    "capabilities": { "drop": ["ALL"] },
                },
            }],
        },
    });
    serde_json::from_value(spec).expect("run pod spec is well-formed")
}

/// Result of spawning an external CLI (helm/buildctl/kubectl). Mirrors the
/// existing `git` subprocess pattern.
#[derive(Debug)]
pub struct CliOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

/// Spawn an external CLI with explicit argv and a timeout. No shell
/// interpolation: arguments are passed directly to `execvp`.
pub async fn run_cli(bin: &str, args: &[&str], timeout: Duration) -> Result<CliOutput, RepoError> {
    let child = tokio::process::Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| RepoError::Other(format!("spawn {bin}: {e}")))?;
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| RepoError::Other(format!("{bin} timed out after {}s", timeout.as_secs())))?
        .map_err(|e| RepoError::Other(format!("wait {bin}: {e}")))?;
    Ok(CliOutput {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    })
}

/// Invoke an external CLI, streaming merged stdout+stderr line-by-line into a
/// sink as it arrives (for buildctl/helm progress). Returns the final exit
/// code. Piping both streams through a shared sink requires a mutex; the sink
/// is wrapped in an `Arc<Mutex<_>>` across the two reader tasks.
pub async fn run_cli_stream(
    bin: &str,
    args: &[&str],
    timeout: Duration,
    sink: impl FnMut(&str) + Send + 'static,
) -> Result<i32, RepoError> {
    use tokio::io::AsyncBufReadExt as _;
    use std::sync::{Arc, Mutex};

    let mut child = tokio::process::Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| RepoError::Other(format!("spawn {bin}: {e}")))?;

    let sink = Arc::new(Mutex::new(sink));
    let mut readers = Vec::new();

    // stdout and stderr are distinct types (ChildStdout / ChildStderr), so
    // pump each pipe in its own task rather than iterating a mixed array.
    if let Some(out) = child.stdout.take() {
        let sink = sink.clone();
        readers.push(tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(mut s) = sink.lock() {
                    s(&line);
                }
            }
        }));
    }
    if let Some(err) = child.stderr.take() {
        let sink = sink.clone();
        readers.push(tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(mut s) = sink.lock() {
                    s(&line);
                }
            }
        }));
    }

    let status = tokio::time::timeout(timeout, child.wait()).await;
    // The reader tasks exit once the pipes close (on child exit); best-effort
    // join so a lingering reader doesn't outlive us with a stale sink.
    for r in readers {
        let _ = r.await;
    }
    match status {
        Ok(Ok(status)) => Ok(status.code().unwrap_or(-1)),
        Ok(Err(e)) => Err(RepoError::Other(format!("wait {bin}: {e}"))),
        Err(_) => Err(RepoError::Other(format!("{bin} timed out after {}s", timeout.as_secs()))),
    }
}

/// Invoke helm with argv (used by future `helm`-style CI steps).
pub async fn run_helm(args: &[&str], timeout: Duration) -> Result<CliOutput, RepoError> {
    run_cli("helm", args, timeout).await
}

/// Invoke buildctl with argv (used by future image-build CI steps).
pub async fn run_buildctl(args: &[&str], timeout: Duration) -> Result<CliOutput, RepoError> {
    run_cli("buildctl", args, timeout).await
}
#[cfg(test)]
mod sandbox_pod_tests {
    use super::*;

    #[test]
    fn plain_pod_has_no_init_container() {
        let p = sandbox_pod("t1", "img:1", "echo hi", 7);
        let spec = p.spec.unwrap();
        assert!(spec.init_containers.is_none());
        assert!(spec.volumes.is_none());
        let c = spec.containers.first().unwrap();
        assert!(c.env.is_none() || c.env.as_ref().unwrap().is_empty());
    }

    #[test]
    fn snapshot_pod_has_init_and_shared_volume() {
        let p = sandbox_pod_full(
            "t2",
            "artifact.example.invalid/rust:1",
            "cat /workspace/README",
            9,
            &[("FOO".into(), "bar".into())],
            Some("http://self/api/v1/repos/o/r/archive/tarball/main"),
        );
        let spec = p.spec.unwrap();
        let init = spec.init_containers.expect("init container present");
        assert_eq!(init.len(), 1);
        assert!(serde_json::to_string(&init[0]).unwrap().contains("tarball/main"));
        // both mounts target /workspace and share the "workspace" volume
        assert!(serde_json::to_string(&spec.volumes).unwrap().contains("workspace"));
        assert!(serde_json::to_string(&init[0].volume_mounts).unwrap().contains("/workspace"));
        let c = spec.containers.first().unwrap();
        assert_eq!(c.env.as_ref().unwrap()[0].name, "FOO");
    }

    #[test]
    fn init_image_follows_job_registry_host() {
        let p = sandbox_pod_full(
            "t3",
            "artifact.other.invalid:5000/rust:1",
            "true",
            3,
            &[],
            Some("http://self/x"),
        );
        let init = p.spec.unwrap().init_containers.unwrap();
        let s = serde_json::to_string(&init[0]).unwrap();
        assert!(s.contains("artifact.other.invalid:5000/library/busybox"), "{s}");
    }
}
