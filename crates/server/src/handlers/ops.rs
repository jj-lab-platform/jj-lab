//! `/ops` handlers: the generic orchestration primitives jjlab exposes to the
//! ops-extension. No agent/NATS/tool shaping lives here — the extension owns
//! all of that; these endpoints are purpose-neutral run/service/build/helm/
//! namespace operations.

use crate::*;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use axum::Json;
use serde_json::json;

use jjlab_git::task::TaskKind;

/// The kube client is built lazily and cached; a single client serves all
/// per-request namespaces (Api::namespaced reuses it).
pub(crate) async fn k8s_client() -> Result<kube::Client, Response> {
    match jjlab_git::runtime::connect_client().await {
        Ok((client, _)) => Ok(client),
        Err(e) => Err(json_err(StatusCode::SERVICE_UNAVAILABLE, e.to_string())),
    }
}

#[allow(clippy::result_large_err)]
pub(crate) fn resolve_ns(state: &AppState, requested: Option<&str>) -> Result<String, Response> {
    state
        .namespaces
        .resolve(requested)
        .map_err(|e| json_err(StatusCode::BAD_REQUEST, e.to_string()))
}

/// `POST /ops/namespaces` — approve a runtime namespace (idempotent).
pub async fn register_namespace(
    State(state): State<AppState>,
    Json(body): Json<NamespaceBody>,
) -> Response {
    match state.namespaces.register(&body.namespace) {
        Ok(_) => Json(json!({ "ok": true, "namespaces": state.namespaces.list() })).into_response(),
        Err(e) => json_err(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

/// `GET /ops/config` — the ops surface's capabilities and defaults.
pub async fn ops_config(State(state): State<AppState>) -> Response {
    Json(json!({
        "namespaces": state.namespaces.list(),
        "default_namespace": state.namespaces.default(),
        "buildkit_addr": jjlab_git::build::buildkit_addr(),
    }))
    .into_response()
}

/// `POST /ops/runs` — start a one-shot run; returns a run id immediately.
pub async fn ops_run(
    State(state): State<AppState>,
    Json(body): Json<RunBody>,
) -> Response {
    let ns = match resolve_ns(&state, body.namespace.as_deref()) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let client = match k8s_client().await {
        Ok(c) => c,
        Err(e) => return e,
    };
    let task = state.tasks.create(
        jjlab_git::task::TaskRegistry::new_id(),
        TaskKind::Build,
    );
    let task_id = task.id.clone();
    let response_id = task_id.clone();
    let image = body.image.clone();
    let command = body.command.clone();
    let env = body.env.clone();
    let cpu = body.cpu.clone().unwrap_or_else(|| "250m".into());
    let memory = body.memory.clone().unwrap_or_else(|| "256Mi".into());
    let timeout = body.timeout_secs.unwrap_or(300);

    tokio::spawn(async move {
        let mut args: Vec<String> = vec![];
        let run_cmd = command.clone().unwrap_or_else(|| "sh".to_string());
        if !run_cmd.contains(' ') {
            args.push(run_cmd);
        } else {
            args.push("sh".into());
            args.push("-c".into());
            args.push(run_cmd);
        }
        let pod = jjlab_git::runtime::generic_run_pod(
            &format!("jjlab-run-{task_id}"),
            &image,
            &args,
            &env,
            &cpu,
            &memory,
            timeout,
        );
        match jjlab_git::runtime::run_pod_to_completion(
            client,
            &ns,
            pod,
            std::time::Duration::from_secs(timeout + 30),
        )
        .await
        {
            Ok((phase, exit_code, logs)) => {
                for line in logs.lines() {
                    task.log(line);
                }
                let ok = phase == "Succeeded" || exit_code == Some(0);
                task.finish(
                    ok,
                    Some(format!("phase={phase} exit={:?}", exit_code)),
                    if ok { None } else { Some(format!("run exited with phase {phase}")) },
                );
            }
            Err(e) => {
                task.log(&format!("error: {e}"));
                task.finish(false, None, Some(e.to_string()));
            }
        }
    });
    Json(json!({ "ok": true, "run_id": response_id })).into_response()
}

/// `POST /ops/services` — create/update a long-running service.
pub async fn ops_service_create(
    State(state): State<AppState>,
    Json(body): Json<jjlab_git::service::ServiceRequest>,
) -> Response {
    let ns = match resolve_ns(&state, body.namespace.as_deref()) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let client = match k8s_client().await {
        Ok(c) => c,
        Err(e) => return e,
    };
    let sc = jjlab_git::service::ServiceClient::new(client);
    let name = body.name.clone();
    let kind = body.kind.clone();
    let image = body.image.clone();
    match sc.ensure(&body, &ns).await {
        Ok(st) => Json(json!({
            "ok": true, "name": name, "kind": kind, "image": image,
            "namespace": ns, "ready": st.ready, "pod_ip": st.pod_ip,
        }))
        .into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `GET /ops/namespaces` — list approved namespaces.
pub async fn ops_namespaces(State(state): State<AppState>) -> Response {
    Json(json!({ "namespaces": state.namespaces.list(), "default": state.namespaces.default() }))
        .into_response()
}

/// `GET /ops/tasks/{id}` and `GET /ops/tasks/{id}/stream` — task progress.
pub async fn ops_task_get(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.tasks.get(&id) {
        Some(t) => Json(t.summary()).into_response(),
        None => json_err(StatusCode::NOT_FOUND, format!("task {id} not found")),
    }
}

pub async fn ops_task_stream(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(task) = state.tasks.get(&id) else {
        return json_err(StatusCode::NOT_FOUND, format!("task {id} not found"));
    };
    let stream = async_stream::stream! {
        for line in task.snapshot_logs() {
            yield Ok::<_, std::convert::Infallible>(Event::default().event("log").data(line));
        }
        yield Ok::<_, std::convert::Infallible>(Event::default().event("state").data(task.status()));
        let mut rx = task.subscribe();
        while let Ok(ev) = rx.recv().await {
            let name = ev.event.clone();
            let data = ev.data.clone();
            let e = Event::default().event(name.clone()).data(data);
            yield Ok::<_, std::convert::Infallible>(e);
            if name == "state" && (ev.data == "done" || ev.data == "failed") {
                break;
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}

pub async fn ops_tasks_list(State(state): State<AppState>) -> Response {
    let tasks: Vec<serde_json::Value> = state.tasks.list().iter().map(|t| t.summary()).collect();
    Json(json!({ "tasks": tasks })).into_response()
}

/// `GET /ops/services` — list services (deployment + bare pods) in a namespace.
pub async fn ops_services_list(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let ns = match resolve_ns(&state, q.get("namespace").map(String::as_str)) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let client = match k8s_client().await {
        Ok(c) => c,
        Err(e) => return e,
    };
    let sc = jjlab_git::service::ServiceClient::new(client);
    match sc.list(&ns).await {
        Ok(items) => Json(json!({ "services": items })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `GET /ops/services/{name}` — a service's status.
pub async fn ops_service_get(
    State(state): State<AppState>,
    Path(name): Path<String>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let ns = match resolve_ns(&state, q.get("namespace").map(String::as_str)) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let client = match k8s_client().await {
        Ok(c) => c,
        Err(e) => return e,
    };
    let sc = jjlab_git::service::ServiceClient::new(client.clone());
    match sc.status(&name, &ns).await {
        Ok(s) => {
            let port = jjlab_git::service::first_container_port_of_pod(&s, &client, &ns, &name).await;
            let body = json!({
                "name": s.name, "kind": s.kind, "replicas": s.replicas,
                "ready": s.ready, "phase": s.phase, "pod_ip": s.pod_ip,
                "worker_url": s.worker_url_on(port),
            });
            Json(body).into_response()
        }
        Err(e) => json_err(StatusCode::NOT_FOUND, e.to_string()),
    }
}

/// `DELETE /ops/services/{name}`.
pub async fn ops_service_delete(
    State(state): State<AppState>,
    Path(name): Path<String>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let ns = match resolve_ns(&state, q.get("namespace").map(String::as_str)) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let client = match k8s_client().await {
        Ok(c) => c,
        Err(e) => return e,
    };
    let sc = jjlab_git::service::ServiceClient::new(client);
    match sc.delete(&name, &ns).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `POST /ops/services/{name}` — restart a deployment.
pub async fn ops_service_restart(
    State(state): State<AppState>,
    Path(name): Path<String>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let ns = match resolve_ns(&state, q.get("namespace").map(String::as_str)) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let client = match k8s_client().await {
        Ok(c) => c,
        Err(e) => return e,
    };
    let sc = jjlab_git::service::ServiceClient::new(client);
    match sc.restart(&name, &ns).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `POST /ops/services/{name}/scale` — scale a deployment.
pub async fn ops_service_scale(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<ScaleBody>,
) -> Response {
    let ns = match resolve_ns(&state, body.namespace.as_deref()) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let client = match k8s_client().await {
        Ok(c) => c,
        Err(e) => return e,
    };
    let sc = jjlab_git::service::ServiceClient::new(client);
    match sc.scale(&name, body.replicas, &ns).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `POST /ops/builds` — enqueue a build (repo checkout or raw Containerfile).
pub async fn ops_build(
    State(state): State<AppState>,
    Json(body): Json<jjlab_git::build::BuildRequest>,
) -> Response {
    let task = state.tasks.create(
        jjlab_git::task::TaskRegistry::new_id(),
        TaskKind::Build,
    );
    let task_id = task.id.clone();
    let response_id = task_id.clone();

    // Materialize the repo checkout (blocking jj) if a repo build was asked.
    let store = state.store.clone();
    let org = body.org.clone();
    let repo = body.repo.clone();
    let bookmark = body.bookmark.clone();
    let image = body.image.clone();
    let export = body.export.clone();
    let build_args = body.build_args.clone();
    let no_cache = body.no_cache;
    let containerfile = body.containerfile.clone();
    let raw = body.raw;
    let dockerfile = body.dockerfile.clone();

    tokio::spawn(async move {
        let task2 = task.clone();
        let sink = move |line: &str| task2.log(line);

        let mut ctx_dir: Option<std::path::PathBuf> = None;
        if !raw && !org.is_empty() && !repo.is_empty() {
            let dir = std::env::temp_dir().join(format!("jjlab-build-repo-{task_id}"));
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::create_dir_all(&dir);
            let (store2, org2, repo2, sha, dir2) =
                (store.clone(), org.clone(), repo.clone(), bookmark.clone(), dir.clone());
            let checkout = tokio::task::spawn_blocking(move || {
                pollster::block_on(jjlab_git::read::checkout_tree(
                    &store2, &org2, &repo2, &sha, &dir2,
                ))
            })
            .await;
            match checkout {
                Ok(Ok(())) => ctx_dir = Some(dir),
                Ok(Err(e)) => {
                    task.log(&format!("checkout error: {e}"));
                    task.finish(false, None, Some(e.to_string()));
                    return;
                }
                Err(e) => {
                    task.log(&format!("checkout join error: {e}"));
                    task.finish(false, None, Some(e.to_string()));
                    return;
                }
            }
        }

        let body_opt = if raw { Some(containerfile.as_str()) } else { None };
        match jjlab_git::build::run_build(
            ctx_dir.as_deref(),
            body_opt,
            image.as_deref(),
            dockerfile.as_deref(),
            &export,
            &build_args,
            no_cache,
            sink,
        )
        .await
        {
            Ok(result) => {
                // Source-repo provenance: when a repo build succeeds, register
                // the produced image as an oci Artifact carrying its owning
                // repo/ref/sha so /pkgs/system/packages?repo=<org/repo> can
                // enumerate the versions a repo published (GitHub-style
                // "linked to repository"). Regression-free: absent meta store
                // or a non-oci export simply skips the record.
                if !org.is_empty() && !repo.is_empty() && export == "push" {
                    if let Some(img) = image.as_deref() {
                        let meta = state.registry.as_ref().map(|r| r.meta.clone());
                        if let Some(meta) = meta {
                            let (org2, repo2, ref2) = (org.clone(), repo.clone(), bookmark.clone());
                            let mut art = pkglab_common::artifact::Artifact {
                                format: "oci".into(),
                                repository: img.to_string(),
                                version: result.clone(),
                                source: "push".into(),
                                repo: format!("{org2}/{repo2}"),
                                ref_: ref2.clone(),
                                ..Default::default()
                            };
                            // Best-effort prove the head sha for this repo.
                            let sha = run_jj(move || {
                                pollster::block_on(jjlab_git::read::head_sha(
                                    &state.store, &org2, &repo2,
                                ))
                            })
                            .await
                            .ok();
                            if let Some(s) = sha {
                                art.sha = s;
                            }
                            let _ = meta.put(art).await;
                        }
                    }
                }
                task.finish(true, Some(result), None)
            }
            Err(e) => task.finish(false, None, Some(e.to_string())),
        }
    });

    Json(json!({ "ok": true, "build_id": response_id })).into_response()
}

/// `POST /ops/helm/install` — install/upgrade a chart.
pub async fn ops_helm_install(
    State(state): State<AppState>,
    Json(body): Json<jjlab_git::helm::HelmInstallRequest>,
) -> Response {
    // The namespace registry still gates the helm target namespace.
    if let Some(ns) = body.namespace.as_deref() {
        if let Err(e) = resolve_ns(&state, Some(ns)) {
            return e;
        }
    }
    let task = state.tasks.create(
        jjlab_git::task::TaskRegistry::new_id(),
        TaskKind::Helm,
    );
    let task_id = task.id.clone();
    let response_id = task_id.clone();

    tokio::spawn(async move {
        let chart = body.chart.clone();
        let chart_path = if chart.is_empty() {
            task.log("helm: chart reference is required");
            task.finish(false, None, Some("chart reference is required".into()));
            return;
        } else {
            chart
        };
        let req = jjlab_git::helm::HelmInstallRequest {
            release_name: body.release_name.clone(),
            chart: chart_path.clone(),
            version: body.version.clone(),
            values: body.values.clone(),
            namespace: body.namespace.clone(),
        };
        match jjlab_git::helm::install_or_upgrade(&req, &chart_path).await {
            Ok(out) => {
                task.log(&format!("helm: {out}"));
                task.finish(true, Some(out), None);
            }
            Err(e) => {
                task.log(&format!("helm error: {e}"));
                task.finish(false, None, Some(e.to_string()));
            }
        }
    });

    Json(json!({ "ok": true, "helm_id": response_id })).into_response()
}

/// `GET /ops/helm/releases`.
pub async fn ops_helm_list(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let ns = match resolve_ns(&state, q.get("namespace").map(String::as_str)) {
        Ok(n) => n,
        Err(e) => return e,
    };
    match jjlab_git::helm::list(&ns).await {
        Ok(releases) => Json(json!({ "releases": releases })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `GET /ops/helm/releases/{name}`.
pub async fn ops_helm_status(
    Path(name): Path<String>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    match jjlab_git::helm::status(&name, q.get("namespace").map(String::as_str)).await {
        Ok(out) => Json(json!({ "status": out })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `DELETE /ops/helm/releases/{name}`.
pub async fn ops_helm_uninstall(
    Path(name): Path<String>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    match jjlab_git::helm::uninstall(&name, q.get("namespace").map(String::as_str)).await {
        Ok(out) => Json(json!({ "ok": true, "output": out })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `POST /ops/helm/releases/{name}/rollback`.
pub async fn ops_helm_rollback(
    Path(name): Path<String>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
    Json(body): Json<RollbackBody>,
) -> Response {
    match jjlab_git::helm::rollback(&name, body.revision, q.get("namespace").map(String::as_str)).await {
        Ok(out) => Json(json!({ "ok": true, "output": out })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `GET /ops/packages` — the in-process registry's package list (proxy-free).
pub async fn ops_packages(State(state): State<AppState>) -> Response {
    match state.registry.as_ref() {
        Some(reg) => {
            let pkgs = reg.meta.list_packages().await;
            match pkgs {
                Ok(p) => Json(json!({ "packages": p })).into_response(),
                Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            }
        }
        None => Json(json!({ "packages": [] })).into_response(),
    }
}

/// `GET /ops/images` — the OCI catalog (repository list).
pub async fn ops_images(State(state): State<AppState>) -> Response {
    match state.registry.as_ref() {
        Some(reg) => {
            let repos = reg.meta.list_repositories_by_format("oci").await;
            match repos {
                Ok(r) => Json(json!({ "repositories": r })).into_response(),
                Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            }
        }
        None => Json(json!({ "repositories": [] })).into_response(),
    }
}
/// `GET /ops/services/{name}/pods` — the pods behind a service.
pub async fn ops_service_pods(
    State(state): State<AppState>,
    Path(name): Path<String>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let ns = match resolve_ns(&state, q.get("namespace").map(String::as_str)) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let client = match k8s_client().await {
        Ok(c) => c,
        Err(e) => return e,
    };
    match jjlab_git::service::service_pods(&client, &name, &ns).await {
        Ok(pods) => Json(json!({ "pods": pods })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `GET /ops/services/{name}/events` — rollout debugging events.
pub async fn ops_service_events(
    State(state): State<AppState>,
    Path(name): Path<String>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let ns = match resolve_ns(&state, q.get("namespace").map(String::as_str)) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let client = match k8s_client().await {
        Ok(c) => c,
        Err(e) => return e,
    };
    match jjlab_git::service::service_events(&client, &name, &ns).await {
        Ok(events) => Json(json!({ "events": events })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `GET /ops/services/{name}/revisions` — ReplicaSet revisions (deployments).
pub async fn ops_service_revisions(
    State(state): State<AppState>,
    Path(name): Path<String>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let ns = match resolve_ns(&state, q.get("namespace").map(String::as_str)) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let client = match k8s_client().await {
        Ok(c) => c,
        Err(e) => return e,
    };
    match jjlab_git::service::service_revisions(&client, &name, &ns).await {
        Ok(revs) => Json(json!({ "revisions": revs })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `POST /ops/services/{name}/rollback` — replay a revision's pod template.
pub async fn ops_service_rollback(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<ServiceRollbackBody>,
) -> Response {
    let ns = match resolve_ns(&state, body.namespace.as_deref()) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let client = match k8s_client().await {
        Ok(c) => c,
        Err(e) => return e,
    };
    match jjlab_git::service::service_rollback(&client, &name, body.revision, &ns).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `GET /ops/helm/releases/{name}/values` — release values.
pub async fn ops_helm_values(
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
    Path(name): Path<String>,
) -> Response {
    match jjlab_git::helm::values(&name, q.get("namespace").map(String::as_str)).await {
        Ok(v) => Json(json!({ "values": v })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
