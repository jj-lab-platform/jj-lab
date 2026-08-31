//! Long-running "service" lifecycle for the `/ops` surface: a named workload
//! that keeps serving (as opposed to a one-shot `run`). Two shapes:
//!
//!   - `bare` — a single Pod + a ClusterIP Service (dev sandbox / interactive
//!     worker semantics)
//!   - `deployment` — a Deployment (replicas, rolling restart, scale,
//!     rollback) + a ClusterIP Service
//!
//! The caller (ops-extension) owns the *meaning* of the service (sandbox vs
//! user app vs helm-managed); jjlab only turns it into the right K8s objects.

use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::Deployment as K8sDeployment;
use k8s_openapi::api::core::v1::{
    Container, ContainerPort, EnvVar, Pod, PodSpec, Service as K8sService,
    ServicePort, ServiceSpec as K8sServiceSpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
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
    pub replicas: Option<i32>,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub resources: Option<ResourceSpec>,
}

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
    let mut list: Vec<ContainerPort> = ports
        .iter()
        .map(|p| ContainerPort { container_port: p.container, ..Default::default() })
        .collect();
    if list.is_empty() {
        list.push(ContainerPort { container_port: 8080, ..Default::default() });
    }
    list
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

fn container(spec: &ServiceRequest) -> Container {
    Container {
        name: spec.name.clone(),
        image: Some(spec.image.clone()),
        image_pull_policy: Some("Always".into()),
        env: Some(env_vars(&spec.env)),
        ports: Some(ports(&spec.ports)),
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

    pub async fn ensure(&self, spec: &ServiceRequest, ns: &str) -> RepoResult<()> {
        if spec.kind == "bare" {
            self.ensure_bare(spec, ns).await
        } else {
            self.ensure_deployment(spec, ns).await
        }
    }

    async fn ensure_bare(&self, spec: &ServiceRequest, ns: &str) -> RepoResult<()> {
        let labels = labels(&spec.name);
        let pod = Pod {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some(spec.name.clone()),
                namespace: Some(ns.to_string()),
                labels: Some(labels.clone()),
                ..Default::default()
            },
            spec: Some(PodSpec {
                restart_policy: Some("Never".into()),
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
        self.upsert_service(spec, ns).await
    }

    async fn ensure_deployment(&self, spec: &ServiceRequest, ns: &str) -> RepoResult<()> {
        let labels = labels(&spec.name);
        let replicas = spec.replicas.unwrap_or(1).max(1);
        let c = container(spec);
        let deploy = K8sDeployment {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some(spec.name.clone()),
                namespace: Some(ns.to_string()),
                labels: Some(labels.clone()),
                ..Default::default()
            },
            spec: Some(k8s_openapi::api::apps::v1::DeploymentSpec {
                replicas: Some(replicas),
                selector: LabelSelector {
                    match_labels: Some(labels.clone()),
                    ..Default::default()
                },
                template: k8s_openapi::api::core::v1::PodTemplateSpec {
                    metadata: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                        labels: Some(labels.clone()),
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
        self.upsert_service(spec, ns).await
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
            let replicas = d.spec.and_then(|s| s.replicas).unwrap_or(0);
            out.push(serde_json::json!({
                "name": name, "kind": "deployment", "replicas": replicas,
                "namespace": ns,
            }));
        }
        for p in self.pods(ns).list(&lp).await.map_err(|e| RepoError::Other(format!("list pods: {e}")))?.items {
            let name = p.metadata.name.clone().unwrap_or_default();
            let phase = p.status.and_then(|s| s.phase).unwrap_or_default();
            out.push(serde_json::json!({
                "name": name, "kind": "bare", "phase": phase, "namespace": ns,
            }));
        }
        Ok(out)
    }

    /// Report a service's status (deployment rollout or bare pod phase).
    pub async fn status(&self, name: &str, ns: &str) -> RepoResult<serde_json::Value> {
        if let Ok(d) = self.deployments(ns).get(name).await {
            let st = d.status.unwrap_or_default();
            return Ok(serde_json::json!({
                "name": name, "kind": "deployment",
                "replicas": st.replicas.unwrap_or(0),
                "ready": st.ready_replicas.unwrap_or(0),
                "available": st.available_replicas.unwrap_or(0),
            }));
        }
        if let Ok(p) = self.pods(ns).get(name).await {
            let phase = p.status.and_then(|s| s.phase).unwrap_or_default();
            return Ok(serde_json::json!({ "name": name, "kind": "bare", "phase": phase }));
        }
        Err(RepoError::NotFound { org: ns.to_string(), repo: name.to_string() })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_kind_is_deployment() {
        assert_eq!(default_kind(), "deployment");
    }
}