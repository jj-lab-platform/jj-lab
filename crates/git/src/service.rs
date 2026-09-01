//! Long-running "service" lifecycle for the `/ops` surface: a named workload
//! that keeps serving (as opposed to a one-shot `run`). Two shapes:
//!
//!   - `bare` — a single Pod + a ClusterIP Service (dev sandbox / interactive
//!     worker semantics)
//!   - `deployment` — a Deployment (replicas, rolling restart, scale,
//!     rollback) + a ClusterIP Service
//!
//! `ensure` is get-or-create: an existing healthy workload is left as-is (and
//! its address is reported), a missing one is created and awaited until the
//! first container is Ready (bare) / Available (deployment).
//!
//! The caller (ops-extension) owns the *meaning* of the service (sandbox vs
//! user app vs helm-managed); jjlab only turns it into the right K8s objects.

use std::collections::BTreeMap;
use std::time::Duration;

use k8s_openapi::api::apps::v1::Deployment as K8sDeployment;
use k8s_openapi::api::core::v1::{
    Container, ContainerPort, EnvVar, Pod, PodSpec, Probe, Service as K8sService, ServicePort,
    ServiceSpec as K8sServiceSpec, TCPSocketAction,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::{DeleteParams, PostParams};
use kube::Api;

use crate::repo::{RepoError, RepoResult};

/// A port mapping: container-port → service-port. Service port 0 → default 80.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PortSpec {
    #[serde(default)]
    pub container: i32,
    #[serde(default)]
    pub service: i32,
}

/// Resources for a service's containers.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ResourceSpec {
    #[serde(default)]
    pub cpu: String,
    #[serde(default)]
    pub memory: String,
}

impl Default for ResourceSpec {
    fn default() -> Self {
        ResourceSpec { cpu: "250m".into(), memory: "256Mi".into() }
    }
}

/// A service create/update request (the `/ops` primitives, purpose-neutral).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ServiceRequest {
    pub name: String,
    pub image: String,
    /// "bare" (pod) or "deployment" (replicas + rollout).
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub ports: Vec<PortSpec>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
    #[serde(default)]
    pub replicas: Option<i32>,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub resources: Option<ResourceSpec>,
}

/// Default timeout for the ensure wait (pod Ready / deployment Available).
const ENSURE_WAIT_TIMEOUT: Duration = Duration::from_secs(120);

fn default_kind() -> String {
    "deployment".to_string()
}

/// Labels applied to everything jjlab owns in a service.
fn labels(name: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("app".to_string(), name.to_string());
    m.insert("app.kubernetes.io/managed-by".to_string(), "jjlab".to_string());
    m
}

fn env_vars(env: &BTreeMap<String, String>) -> Vec<EnvVar> {
    env.iter().map(|(k, v)| EnvVar { name: k.clone(), value: Some(v.clone()), ..Default::default() }).collect()
}

fn ports(ports: &[PortSpec]) -> Vec<ContainerPort> {
    let list: Vec<ContainerPort> = ports
        .iter()
        .map(|p| ContainerPort { container_port: p.container, ..Default::default() })
        .collect();
    if list.is_empty() {
        vec![ContainerPort { container_port: 8080, ..Default::default() }]
    } else {
        list
    }
}

fn service_ports(ports: &[PortSpec]) -> Vec<ServicePort> {
    if ports.is_empty() {
        return vec![ServicePort {
            name: Some("http".into()),
            port: 80,
            target_port: Some(k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(8080)),
            ..Default::default()
        }];
    }
    ports
        .iter()
        .map(|p| ServicePort {
            name: Some("http".into()),
            port: if p.service == 0 { 80 } else { p.service },
            target_port: Some(k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(p.container)),
            ..Default::default()
        })
        .collect()
}

/// A single TCP readiness probe on the first declared container port (falls
/// back to the container's default 8080).
fn tcp_probe(container_port: i32) -> Probe {
    Probe {
        tcp_socket: Some(TCPSocketAction {
            port: IntOrString::Int(container_port),
            ..Default::default()
        }),
        initial_delay_seconds: Some(2),
        period_seconds: Some(2),
        failure_threshold: Some(30),
        ..Default::default()
    }
}

fn container(spec: &ServiceRequest) -> Container {
    let cps = ports(&spec.ports);
    let probe_port = cps.first().map(|p| p.container_port).unwrap_or(8080);
    // ResourceQuota-safe: pods in quota'd namespaces must carry explicit
    // requests+limits (defaults mirror the old ops-extension values).
    let (cpu, memory) = match &spec.resources {
        Some(r) if !r.cpu.is_empty() || !r.memory.is_empty() => (
            if r.cpu.is_empty() { "250m".to_string() } else { r.cpu.clone() },
            if r.memory.is_empty() { "256Mi".to_string() } else { r.memory.clone() },
        ),
        _ => ("250m".to_string(), "256Mi".to_string()),
    };
    Container {
        name: spec.name.clone(),
        image: Some(spec.image.clone()),
        image_pull_policy: Some("Always".into()),
        env: Some(env_vars(&spec.env)),
        ports: Some(cps),
        readiness_probe: Some(tcp_probe(probe_port)),
        resources: Some(k8s_openapi::api::core::v1::ResourceRequirements {
            requests: Some(BTreeMap::from([
                ("cpu".to_string(), k8s_openapi::apimachinery::pkg::api::resource::Quantity(cpu.clone())),
                ("memory".to_string(), k8s_openapi::apimachinery::pkg::api::resource::Quantity(memory.clone())),
            ])),
            limits: Some(BTreeMap::from([
                ("cpu".to_string(), k8s_openapi::apimachinery::pkg::api::resource::Quantity(cpu.clone())),
                ("memory".to_string(), k8s_openapi::apimachinery::pkg::api::resource::Quantity(memory.clone())),
            ])),
            claims: None,
        }),
        ..Default::default()
    }
}

/// A client handle carrying the kube client only; the namespace is per-call
/// (resolved against the namespace registry by the caller).
#[derive(Clone)]
pub struct ServiceClient {
    client: kube::Client,
}

impl ServiceClient {
    pub fn new(client: kube::Client) -> Self {
        ServiceClient { client }
    }

    fn deployments(&self, ns: &str) -> Api<K8sDeployment> {
        Api::namespaced(self.client.clone(), ns)
    }
    fn pods(&self, ns: &str) -> Api<Pod> {
        Api::namespaced(self.client.clone(), ns)
    }
    fn services(&self, ns: &str) -> Api<K8sService> {
        Api::namespaced(self.client.clone(), ns)
    }

    pub async fn ensure(&self, spec: &ServiceRequest, ns: &str) -> RepoResult<ServiceStatus> {
        if spec.kind == "bare" {
            self.ensure_bare(spec, ns).await
        } else {
            self.ensure_deployment(spec, ns).await
        }
    }

    fn object_meta(&self, spec: &ServiceRequest, ns: &str) -> k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
        k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some(spec.name.clone()),
            namespace: Some(ns.to_string()),
            labels: Some(labels(&spec.name)),
            annotations: if spec.annotations.is_empty() {
                None
            } else {
                Some(spec.annotations.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            },
            ..Default::default()
        }
    }

    async fn ensure_bare(&self, spec: &ServiceRequest, ns: &str) -> RepoResult<ServiceStatus> {
        // Get-or-create: an existing pod is reused (its spec is NOT mutated —
        // callers that changed the image should delete + ensure first).
        if let Some(pod) = self.pods(ns).get_opt(&spec.name).await.map_err(kube_err("get pod"))? {
            if pod.metadata.deletion_timestamp.is_none() {
                self.upsert_service(spec, ns).await?;
                return self.wait_bare_ready(&spec.name, ns).await;
            }
            // Terminating: wait for it to disappear, then recreate below.
            let deadline = tokio::time::Instant::now() + ENSURE_WAIT_TIMEOUT;
            while self.pods(ns).get_opt(&spec.name).await.map_err(kube_err("get pod"))?.is_some() {
                if tokio::time::Instant::now() >= deadline {
                    return Err(RepoError::Other(format!(
                        "pod {} is stuck terminating",
                        spec.name
                    )));
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        let pod = Pod {
            metadata: self.object_meta(spec, ns),
            spec: Some(PodSpec {
                restart_policy: Some("Always".into()),
                containers: vec![container(spec)],
                ..Default::default()
            }),
            ..Default::default()
        };
        let _ = self
            .pods(ns)
            .create(&PostParams::default(), &pod)
            .await
            .map_err(|e| RepoError::Other(format!("create pod {}: {e}", spec.name)))?;
        self.upsert_service(spec, ns).await?;
        self.wait_bare_ready(&spec.name, ns).await
    }

    async fn ensure_deployment(&self, spec: &ServiceRequest, ns: &str) -> RepoResult<ServiceStatus> {
        // Get-or-create: an existing deployment is left untouched (scale/
        // restart endpoints own mutation). A terminating one must finish
        // deleting before the name can be reused.
        if let Some(d) = self.deployments(ns).get_opt(&spec.name).await.map_err(kube_err("get deployment"))? {
            if d.metadata.deletion_timestamp.is_none() {
                self.upsert_service(spec, ns).await?;
                return self.wait_deployment_ready(&spec.name, ns).await;
            }
            let deadline = tokio::time::Instant::now() + ENSURE_WAIT_TIMEOUT;
            while self.deployments(ns).get_opt(&spec.name).await.map_err(kube_err("get deployment"))?.is_some() {
                if tokio::time::Instant::now() >= deadline {
                    return Err(RepoError::Other(format!(
                        "deployment {} is stuck terminating",
                        spec.name
                    )));
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        let replicas = spec.replicas.unwrap_or(1).max(1);
        let c = container(spec);
        let deploy = K8sDeployment {
            metadata: self.object_meta(spec, ns),
            spec: Some(k8s_openapi::api::apps::v1::DeploymentSpec {
                replicas: Some(replicas),
                selector: LabelSelector {
                    match_labels: Some(labels(&spec.name)),
                    ..Default::default()
                },
                template: k8s_openapi::api::core::v1::PodTemplateSpec {
                    metadata: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                        labels: Some(labels(&spec.name)),
                        ..Default::default()
                    }),
                    spec: Some(PodSpec {
                        restart_policy: Some("Always".into()),
                        containers: vec![c],
                        ..Default::default()
                    }),
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        let _ = self
            .deployments(ns)
            .create(&PostParams::default(), &deploy)
            .await
            .map_err(|e| RepoError::Other(format!("create deployment {}: {e}", spec.name)))?;
        self.upsert_service(spec, ns).await?;
        self.wait_deployment_ready(&spec.name, ns).await
    }

    /// Poll the bare pod until its first container reports Ready.
    async fn wait_bare_ready(&self, name: &str, ns: &str) -> RepoResult<ServiceStatus> {
        let deadline = tokio::time::Instant::now() + ENSURE_WAIT_TIMEOUT;
        loop {
            let status = self.status(name, ns).await?;
            if status.ready {
                return Ok(status);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(RepoError::Other(format!(
                    "pod {name} not ready after {:?} (phase {})",
                    ENSURE_WAIT_TIMEOUT, status.phase
                )));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Poll the deployment until it reports Available replicas.
    async fn wait_deployment_ready(&self, name: &str, ns: &str) -> RepoResult<ServiceStatus> {
        let deadline = tokio::time::Instant::now() + ENSURE_WAIT_TIMEOUT;
        loop {
            let status = self.status(name, ns).await?;
            if status.ready {
                return Ok(status);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(RepoError::Other(format!(
                    "deployment {name} not available after {:?}",
                    ENSURE_WAIT_TIMEOUT
                )));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn upsert_service(&self, spec: &ServiceRequest, ns: &str) -> RepoResult<()> {
        let labels = labels(&spec.name);
        let svc = K8sService {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some(spec.name.clone()),
                namespace: Some(ns.to_string()),
                labels: Some(labels.clone()),
                ..Default::default()
            },
            spec: Some(K8sServiceSpec {
                selector: Some(labels.clone()),
                ports: Some(service_ports(&spec.ports)),
                ..Default::default()
            }),
            ..Default::default()
        };
        // Delete-if-exists then recreate is simplest; Service clusterIP is
        // mutable-on-recreate for a dev orchestrator surface.
        let _ = self.services(ns).delete(&spec.name, &DeleteParams::default()).await;
        let _ = self.services(ns).create(&PostParams::default(), &svc).await.map_err(|e| {
            RepoError::Other(format!("create service {}: {e}", spec.name))
        })?;
        Ok(())
    }

    pub async fn delete(&self, name: &str, ns: &str) -> RepoResult<()> {
        let _ = self.deployments(ns).delete(name, &DeleteParams::default()).await;
        let _ = self.pods(ns).delete(name, &DeleteParams::default()).await;
        let _ = self.services(ns).delete(name, &DeleteParams::default()).await;
        Ok(())
    }

    /// List services (deployments + bare pods) owned by jjlab.
    pub async fn list(&self, ns: &str) -> RepoResult<Vec<serde_json::Value>> {
        use kube::api::ListParams;
        let mut out = Vec::new();
        let lp = ListParams::default().labels("app.kubernetes.io/managed-by=jjlab");
        for d in self.deployments(ns).list(&lp).await.map_err(|e| RepoError::Other(format!("list deployments: {e}")))?.items {
            let name = d.metadata.name.clone().unwrap_or_default();
            let replicas = d.spec.as_ref().and_then(|s| s.replicas).unwrap_or(0);
            out.push(serde_json::json!({
                "name": name, "kind": "deployment", "replicas": replicas,
                "namespace": ns,
            }));
        }
        for p in self.pods(ns).list(&lp).await.map_err(|e| RepoError::Other(format!("list pods: {e}")))?.items {
            let name = p.metadata.name.clone().unwrap_or_default();
            let phase = p.status.as_ref().and_then(|s| s.phase.clone()).unwrap_or_default();
            let pod_ip = p.status.as_ref().and_then(|s| s.pod_ip.clone()).unwrap_or_default();
            let session = p
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get("zergx/session").cloned())
                .unwrap_or_default();
            out.push(serde_json::json!({
                "name": name, "kind": "bare", "phase": phase, "namespace": ns,
                "pod_ip": pod_ip, "session": session,
            }));
        }
        Ok(out)
    }

    /// Report a service's status (deployment rollout or bare pod phase).
    /// `ready` mirrors pod Ready / deployment Availability; `worker_url` is
    /// the first container port on the pod IP (sandbox workers are addressed
    /// pod-directly, not through the Service).
    pub async fn status(&self, name: &str, ns: &str) -> RepoResult<ServiceStatus> {
        if let Ok(d) = self.deployments(ns).get(name).await {
            let st = d.status.unwrap_or_default();
            let ready = st.available_replicas.unwrap_or(0) > 0;
            let pod_ip = self.first_pod_ip(name, ns).await;
            return Ok(ServiceStatus {
                name: name.to_string(),
                kind: "deployment".to_string(),
                replicas: st.replicas.unwrap_or(0),
                ready,
                phase: String::new(),
                pod_ip,
            });
        }
        if let Ok(p) = self.pods(ns).get(name).await {
            let phase = p.status.as_ref().and_then(|s| s.phase.clone()).unwrap_or_default();
            let ready = p
                .status
                .as_ref()
                .and_then(|s| s.conditions.as_ref())
                .map(|cs| cs.iter().any(|c| c.type_ == "Ready" && c.status == "True"))
                .unwrap_or(false);
            let pod_ip = p.status.as_ref().and_then(|s| s.pod_ip.clone()).unwrap_or_default();
            return Ok(ServiceStatus {
                name: name.to_string(),
                kind: "bare".to_string(),
                replicas: 1,
                ready,
                phase,
                pod_ip,
            });
        }
        Err(RepoError::NotFound { org: ns.to_string(), repo: name.to_string() })
    }

    /// The pod IP of the first pod behind a deployment (best-effort; bare
    /// pods resolve directly).
    async fn first_pod_ip(&self, name: &str, ns: &str) -> String {
        let lp = ListParams::default().labels(&format!("app={name}"));
        match self.pods(ns).list(&lp).await {
            Ok(list) => list
                .items
                .first()
                .and_then(|p| p.status.as_ref().and_then(|s| s.pod_ip.clone()))
                .unwrap_or_default(),
            Err(_) => String::new(),
        }
    }

    /// Restart a service: rollout restart for deployments (rewrite the pod
    /// template annotation to force a new ReplicaSet). Bare pods restart by
    /// the caller re-issuing ensure.
    pub async fn restart(&self, name: &str, ns: &str) -> RepoResult<()> {
        let mut d = self
            .deployments(ns)
            .get(name)
            .await
            .map_err(|e| RepoError::Other(format!("get deployment: {e}")))?;
        // Force a rollout by touching a template annotation.
        let stamp = format!("{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
        if let Some(spec) = d.spec.as_mut() {
            let annotations = &mut spec.template.metadata.get_or_insert_default().annotations;
            annotations.get_or_insert_default().insert("jjlab/restart".into(), stamp);
        }
        self.deployments(ns)
            .replace(name, &PostParams::default(), &d)
            .await
            .map_err(|e| RepoError::Other(format!("restart deployment: {e}")))?;
        Ok(())
    }

    /// Scale a deployment's replicas.
    pub async fn scale(&self, name: &str, replicas: i32, ns: &str) -> RepoResult<()> {
        let mut d = self.deployments(ns).get(name).await.map_err(|e| RepoError::Other(format!("get deployment: {e}")))?;
        if let Some(spec) = d.spec.as_mut() {
            spec.replicas = Some(replicas.max(0));
        }
        self.deployments(ns).replace(name, &PostParams::default(), &d).await
            .map_err(|e| RepoError::Other(format!("scale deployment: {e}")))?;
        Ok(())
    }
}

/// Uniform status payload for `/ops/services/{name}` and `ensure` responses.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ServiceStatus {
    pub name: String,
    pub kind: String,
    pub replicas: i32,
    pub ready: bool,
    /// Bare pod phase ("Running" etc.); empty for deployments.
    pub phase: String,
    /// First container port on the pod IP: `http://<pod-ip>:<port>`. Empty
    /// until the pod is scheduled and its IP is assigned.
    pub pod_ip: String,
}

impl ServiceStatus {
    /// `http://<pod-ip>:<first-container-port>`; the port is inferred from
    /// kind-specific knowledge the caller has (worker sandbox = 48080), so
    /// this is provided by the handler layer, not here.
    pub fn worker_url_on(&self, container_port: i32) -> String {
        if self.pod_ip.is_empty() {
            String::new()
        } else {
            format!("http://{}:{}", self.pod_ip, container_port)
        }
    }
}

use kube::api::ListParams;

fn kube_err(what: &'static str) -> impl Fn(kube::Error) -> RepoError {
    move |e| RepoError::Other(format!("{what}: {e}"))
}

/// Best-effort first container port of a service's pod (handler-layer helper
/// for building `worker_url` without threading the spec around).
pub async fn first_container_port_of_pod(
    status: &ServiceStatus,
    client: &kube::Client,
    ns: &str,
    name: &str,
) -> i32 {
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);
    let pod = match pods.get_opt(name).await {
        Ok(Some(p)) => p,
        _ => return 0,
    };
    if let Some(port) = pod
        .spec
        .as_ref()
        .and_then(|s| s.containers.first())
        .and_then(|c| c.ports.as_ref())
        .and_then(|ps| ps.first())
        .map(|p| p.container_port)
    {
        return port;
    }
    let _ = status;
    0
}


/// The pods behind a service (bare = the pod itself; deployment = selector).
pub async fn service_pods(client: &kube::Client, name: &str, ns: &str) -> RepoResult<Vec<serde_json::Value>> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);
    let lp = ListParams::default().labels(&format!("app={name}"));
    let list = pods
        .list(&lp)
        .await
        .map_err(|e| RepoError::Other(format!("list pods: {e}")))?;
    Ok(list
        .items
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.metadata.name.clone().unwrap_or_default(),
                "phase": p.status.as_ref().and_then(|s| s.phase.clone()).unwrap_or_default(),
                "pod_ip": p.status.as_ref().and_then(|s| s.pod_ip.clone()).unwrap_or_default(),
                "node": p.spec.as_ref().and_then(|s| s.node_name.clone()).unwrap_or_default(),
                "started_at": p.status.as_ref()
                    .and_then(|s| s.container_statuses.as_ref())
                    .and_then(|cs| cs.first())
                    .and_then(|c| c.state.as_ref())
                    .and_then(|st| st.running.as_ref())
                    .and_then(|r| r.started_at.clone())
                    .map(|t| t.0.to_string()),
            })
        })
        .collect())
}

/// One k8s Event associated with a service (rollout debugging).
async fn service_events_inner(client: &kube::Client, name: &str, ns: &str) -> RepoResult<Vec<serde_json::Value>> {
    use k8s_openapi::api::core::v1::Event as K8sEvent;
    let events: Api<K8sEvent> = Api::namespaced(client.clone(), ns);
    let list = events
        .list(&ListParams::default())
        .await
        .map_err(|e| RepoError::Other(format!("list events: {e}")))?;
    Ok(list
        .items
        .iter()
        .filter(|e| {
            let obj = e.involved_object.name.clone().unwrap_or_default();
            obj == name || obj.starts_with(&format!("{name}-"))
        })
        .map(|e| {
            let age = e.last_timestamp.as_ref().map(|t| t.0.to_string());
            serde_json::json!({
                "reason": e.reason.clone().unwrap_or_default(),
                "message": e.message.clone().unwrap_or_default(),
                "type": e.type_.clone().unwrap_or_default(),
                "last_timestamp": age,
            })
        })
        .collect())
}

/// Public event shape (name-qualified) used by the handler.
pub async fn service_events(client: &kube::Client, name: &str, ns: &str) -> RepoResult<Vec<serde_json::Value>> {
    service_events_inner(client, name, ns).await
}

/// ReplicaSet revisions of a deployment, newest-first.
pub async fn service_revisions(client: &kube::Client, name: &str, ns: &str) -> RepoResult<Vec<serde_json::Value>> {
    use k8s_openapi::api::apps::v1::ReplicaSet as K8sRS;
    let rss: Api<K8sRS> = Api::namespaced(client.clone(), ns);
    let lp = ListParams::default().labels(&format!("app={name}"));
    let list = rss
        .list(&lp)
        .await
        .map_err(|e| RepoError::Other(format!("list replicasets: {e}")))?;
    let mut out: Vec<serde_json::Value> = list
        .items
        .iter()
        .map(|rs| {
            let rev = rs
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get("deployment.kubernetes.io/revision"))
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            let img = rs
                .spec
                .as_ref()
                .and_then(|s| s.template.as_ref()).and_then(|t| t.spec.as_ref())
                .and_then(|s| s.containers.first())
                .and_then(|c| c.image.clone())
                .unwrap_or_default();
            let st = rs.status.as_ref();
            let (st_replicas, st_ready) = st
                .map(|s| (s.replicas, s.ready_replicas.unwrap_or(0)))
                .unwrap_or((0, 0));
            serde_json::json!({
                "revision": rev,
                "image": img,
                "replicas": st_replicas,
                "ready": st_ready,
                "created_at": rs.metadata.creation_timestamp.clone().map(|t| t.0.to_string()),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        let ra = a["revision"].as_i64().unwrap_or(0);
        let rb = b["revision"].as_i64().unwrap_or(0);
        rb.cmp(&ra)
    });
    Ok(out)
}

/// Rollback a deployment by replaying the target ReplicaSet's pod template
/// (`revision == 0` = previous revision).
pub async fn service_rollback(client: &kube::Client, name: &str, revision: i64, ns: &str) -> RepoResult<()> {
    use k8s_openapi::api::apps::v1::ReplicaSet as K8sRS;
    let rss: Api<K8sRS> = Api::namespaced(client.clone(), ns);
    let lp = ListParams::default().labels(&format!("app={name}"));
    let list = rss
        .list(&lp)
        .await
        .map_err(|e| RepoError::Other(format!("list replicasets: {e}")))?;

    let rev_of = |rs: &K8sRS| -> i64 {
        rs.metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get("deployment.kubernetes.io/revision"))
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
    };

    let target = if revision > 0 {
        list.items
            .iter()
            .find(|rs| rev_of(rs) == revision)
            .cloned()
            .ok_or_else(|| RepoError::Other(format!("revision {revision} not found")))?
    } else if revision == 0 {
        let deployments: Api<K8sDeployment> = Api::namespaced(client.clone(), ns);
        let d = deployments
            .get(name)
            .await
            .map_err(|e| RepoError::Other(format!("get deployment: {e}")))?;
        let cur = d
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get("deployment.kubernetes.io/revision"))
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let best = list
            .items
            .iter()
            .filter(|rs| rev_of(rs) < cur)
            .max_by_key(|rs| rev_of(rs))
            .cloned()
            .ok_or_else(|| RepoError::Other("no previous revision to roll back to".to_string()))?;
        best
    } else {
        return Err(RepoError::Other("revision must be >= 0".to_string()));
    };

    let deployments: Api<K8sDeployment> = Api::namespaced(client.clone(), ns);
    let mut d = deployments
        .get(name)
        .await
        .map_err(|e| RepoError::Other(format!("get deployment: {e}")))?;
    if let Some(spec) = d.spec.as_mut() {
        spec.template = target.spec.expect("RS has a template").template.expect("RS template is set");
        // Selector must stay stable: re-apply the owned labels.
        spec.template
            .metadata
            .get_or_insert_default()
            .labels
            .get_or_insert_default()
            .insert("app".into(), name.to_string());
    }
    deployments
        .replace(name, &PostParams::default(), &d)
        .await
        .map_err(|e| RepoError::Other(format!("rollback deployment: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_kind_is_deployment() {
        assert_eq!(default_kind(), "deployment");
    }
}