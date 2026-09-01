//! HTTP facade (REST). Git-aligned read surface (commit-addressed) plus
//! jj-native change/conflict reads, and mirror sync (clone/fetch/push).
//!
//! Paths follow Gitea's `/repos/{org}/{repo}` shape. Commits are commit-
//! addressed; change-id is an extra (jj-native) addressing scheme, never a
//! replacement for commit sha.

pub mod handlers;
pub mod registry;

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use jjlab_core::Db;

/// In-process package registry substrate (pkglab). `None` when the registry is
/// disabled (e.g. unit tests that only exercise the git surface).
pub type RegistryHandle = Option<Arc<pkglab_common::Registry>>;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub store: Arc<jjlab_git::RepoStore>,
    pub tokens: Arc<Vec<(String, Level)>>,
    pub assets: Arc<pkglab_core::blob::FsBlobStore>,
    pub registry: RegistryHandle,
    pub namespaces: Arc<jjlab_git::namespaces::NamespaceRegistry>,
    pub tasks: Arc<jjlab_git::task::TaskRegistry>,
    pub sync_cache: Arc<crate::handlers::sync::SyncCache>,
}

impl AppState {
    pub fn new(
        db: Arc<Db>,
        store: Arc<jjlab_git::RepoStore>,
        tokens: Vec<(String, Level)>,
        assets: Arc<pkglab_core::blob::FsBlobStore>,
    ) -> Self {
        Self {
            db,
            store,
            tokens: Arc::new(tokens),
            assets,
            registry: None,
            namespaces: Arc::new(jjlab_git::namespaces::NamespaceRegistry::from_env()),
            tasks: Arc::new(jjlab_git::task::TaskRegistry::new()),
            sync_cache: Arc::new(crate::handlers::sync::SyncCache::new()),
        }
    }

    pub fn with_namespaces(mut self, namespaces: Vec<String>) -> Self {
        self.namespaces = Arc::new(jjlab_git::namespaces::NamespaceRegistry::new(namespaces));
        self
    }

    pub fn with_registry(mut self, registry: Arc<pkglab_common::Registry>) -> Self {
        self.registry = Some(registry);
        self
    }
}

#[derive(Deserialize)]
pub struct SyncBody {
    url: String,
    #[serde(default)]
    remote: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    secret: String,
}

#[derive(Deserialize)]
pub struct CreateRepoBody {
    #[serde(default = "default_branch")]
    default_branch: String,
}

pub 
fn default_branch() -> String {
    "main".to_string()
}

#[derive(Deserialize)]
pub struct RenameRepoBody {
    #[serde(rename = "new_name")]
    name: String,
}

#[derive(Deserialize)]
pub struct BranchBody {
    /// Snapshot to point the branch at: commit-sha / bookmark / tag.
    #[serde(default)]
    target: String,
    /// Alternative: a change-id (resolved to its current commit).
    #[serde(default)]
    change: String,
}

#[derive(Deserialize)]
pub struct TagBody {
    /// Snapshot to point the tag at: commit-sha / bookmark / tag.
    #[serde(default)]
    target: String,
    /// Alternative: a change-id (resolved to its current commit).
    #[serde(default)]
    change: String,
}

#[derive(Deserialize)]
pub struct FileBody {
    /// Base64-encoded content (Gitea-compatible).
    #[serde(default)]
    content_base64: String,
    /// Plain-text content shortcut.
    #[serde(default)]
    content: String,
    #[serde(default = "default_branch")]
    branch: String,
    #[serde(default)]
    message: String,
    /// Rewrite the branch head's change (stable change-id) instead of creating
    /// a fresh change. Defaults to `true` (jj-native amend semantics).
    #[serde(default = "default_amend")]
    amend: bool,
}

pub 
fn default_amend() -> bool {
    true
}

pub 
fn file_content(body: &FileBody) -> Result<Vec<u8>, String> {
    if !body.content_base64.is_empty() {
        use base64::Engine as _;
        return base64::engine::general_purpose::STANDARD
            .decode(&body.content_base64)
            .map_err(|e| format!("bad base64: {e}"));
    }
    Ok(body.content.clone().into_bytes())
}

#[derive(Deserialize)]
pub struct CreateMrBody {
    title: String,
    #[serde(default)]
    body: String,
    head: String,
    #[serde(default = "default_branch")]
    base: String,
}

#[derive(Deserialize)]
pub struct UpdateMrBody {
    /// close | reopen
    state: String,
}

#[derive(Deserialize)]
pub struct UpdateMrHeadBody {
    /// the MR's new head rev (force-push re-association + CI re-trigger)
    head: String,
}

#[derive(Deserialize)]
pub struct ReviewBody {
    /// approved | request_changes | comment
    state: String,
    #[serde(default)]
    body: String,
}

#[derive(Deserialize)]
pub struct CommentBody {
    body: String,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Deserialize)]
pub struct ReleaseBody {
    tag_name: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

// ── /ops: orchestration primitives ──

/// One-shot run request (a debug pod that runs to completion and dies).
#[derive(Deserialize)]
pub struct RunBody {
    pub image: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default = "default_workdir")]
    pub workdir: String,
    #[serde(default)]
    pub cpu: Option<String>,
    #[serde(default)]
    pub memory: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub namespace: Option<String>,
}

fn default_workdir() -> String {
    "/workspace".to_string()
}

/// Register a runtime namespace (write-token gated).
#[derive(Deserialize)]
pub struct NamespaceBody {
    pub namespace: String,
}

/// Scale a deployment (replicas target).
#[derive(Deserialize)]
pub struct ScaleBody {
    pub replicas: i32,
    #[serde(default)]
    pub namespace: Option<String>,
}

/// Rebase one snapshot onto another, advancing the dest bookmark.
#[derive(Deserialize)]
pub struct RebaseBody {
    pub source: String,
    pub dest: String,
}

/// Sync body: which repo snapshot to push into a service's worker.
#[derive(Deserialize)]
pub struct ServiceSyncBody {
    pub org: String,
    pub repo: String,
    pub rev: String,
    #[serde(default)]
    pub namespace: Option<String>,
}

/// Rollback a helm release to a revision.
#[derive(Deserialize)]
pub struct RollbackBody {
    #[serde(default)]
    pub revision: Option<u32>,
    #[serde(default)]
    pub namespace: Option<String>,
}

/// Rollback a deployment to a ReplicaSet revision (0 = previous).
#[derive(Deserialize)]
pub struct ServiceRollbackBody {
    #[serde(default)]
    pub revision: i64,
    #[serde(default)]
    pub namespace: Option<String>,
}

// ── auth ──

/// Static-token auth, aligned with Gitea's `Authorization: token <pat>` scheme.
/// Tokens are configured via `JJLAB_TOKENS` as `token=level` pairs joined by
/// `,` (levels: read, write). Anonymous = read-only (clone may be anonymous,
/// push must present a write token).
#[derive(Clone)]
pub struct Auth {
    pub level: Level,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Anonymous,
    Read,
    Write,
}

impl Level {
    fn parse(s: &str) -> Option<Level> {
        match s {
            "read" => Some(Level::Read),
            "write" => Some(Level::Write),
            _ => None,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Level::Anonymous => "anonymous",
            Level::Read => "read",
            Level::Write => "write",
        }
    }
}

pub 
fn parse_tokens(spec: &str) -> Vec<(String, Level)> {
    spec.split(',')
        .filter_map(|pair| {
            let (token, level) = pair.split_once('=')?;
            let token = token.trim().to_string();
            if token.is_empty() {
                return None;
            }
            Level::parse(level.trim()).map(|l| (token, l))
        })
        .collect()
}

impl Auth {
    fn from_request(state: &AppState, headers: &axum::http::HeaderMap) -> Auth {
        let raw = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let token = raw
            .strip_prefix("token ")
            .or_else(|| raw.strip_prefix("Token "))
            .or_else(|| raw.strip_prefix("Bearer "))
            .map(str::trim);
        if let Some(token) = token {
            for (t, level) in state.tokens.iter() {
                if t == token {
                    return Auth { level: *level };
                }
            }
        }
        // git smart-http pushes credentials via basic auth `user:token`.
        if let Some(raw_basic) = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Basic "))
        {
            use base64::Engine as _;
            if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(raw_basic) {
                if let Ok(text) = String::from_utf8(decoded) {
                    if let Some((_, secret)) = text.split_once(':') {
                        for (t, level) in state.tokens.iter() {
                            if t == secret {
                                return Auth { level: *level };
                            }
                        }
                    }
                }
            }
        }
        Auth { level: Level::Anonymous }
    }
}

/// Gitea-style error: `{"message": "...", "url": "https://docs..."}`.
pub 
fn gitea_err(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({
            "message": message.into(),
            "url": "https://docs.jjlab.dev/api",
        })),
    )
        .into_response()
}

pub 
fn json_err(status: StatusCode, msg: String) -> Response {
    gitea_err(status, msg)
}

/// Map a domain error to its HTTP status code. `NotFound`/`Invalid`/
/// `Conflict` carry semantic statuses; everything else is an internal error.
pub(crate) fn error_status(err: &jjlab_core::Error) -> StatusCode {
    match err {
        jjlab_core::Error::NotFound(_) => StatusCode::NOT_FOUND,
        jjlab_core::Error::Invalid(_) => StatusCode::BAD_REQUEST,
        jjlab_core::Error::Conflict(_) => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Map a git-layer error to its HTTP status code. Mirrors [`error_status`] but
/// for [`jjlab_git::repo::RepoError`].
pub(crate) fn repo_error_status(err: &jjlab_git::repo::RepoError) -> StatusCode {
    match err {
        jjlab_git::repo::RepoError::NotFound { .. } => StatusCode::NOT_FOUND,
        jjlab_git::repo::RepoError::Invalid(_) => StatusCode::BAD_REQUEST,
        jjlab_git::repo::RepoError::Conflict(_) => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Run a blocking git/jj operation off the reactor and fold its error into a
/// ready-made response. Callers pass a `move ||` closure that owns its data and
/// uses `pollster::block_on` inside (jj's `ReadonlyRepo` is `!Send`).
pub(crate) async fn run_jj<T, F>(f: F) -> Result<T, Response>
where
    T: Send + 'static,
    F: FnOnce() -> jjlab_git::repo::RepoResult<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(t)) => Ok(t),
        Ok(Err(e)) => Err(json_err(repo_error_status(&e), e.to_string())),
        Err(e) => Err(json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

/// Run a synchronous DB closure on the blocking pool and fold its
/// [`jjlab_core::Error`] into a ready-made response (see [`Db::run`]).
pub(crate) async fn db_run<T, F>(db: &Arc<Db>, f: F) -> Result<T, Response>
where
    T: Send + 'static,
    F: FnOnce(&Db) -> jjlab_core::Result<T> + Send + 'static,
{
    match db.run(f).await {
        Ok(t) => Ok(t),
        Err(e) => Err(json_err(error_status(&e), e.to_string())),
    }
}

/// Extract auth; enforce minimum level.
#[allow(clippy::result_large_err)]
pub 
fn require_auth(state: &AppState, headers: &axum::http::HeaderMap, min: Level) -> Result<Auth, Response> {
    let auth = Auth::from_request(state, headers);
    if auth.level >= min {
        Ok(auth)
    } else if auth.level == Level::Anonymous {
        let mut resp = gitea_err(
            StatusCode::UNAUTHORIZED,
            "authentication required: supply `Authorization: token <pat>`",
        );
        resp.headers_mut().insert(
            axum::http::header::WWW_AUTHENTICATE,
            axum::http::HeaderValue::from_static("Basic realm=\"jjlab\""),
        );
        Err(resp)
    } else {
        Err(gitea_err(
            StatusCode::FORBIDDEN,
            format!(
                "insufficient permissions: need {}, have {}",
                min.as_str(),
                auth.level.as_str()
            ),
        ))
    }
}

/// Require a write token on every mutating REST route (`/api/*`). Git
/// smart-HTTP transport (not under `/api`) enforces its own auth in
/// `smart_http_rpc` for receive-pack; anonymous clone/fetch must stay open.
async fn require_write_reset(
    State(state): State<AppState>,
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Response {
    let method = req.method().clone();
    let is_mutating = matches!(
        method,
        axum::http::Method::POST
            | axum::http::Method::PUT
            | axum::http::Method::PATCH
            | axum::http::Method::DELETE
    );
    if is_mutating && req.uri().path().starts_with("/api/") {
        if let Err(resp) = require_auth(&state, req.headers(), Level::Write) {
            return resp;
        }
    }
    next.run(req).await
}

pub async fn health() -> Json<Value> {
    Json(json!({ "ok": true, "name": "jjlab" }))
}

pub(crate) fn server_author() -> (String, String) {
    jjlab_git::settings::author_identity()
}

pub fn build_router(state: AppState) -> axum::Router {
    use crate::handlers::*;
use axum::routing::put as put_route;
    let mut router = Router::new()
            .route("/api/v1/health", get(health))
            .route("/api/v1/repos", get(list_orgs))
            .route("/api/v1/repos/{org}/{repo}/graph", get(graph_handler))
            .route("/api/v1/repos/{org}/{repo}/file-log", get(file_log_handler))
            .route("/api/v1/repos/{org}/{repo}/search", get(search_handler))
            // Git-aligned read surface (commit-addressed).
            .route("/api/v1/repos/{org}/{repo}/commits/{sha}", get(commit_info))
            .route("/api/v1/repos/{org}/{repo}/commits/{sha}/diff", get(commit_diff))
            .route("/api/v1/repos/{org}/{repo}/branches", get(list_branches))
            .route("/api/v1/repos/{org}/{repo}/blob", get(raw_file))
            .route("/api/v1/repos/{org}/{repo}/annotate/{*path}", get(annotate_file))
            .route("/api/v1/repos/{org}/{repo}/tree/{sha}", get(tree_at_sha))
            // jj-native: change list anchored to a snapshot rev (like commits/files).
            .route("/api/v1/repos/{org}/{repo}/changes", get(list_changes))
            // Metadata (jj-native) — conflicts + bookmarks.
            .route("/api/v1/repos/{org}/{repo}/conflicts", get(list_conflicts))
            .route("/api/v1/repos/{org}/{repo}/bookmarks", get(list_bookmarks))
            // Mirror sync (pair: pull-mirror fetches+prunes, push-mirror pushes
            // all refs; clone bootstraps a fresh repo from a remote).
            .route("/api/v1/repos/{org}/{repo}/clone", post(clone_remote))
            .route("/api/v1/repos/{org}/{repo}/pull-mirror", post(pull_mirror))
            .route("/api/v1/repos/{org}/{repo}/push-mirror", post(push_mirror))
            // Write surface (D).
            .route("/api/v1/repos/{org}/{repo}", post(create_repo).delete(delete_repo).patch(rename_repo))
            .route(
                "/api/v1/repos/{org}/{repo}/branches/{name}",
                post(set_branch_handler).delete(delete_branch_handler),
            )
            .route(
                "/api/v1/repos/{org}/{repo}/tags/{name}",
                post(set_tag_handler).delete(delete_tag_handler),
            )
            .route(
                "/api/v1/repos/{org}/{repo}/contents/{*path}",
                post(create_file).put(update_file).delete(delete_file_handler),
            )
            .route("/api/v1/repos/{org}/{repo}/rebase", post(rebase_handler))
            // Extended read surface (E), Gitea-aligned.
            .route("/api/v1/repos/{org}/{repo}/commits", get(commit_log_handler))
            .route(
                "/api/v1/repos/{org}/{repo}/tags",
                get(tags_handler),
            )
            .route("/api/v1/repos/{org}/{repo}/refs", get(refs_handler))
            .route(
                "/api/v1/repos/{org}/{repo}/contents",
                get(contents_list_handler),
            )
            .route(
                "/api/v1/repos/{org}/{repo}/contents/{*path}",
                get(contents_handler),
            )
            .route(
                "/api/v1/repos/{org}/{repo}/compare",
                get(compare_handler),
            )
            .route(
                "/api/v1/repos/{org}/{repo}/archive/{ball_type}/{sha}",
                get(archive_handler),
            )
            // Merge requests (M3).
            .route("/api/v1/repos/{org}/{repo}/pulls", get(list_mrs).post(create_mr_handler))
            .route(
                "/api/v1/repos/{org}/{repo}/pulls/{number}",
                get(get_mr_handler).patch(update_mr_handler),
            )
            .route(
                "/api/v1/repos/{org}/{repo}/pulls/{number}/head",
                put_route(update_mr_head_handler),
            )
            .route(
                "/api/v1/repos/{org}/{repo}/pulls/{number}/reviews",
                get(list_reviews).post(add_review),
            )
            .route(
                "/api/v1/repos/{org}/{repo}/pulls/{number}/comments",
                get(list_comments).post(add_comment),
            )
            .route(
                "/api/v1/repos/{org}/{repo}/pulls/{number}/diff",
                get(mr_diff),
            )
            // Releases (M5-A).
            .route("/api/v1/repos/{org}/{repo}/releases", get(list_releases).post(create_release))
            .route(
                "/api/v1/repos/{org}/{repo}/releases/latest",
                get(latest_release),
            )
            .route(
                "/api/v1/repos/{org}/{repo}/releases/{tag}",
                get(get_release).delete(delete_release),
            )
            .route(
                "/api/v1/repos/{org}/{repo}/releases/{tag}/assets",
                post(upload_asset),
            )
            .route(
                "/api/v1/repos/{org}/{repo}/releases/{tag}/assets/{name}",
                get(download_asset).delete(delete_asset),
            )
            // Actions/CI (M6-C1).
            .route("/api/v1/repos/{org}/{repo}/actions/workflows", get(list_workflows))
            .route(
                "/api/v1/repos/{org}/{repo}/actions/workflows/{id}/dispatch",
                post(dispatch_workflow),
            )
            .route("/api/v1/repos/{org}/{repo}/actions/runs", get(list_runs))
            .route(
                "/api/v1/repos/{org}/{repo}/actions/runs/{run_id}/jobs",
                get(list_run_jobs),
            )
            .route(
                "/api/v1/repos/{org}/{repo}/actions/jobs/{job_id}/logs",
                get(job_logs),
            )
            .route(
                "/api/v1/repos/{org}/{repo}/actions/jobs/{job_id}/logs/stream",
                get(job_logs_stream),
            )
            // Orchestration primitives (/ops): purpose-neutral run/service/
            // build/helm/namespace operations consumed by the ops-extension.
            .route("/api/v1/ops/config", get(ops_config))
            .route("/api/v1/ops/namespaces", get(ops_namespaces).put(register_namespace))
            .route("/api/v1/ops/runs", post(ops_run))
            .route("/api/v1/ops/services", get(ops_services_list).post(ops_service_create))
            .route(
                "/api/v1/ops/services/{name}",
                get(ops_service_get)
                    .delete(ops_service_delete)
                    .post(ops_service_restart),
            )
            .route("/api/v1/ops/services/{name}/scale", post(ops_service_scale))
            .route("/api/v1/ops/services/{name}/sync", post(crate::handlers::sync::ops_service_sync))
            .route("/api/v1/ops/services/{name}/pods", get(ops_service_pods))
            .route("/api/v1/ops/services/{name}/events", get(ops_service_events))
            .route("/api/v1/ops/services/{name}/revisions", get(ops_service_revisions))
            .route("/api/v1/ops/services/{name}/rollback", post(ops_service_rollback))
            .route("/api/v1/ops/helm/install", post(ops_helm_install))
            .route("/api/v1/ops/helm/releases", get(ops_helm_list))
            .route(
                "/api/v1/ops/helm/releases/{name}",
                get(ops_helm_status).delete(ops_helm_uninstall),
            )
            .route("/api/v1/ops/helm/releases/{name}/rollback", post(ops_helm_rollback))
            .route("/api/v1/ops/helm/releases/{name}/values", get(ops_helm_values))
            .route("/api/v1/ops/builds", post(ops_build))
            .route("/api/v1/ops/packages", get(ops_packages))
            .route("/api/v1/ops/images", get(ops_images))
            .route("/api/v1/ops/tasks", get(ops_tasks_list))
            .route("/api/v1/ops/tasks/{id}", get(ops_task_get))
            .route("/api/v1/ops/tasks/{id}/stream", get(ops_task_stream))
            // Inbound Git Smart HTTP (ADR-009): git clone/push straight to jjlab.
            // axum allows one param per segment, so catch the remainder and parse:
            //   /{org}/{repo}.git/info/refs
            //   /{org}/{repo}.git/git-upload-pack | git-receive-pack
            .route("/{org}/{*git_repo_path}", get(smart_http_get).post(smart_http_post))
            .route("/assets/{*path}", get(spa))
            .route("/static/{*path}", get(spa))
            .with_state(state.clone())
            .layer(axum::middleware::from_fn_with_state(state.clone(), require_write_reset))
            .layer(axum::extract::DefaultBodyLimit::max(512 * 1024 * 1024))
            .fallback(spa);

    // Nest the in-process package registry (OCI + protocol routers). The
    // registry surface is more specific than the catch-all git route and the
    // SPA fallback. Use `nest_service` (not `nest`) so the adapter's explicit
    // fallback is not registered as a competing route and cannot panic when
    // the main router later adds its own `fallback(spa)`.
    if let Some(reg) = state.registry.as_ref() {
        if let Some(mounts) = registry::assemble(reg, &state) {
            for (prefix, sub) in mounts {
                router = router.nest_service(prefix, sub);
            }
        }
    }
    router
}

