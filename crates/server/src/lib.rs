//! HTTP facade (REST). Git-aligned read surface (commit-addressed) plus
//! jj-native change/conflict/op-log reads, and mirror sync (clone/fetch/push).
//!
//! Paths follow Gitea's `/repos/{org}/{repo}` shape. Commits are commit-
//! addressed; change-id is an extra (jj-native) addressing scheme, never a
//! replacement for commit sha.

pub mod registry;

use std::sync::Arc;

use axum::extract::{Path, State};
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
    pub assets: Arc<jjlab_git::assets::AssetStore>,
    pub registry: RegistryHandle,
}

impl AppState {
    pub fn new(
        db: Arc<Db>,
        store: Arc<jjlab_git::RepoStore>,
        tokens: Vec<(String, Level)>,
        assets: Arc<jjlab_git::assets::AssetStore>,
    ) -> Self {
        Self { db, store, tokens: Arc::new(tokens), assets, registry: None }
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
    /// Commit sha / change-id / bookmark to point the branch at.
    target: String,
}

#[derive(Deserialize)]
pub struct TagBody {
    target: String,
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

// ── org/repo listing (Explore) ──

struct OrgRepoEntry {
    name: String,
    default_bookmark: String,
}

pub async fn list_orgs(State(state): State<AppState>) -> Response {
    let Ok(orgs) = state.db.list_orgs() else {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "list orgs".into());
    };
    let mut items: Vec<Value> = Vec::new();
    for (id, name) in orgs {
        let repos = match state.db.list_repos() {
            Ok(all) => all
                .into_iter()
                .filter(|r| r.org_id == id)
                .map(|r| OrgRepoEntry { name: r.name, default_bookmark: r.default_bookmark })
                .collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        };
        items.push(json!({
            "org": name,
            "repos": repos.into_iter().map(|r| json!({
                "repo": r.name,
                "default_bookmark": r.default_bookmark,
            })).collect::<Vec<_>>(),
        }));
    }
    Json(json!({ "orgs": items })).into_response()
}

// ── graph / history / search reads ──

pub async fn graph_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let limit: usize = q.get("limit").and_then(|v| v.parse().ok()).unwrap_or(100);
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::change_graph(&store, &org, &repo, limit))
    })
    .await;
    match r {
        Ok(Ok(nodes)) => Json(json!({ "graph": nodes })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn file_log_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(path) = q.get("path").cloned() else {
        return json_err(StatusCode::BAD_REQUEST, "path required".into());
    };
    let limit: usize = q.get("limit").and_then(|v| v.parse().ok()).unwrap_or(50);
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::file_log(&store, &org, &repo, &path, limit))
    })
    .await;
    match r {
        Ok(Ok((items, total))) => Json(json!({ "total_count": total, "commits": items })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn search_handler(
    State(state): State<AppState>,
    Path((org, repo, _rev)): Path<(String, String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let rev = q.get("ref").cloned().unwrap_or_default();
    let Some(pattern) = q.get("pattern").cloned().filter(|p| !p.is_empty()) else {
        return json_err(StatusCode::BAD_REQUEST, "pattern required".into());
    };
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::search_code(&store, &org, &repo, &rev, &pattern))
    })
    .await;
    match r {
        Ok(Ok(matches)) => Json(json!({ "matches": matches })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── static SPA ──

/// Serve the embedded frontend. `/` and non-asset paths fall back to
/// index.html (client-side hash routing); asset paths resolve within `dist`.
async fn spa(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/').to_string();
    let candidate = if path.is_empty() || !path.contains('.') {
        "index.html".to_string()
    } else {
        path.clone()
    };
    const DIST: include_dir::Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../dist");
    if let Some(file) = DIST.get_file(candidate.as_str()) {
        let ext = candidate.rsplit('.').next().unwrap_or("");
        let ct = match ext {
            "html" => "text/html",
            "js" => "text/javascript",
            "mjs" => "text/javascript",
            "css" => "text/css",
            "json" => "application/json",
            "svg" => "image/svg+xml",
            "png" => "image/png",
            "ico" => "image/x-icon",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            "txt" => "text/plain",
            "map" => "application/json",
            _ => "application/octet-stream",
        };
        return (
            [(axum::http::header::CONTENT_TYPE, ct)],
            file.contents(),
        )
            .into_response();
    }
    if let Some(file) = DIST.get_file("index.html") {
        return (
            [(axum::http::header::CONTENT_TYPE, "text/html")],
            file.contents(),
        )
            .into_response();
    }
    (StatusCode::NOT_FOUND, "not found").into_response()
}

// ── git-aligned read: commit info (commit sha addressing) ──

pub async fn commit_info(
    State(state): State<AppState>,
    Path((org, repo, sha)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::commit_by_sha(&store, &org, &repo, &sha))
    })
    .await;
    match r {
        Ok(Ok(info)) => Json(json!(info)).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn list_branches(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::branches(&store, &org, &repo))
    })
    .await;
    match r {
        Ok(Ok(branches)) => Json(json!({ "branches": branches })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn raw_file(
    State(state): State<AppState>,
    Path((org, repo, path)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::raw_at_head(&store, &org, &repo, &path))
    })
    .await;
    match r {
        Ok(Ok(bytes)) => bytes.into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn tree_at_sha(
    State(state): State<AppState>,
    Path((org, repo, sha)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::tree_at_sha(&store, &org, &repo, &sha))
    })
    .await;
    match r {
        Ok(Ok(entries)) => Json(json!({ "tree": entries })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── jj-native: change addressed read ──

pub async fn change_info(
    State(state): State<AppState>,
    Path((org, repo, change_id)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::change_info(
            &store,
            &org,
            &repo,
            &change_id,
        ))
    })
    .await;
    match r {
        Ok(Ok(info)) => Json(json!(info)).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── metadata + op-log ──

pub async fn list_op_log(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    match state.db.list_op_log(&repo_id) {
        Ok(rows) => {
            let ops: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    json!({
                        "id": r.id,
                        "repo_id": r.repo_id,
                        "op_type": r.op_type,
                        "payload": r.payload,
                        "undo_of": r.undo_of,
                    })
                })
                .collect();
            Json(json!({ "ops": ops })).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn list_conflicts(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    match state.db.list_conflicts(&repo_id) {
        Ok(rows) => {
            let conflicts: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    json!({
                        "id": r.id,
                        "repo_id": r.repo_id,
                        "change_id": r.change_id,
                        "path": r.path,
                        "adds": serde_json::from_str::<Value>(&r.adds).unwrap_or(Value::Null),
                        "removes": serde_json::from_str::<Value>(&r.removes).unwrap_or(Value::Null),
                    })
                })
                .collect();
            Json(json!({ "conflicts": conflicts })).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn list_bookmarks(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    match state.db.list_bookmarks(&repo_id) {
        Ok(rows) => {
            let bookmarks: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    json!({
                        "name": r.name,
                        "change_id": r.change_id,
                        "is_remote": r.is_remote,
                    })
                })
                .collect();
            Json(json!({ "bookmarks": bookmarks })).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn list_change_ids(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let store = state.store.clone();
    match tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::sync::list_change_ids(&store, &org, &repo))
    })
    .await
    {
        Ok(Ok(ids)) => Json(json!({ "change_ids": ids })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── mirror sync ──

pub async fn clone_remote(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<SyncBody>,
) -> Response {
    let url = body.url.clone();
    let branch = body.branch.clone();
    let store = state.store.clone();
    let db = state.db.clone();
    match tokio::task::spawn_blocking(move || {
        (|| {
            pollster::block_on(jjlab_git::sync::clone_remote(
                &store,
                &db,
                &org,
                &repo,
                &url,
                branch.as_deref(),
            ))?;
            pollster::block_on(jjlab_git::project::project_repo(&store, &db, &org, &repo))
        })()
    })
    .await
    {
        Ok(Ok(head)) => Json(json!({ "ok": true, "head": head })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn fetch_remote(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<SyncBody>,
) -> Response {
    let remote = body.remote.clone().unwrap_or_else(|| "origin".to_string());
    let url = body.url.clone();
    let store = state.store.clone();
    let db = state.db.clone();
    match tokio::task::spawn_blocking(move || {
        (|| {
            let updated =
                pollster::block_on(jjlab_git::sync::fetch_remote(&store, &org, &repo, &remote, &url))?;
            pollster::block_on(jjlab_git::project::project_repo(&store, &db, &org, &repo))?;
            Ok::<usize, jjlab_git::repo::RepoError>(updated)
        })()
    })
    .await
    {
        Ok(Ok(updated)) => Json(json!({ "ok": true, "updated_bookmarks": updated })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn push_mirror(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<SyncBody>,
) -> Response {
    let url = body.url.clone();
    let secret = body.secret.clone();
    let store = state.store.clone();
    let db = state.db.clone();
    match tokio::task::spawn_blocking(move || {
        (|| {
            pollster::block_on(jjlab_git::sync::push_mirror(&store, &org, &repo, &url, &secret))?;
            pollster::block_on(jjlab_git::project::project_repo(&store, &db, &org, &repo))
        })()
    })
    .await
    {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── inbound Git Smart HTTP (Gitea-equivalent transport) ──

fn smart_http_error(status: StatusCode, msg: String) -> Response {
    (status, [("content-type", "text/plain")], msg).into_response()
}

pub async fn smart_http_info_refs(
    state: State<AppState>,
    org: String,
    repo: String,
    headers: axum::http::HeaderMap,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> Response {
    let Some(query) = query else {
        return smart_http_error(StatusCode::BAD_REQUEST, "missing service param".into());
    };
    let service_param = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("service="))
        .map(|s| s.replace("%20", " "));
    let Some(service) = jjlab_git::http::GitService::from_service_param(service_param.as_deref())
    else {
        return smart_http_error(StatusCode::BAD_REQUEST, "unknown service".into());
    };

    // Push advertisement requires write token (git sends basic auth).
    if service == jjlab_git::http::GitService::ReceivePack {
        if let Err(resp) = require_auth(&state, &headers, Level::Write) {
            return resp;
        }
    }

    let git_dir = match jjlab_git::http::git_dir_of(&state.store, &org, &repo) {
        Ok(d) => d,
        Err(e) => return smart_http_error(StatusCode::NOT_FOUND, e.to_string()),
    };
    match jjlab_git::http::advertise_refs(service, &git_dir).await {
        Ok(pkt) => (
            StatusCode::OK,
            [
                ("content-type", service.content_type("advertisement").as_str()),
                ("cache-control", "no-cache"),
            ],
            pkt,
        )
            .into_response(),
        Err(e) => smart_http_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn smart_http_rpc(
    state: State<AppState>,
    org: String,
    repo: String,
    svc: String,
    headers: axum::http::HeaderMap,
    body: axum::body::Body,
) -> Response {
    let service = match svc.as_str() {
        "git-upload-pack" => jjlab_git::http::GitService::UploadPack,
        "git-receive-pack" => jjlab_git::http::GitService::ReceivePack,
        _ => return smart_http_error(StatusCode::NOT_FOUND, format!("unknown service {svc}")),
    };

    if service == jjlab_git::http::GitService::ReceivePack {
        if let Err(resp) = require_auth(&state, &headers, Level::Write) {
            return resp;
        }
    }

    let git_dir = match jjlab_git::http::git_dir_of(&state.store, &org, &repo) {
        Ok(d) => d,
        Err(e) => return smart_http_error(StatusCode::NOT_FOUND, e.to_string()),
    };

    let req_headers = headers.clone();
    let content_type = service.content_type("request");
    if !req_headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v == content_type)
        .unwrap_or(false)
    {
        return smart_http_error(
            StatusCode::BAD_REQUEST,
            format!("content-type must be {content_type}"),
        );
    }

    let body_bytes = match axum::body::to_bytes(body, 512 * 1024 * 1024).await {
        Ok(b) => b.to_vec(),
        Err(e) => return smart_http_error(StatusCode::BAD_REQUEST, format!("read body: {e}")),
    };
    match jjlab_git::http::run_rpc(service, &git_dir, body_bytes).await {
        Ok((out, _err, ok)) => {
            if service == jjlab_git::http::GitService::ReceivePack && ok {
                // Received a pack: import refs so the pushed commits become
                // native jj changes (change-id header preserved), then project
                // the view into SQLite.
                let store = state.store.clone();
                let db = state.db.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    pollster::block_on(jjlab_git::sync::import_after_receive(&store, &db, &org, &repo))
                })
                .await;
            }
            (
                StatusCode::OK,
                [
                    ("content-type", service.content_type("result").as_str()),
                    ("cache-control", "no-cache"),
                ],
                out,
            )
                .into_response()
        }
        Err(e) => smart_http_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn smart_http_get(
    state: State<AppState>,
    Path((org, git_repo_path)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    query: axum::extract::RawQuery,
) -> Response {
    let Some((rest, tail)) = git_repo_path.split_once('/') else {
        return smart_http_error(StatusCode::NOT_FOUND, "not a git endpoint".into());
    };
    if tail != "info/refs" {
        return smart_http_error(StatusCode::NOT_FOUND, "not a git endpoint".into());
    }
    let Some(repo) = rest.strip_suffix(".git") else {
        return smart_http_error(StatusCode::NOT_FOUND, "not a git endpoint".into());
    };
    smart_http_info_refs(state, org, repo.to_string(), headers, query).await
}

pub async fn smart_http_post(
    state: State<AppState>,
    Path((org, git_repo_path)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    body: axum::body::Body,
) -> Response {
    let Some((rest, tail)) = git_repo_path.split_once('/') else {
        return smart_http_error(StatusCode::NOT_FOUND, "not a git endpoint".into());
    };
    let Some(repo) = rest.strip_suffix(".git") else {
        return smart_http_error(StatusCode::NOT_FOUND, "not a git endpoint".into());
    };
    let svc = match tail {
        t if t == "git-upload-pack" || t == "upload-pack" => "git-upload-pack".to_string(),
        t if t == "git-receive-pack" || t == "receive-pack" => "git-receive-pack".to_string(),
        _ => return smart_http_error(StatusCode::NOT_FOUND, "not a git endpoint".into()),
    };
    smart_http_rpc(state, org, repo.to_string(), svc, headers, body).await
}


// ── write surface (D) ──

pub 
const SERVER_AUTHOR: (&str, &str) = ("zergx", "zergx@dev");

pub async fn create_repo(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<CreateRepoBody>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    let author = (SERVER_AUTHOR.0.to_string(), SERVER_AUTHOR.1.to_string());
    let (o2, r2) = (org.clone(), repo.clone());
    match tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::mutation::create_repo(
            &store, &db, &org, &repo, &body.default_branch, author,
        ))
    })
    .await
    {
        Ok(Ok(())) => (
            StatusCode::CREATED,
            Json(json!({ "full_name": format!("{o2}/{r2}") })),
        )
            .into_response(),
        Ok(Err(e)) => json_err(StatusCode::CONFLICT, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn delete_repo(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    let repo_id = format!("{org}/{repo}");
    match tokio::task::spawn_blocking(move || {
        // Remove the on-disk store first; treat "not found" as already-gone
        // so that a re-DELETE also cleans any orphaned DB rows.
        match pollster::block_on(jjlab_git::mutation::delete_repo(&store, &org, &repo)) {
            Ok(()) => {}
            Err(jjlab_git::repo::RepoError::NotFound { .. }) => {}
            Err(e) => return Err(e),
        }
        db.delete_repo(&repo_id)
            .map_err(|e| jjlab_git::repo::RepoError::Other(e.to_string()))
    })
    .await
    {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn rename_repo(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<RenameRepoBody>,
) -> Response {
    let new_name = body.name;
    if let Err(e) = jjlab_core::validate_segment(&new_name, "repo") {
        return json_err(StatusCode::BAD_REQUEST, e);
    }
    let old_id = format!("{org}/{repo}");
    let new_id = format!("{org}/{new_name}");

    // DB rename first (single UPDATE, cascades repo_id everywhere).
    let outcome = state.db.rename_repo(&old_id, &new_id);
    let outcome = match outcome {
        Ok(o) => o,
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    match outcome {
        jjlab_core::db::RenameOutcome::NotFound => {
            return json_err(StatusCode::NOT_FOUND, format!("{old_id} not found"));
        }
        jjlab_core::db::RenameOutcome::Conflict => {
            return json_err(StatusCode::CONFLICT, format!("{new_id} already exists"));
        }
        jjlab_core::db::RenameOutcome::Ok => {}
    }

    let old_dir = state.store.repo_dir_checked(&org, &repo);
    let new_dir = state.store.repo_dir_checked(&org, &new_name);
    let (old_dir, new_dir) = match (old_dir, new_dir) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            let _ = state.db.rename_repo(&new_id, &old_id);
            return json_err(StatusCode::BAD_REQUEST, e.to_string());
        }
    };
    match tokio::fs::rename(&old_dir, &new_dir).await {
        Ok(()) => Json(json!({ "full_name": new_id })).into_response(),
        Err(e) => {
            // Roll back the DB rename so metadata stays consistent.
            let _ = state.db.rename_repo(&new_id, &old_id);
            json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("rename: {e}"))
        }
    }
}

pub async fn set_branch_handler(
    State(state): State<AppState>,
    Path((org, repo, name)): Path<(String, String, String)>,
    Json(body): Json<BranchBody>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    let n2 = name.clone();
    match tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::mutation::set_branch(
            &store, &db, &org, &repo, &name, &body.target,
        ))
    })
    .await
    {
        Ok(Ok(sha)) => Json(json!({ "name": n2, "sha": sha })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn delete_branch_handler(
    State(state): State<AppState>,
    Path((org, repo, name)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    match tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::mutation::delete_branch(&store, &db, &org, &repo, &name))
    })
    .await
    {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn set_tag_handler(
    State(state): State<AppState>,
    Path((org, repo, name)): Path<(String, String, String)>,
    Json(body): Json<TagBody>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    let n2 = name.clone();
    match tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::mutation::set_tag(
            &store, &db, &org, &repo, &name, &body.target,
        ))
    })
    .await
    {
        Ok(Ok(sha)) => Json(json!({ "name": n2, "sha": sha })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn delete_tag_handler(
    State(state): State<AppState>,
    Path((org, repo, name)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    match tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::mutation::delete_tag(&store, &db, &org, &repo, &name))
    })
    .await
    {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn create_file(
    State(state): State<AppState>,
    Path((org, repo, path)): Path<(String, String, String)>,
    Json(body): Json<FileBody>,
) -> Response {
    let content = match file_content(&body) {
        Ok(c) => c,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e),
    };
    let message = if body.message.is_empty() {
        format!("add {path}")
    } else {
        body.message.clone()
    };
    let store = state.store.clone();
    let db = state.db.clone();
    let author = (SERVER_AUTHOR.0.to_string(), SERVER_AUTHOR.1.to_string());
    let branch = body.branch.clone();
    match tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::mutation::write_file(
            &store,
            &db,
            &org,
            &repo,
            &branch,
            &path,
            &content,
            &message,
            author,
            None,
        ))
    })
    .await
    {
        Ok(Ok(outcome)) => (
            StatusCode::CREATED,
            Json(json!({ "sha": outcome.sha, "change_id": outcome.change_id })),
        )
            .into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn update_file(
    State(state): State<AppState>,
    Path((org, repo, path)): Path<(String, String, String)>,
    Json(body): Json<FileBody>,
) -> Response {
    let content = match file_content(&body) {
        Ok(c) => c,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e),
    };
    let message = if body.message.is_empty() {
        format!("update {path}")
    } else {
        body.message.clone()
    };
    let store = state.store.clone();
    let db = state.db.clone();
    let author = (SERVER_AUTHOR.0.to_string(), SERVER_AUTHOR.1.to_string());
    let branch = body.branch.clone();
    match tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::mutation::write_file(
            &store,
            &db,
            &org,
            &repo,
            &branch,
            &path,
            &content,
            &message,
            author,
            None,
        ))
    })
    .await
    {
        Ok(Ok(outcome)) => Json(json!({ "sha": outcome.sha, "change_id": outcome.change_id })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn delete_file_handler(
    State(state): State<AppState>,
    Path((org, repo, path)): Path<(String, String, String)>,
    Json(body): Json<FileBody>,
) -> Response {
    let message = if body.message.is_empty() {
        format!("delete {path}")
    } else {
        body.message.clone()
    };
    let store = state.store.clone();
    let db = state.db.clone();
    let author = (SERVER_AUTHOR.0.to_string(), SERVER_AUTHOR.1.to_string());
    let branch = body.branch.clone();
    match tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::mutation::delete_file(
            &store, &db, &org, &repo, &branch, &path, &message, author,
        ))
    })
    .await
    {
        Ok(Ok(outcome)) => Json(json!({ "sha": outcome.sha, "change_id": outcome.change_id })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}


// ── extended read surface (E) ──

pub async fn commit_log_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let page: usize = q.get("page").and_then(|v| v.parse().ok()).unwrap_or(1);
    let limit: usize = q.get("limit").and_then(|v| v.parse().ok()).unwrap_or(20);
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::commit_log(&store, &org, &repo, page.saturating_sub(1), limit))
    })
    .await;
    match r {
        Ok(Ok((items, total))) => Json(json!({ "total_count": total, "commits": items })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn tags_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::tags(&store, &org, &repo))
    })
    .await;
    match r {
        Ok(Ok(tags)) => Json(json!({ "tags": tags })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn refs_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::all_refs(&store, &org, &repo))
    })
    .await;
    match r {
        Ok(Ok(refs)) => {
            let items: Vec<Value> = refs
                .into_iter()
                .map(|(name, sha)| json!({ "ref": name, "sha": sha }))
                .collect();
            Json(json!({ "refs": items })).into_response()
        }
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn contents_handler(
    State(state): State<AppState>,
    Path((org, repo, path)): Path<(String, String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let sha = q.get("ref").cloned().unwrap_or_default();
    let store = state.store.clone();
    let is_dir = path.ends_with('/') || path.is_empty();
    let r = tokio::task::spawn_blocking(move || {
        if is_dir {
            pollster::block_on(jjlab_git::read::contents_dir(&store, &org, &repo, &sha))
                .map(|entries| Json(json!({ "entries": entries })).into_response())
        } else {
            pollster::block_on(jjlab_git::read::contents_entry(&store, &org, &repo, &sha, &path))
                .map(|v| Json(v).into_response())
        }
    })
    .await;
    match r {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn compare_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (Some(base), Some(head)) = (q.get("base").cloned(), q.get("head").cloned()) else {
        return json_err(StatusCode::BAD_REQUEST, "base and head required".into());
    };
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::compare_patch(&store, &org, &repo, &base, &head))
    })
    .await;
    match r {
        Ok(Ok(patch)) => Json(json!({ "diff": patch })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn archive_handler(
    State(state): State<AppState>,
    Path((org, repo, ball_type, sha)): Path<(String, String, String, String)>,
) -> Response {
    if ball_type != "tarball" {
        return json_err(StatusCode::BAD_REQUEST, "only tarball supported".into());
    }
    let store = state.store.clone();
    let (o2, r2, s2) = (org.clone(), repo.clone(), sha.clone());
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::archive_tarball(&store, &o2, &r2, &s2))
    })
    .await;
    match r {
        Ok(Ok(bytes)) => (
            StatusCode::OK,
            [
                ("content-type", "application/gzip"),
                (
                    "content-disposition",
                    &format!("attachment; filename=\"{repo}-{sha}.tar.gz\"")[..],
                ),
            ],
            bytes,
        )
            .into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}


pub async fn contents_root_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let sha = q.get("ref").cloned().unwrap_or_default();
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::contents_dir(&store, &org, &repo, &sha))
    })
    .await;
    match r {
        Ok(Ok(entries)) => Json(json!({ "entries": entries })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}


// ── merge requests (M3) ──

pub 
fn mr_json(mr: &jjlab_core::db::MrRow, review_state: String) -> Value {
    json!({
        "number": mr.number,
        "title": mr.title,
        "body": mr.description,
        "author": mr.author,
        "state": mr.state,
        "head_change_id": mr.head_change_id,
        "head_sha": mr.head_sha,
        "base": mr.base_rev,
        "review_state": review_state,
    })
}

#[allow(clippy::result_large_err)]
pub 
fn load_mr(state: &AppState, org: &str, repo: &str, number: i64) -> Result<jjlab_core::db::MrRow, Response> {
    let repo_id = format!("{org}/{repo}");
    state
        .db
        .get_mr_by_number(&repo_id, number)
        .map_err(|e| gitea_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| gitea_err(StatusCode::NOT_FOUND, format!("pull request {number} not found")))
}

pub async fn create_mr_handler(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<CreateMrBody>,
) -> Response {
    // Resolve head rev to its change-id + sha so force-pushes can re-associate.
    let store = state.store.clone();
    let head = body.head.clone();
    let (o2, r2) = (org.clone(), repo.clone());
    let resolved = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::resolve_rev(&store, &o2, &r2, &head))
    })
    .await;
    let (head_change_id, head_sha) = match resolved {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let repo_id = format!("{org}/{repo}");
    let _ = state.db.upsert_org(&org, &org);
    let _ = state.db.upsert_repo(&repo_id, &org, &repo, "main", None);
    match state.db.create_mr(
        &repo_id,
        &body.title,
        &body.body,
        SERVER_AUTHOR.0,
        &head_change_id,
        Some(&head_sha),
        Some(&body.head),
        &body.base,
    ) {
        Ok(mr) => (
            StatusCode::CREATED,
            Json(mr_json(&mr, "pending".to_string())),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn list_mrs(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let mrs = match state.db.list_mrs(&repo_id, q.get("state").map(String::as_str)) {
        Ok(v) => v,
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let mut items = Vec::new();
    for mr in mrs {
        let rs = state.db.mr_review_state(mr.id).unwrap_or_default();
        items.push(mr_json(&mr, rs));
    }
    Json(json!({ "pull_requests": items })).into_response()
}

pub async fn get_mr_handler(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    let rs = state.db.mr_review_state(mr.id).unwrap_or_default();
    Json(mr_json(&mr, rs)).into_response()
}

pub async fn update_mr_handler(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<UpdateMrBody>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    match state.db.update_mr(mr.id, Some(&body.state), None, None) {
        Ok(updated) => {
            let rs = state.db.mr_review_state(mr.id).unwrap_or_default();
            Json(mr_json(&updated, rs)).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn add_review(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<ReviewBody>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    match state.db.add_mr_review(
        mr.id,
        SERVER_AUTHOR.0,
        &body.state,
        &body.body,
        mr.head_sha.as_deref(),
    ) {
        Ok(_) => {
            let rs = state.db.mr_review_state(mr.id).unwrap_or_default();
            Json(json!({ "review_state": rs })).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn list_reviews(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    match state.db.list_mr_reviews(mr.id) {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    json!({
                        "reviewer": r.reviewer,
                        "state": r.state,
                        "body": r.body,
                        "commit_sha": r.commit_sha,
                    })
                })
                .collect();
            Json(json!({ "reviews": items })).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn add_comment(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<CommentBody>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    match state.db.add_mr_comment(
        mr.id,
        SERVER_AUTHOR.0,
        &body.body,
        body.path.as_deref(),
        mr.head_sha.as_deref(),
    ) {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn list_comments(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    match state.db.list_mr_comments(mr.id) {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    json!({
                        "author": r.author,
                        "body": r.body,
                        "path": r.path,
                        "commit_sha": r.commit_sha,
                    })
                })
                .collect();
            Json(json!({ "comments": items })).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// Unified diff between MR base and head.
pub async fn mr_diff(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i64)>,
) -> Response {
    let mr = match load_mr(&state, &org, &repo, number) {
        Ok(mr) => mr,
        Err(resp) => return resp,
    };
    let Some(head_sha) = mr.head_sha.clone() else {
        return json_err(StatusCode::CONFLICT, "mr has no head sha".into());
    };
    let store = state.store.clone();
    let base = mr.base_rev.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::read::compare_patch(&store, &org, &repo, &base, &head_sha))
    })
    .await;
    match r {
        Ok(Ok(patch)) => Json(json!({ "diff": patch })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}


// ── operation-log cloud sync (M4) ──

/// SSE stream of op-log events for a repo. Query: `after=<op_id>` to catch up.
pub async fn subscribe_ops(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let repo_id = format!("{org}/{repo}");

    // Catch-up replay (after=op_id) sent inline before live polling loop.
    let after = q.get("after").cloned();
    let db = state.db.clone();

    let stream = async_stream::stream! {
        if let Some(after_id) = &after {
            for ev in jjlab_git::ops::ops_since(&db, &repo_id, after_id) {
                yield Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default()
                    .event("op")
                    .id(ev.id.clone())
                    .data(serde_json::json!({
                        "id": ev.id,
                        "repo_id": ev.repo_id,
                        "op_type": ev.op_type,
                        "payload": ev.payload,
                        "undo_of": ev.undo_of,
                    }).to_string()));
            }
        }
        // Live tail: poll the DB on an interval (simple, no broadcast bus yet).
        let mut seen = after;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let Ok(rows) = db.list_op_log(&repo_id) else { continue };
            let start = rows.iter().position(|r| Some(&r.id) == seen.as_ref()).map(|i| i + 1).unwrap_or(0);
            for ev in rows.into_iter().skip(start) {
                seen = Some(ev.id.clone());
                yield Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default()
                    .event("op")
                    .id(ev.id.clone())
                    .data(serde_json::json!({
                        "id": ev.id,
                        "repo_id": ev.repo_id,
                        "op_type": ev.op_type,
                        "payload": ev.payload,
                        "undo_of": ev.undo_of,
                    }).to_string()));
            }
        }
    };

    axum::response::Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response()
}

pub async fn undo_op(
    State(state): State<AppState>,
    Path((org, repo, op_id)): Path<(String, String, String)>,
) -> Response {
    let store = state.store.clone();
    let db = state.db.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::ops::undo_operation(&store, &db, &org, &repo, &op_id))
    })
    .await;
    match r {
        Ok(Ok(ev)) => Json(json!({
            "ok": true,
            "undo_op_id": ev.id,
            "undo_of": ev.undo_of,
        }))
        .into_response(),
        Ok(Err(e)) => json_err(StatusCode::CONFLICT, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn op_head(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let store = state.store.clone();
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::ops::current_op_id(&store, &org, &repo))
    })
    .await;
    match r {
        Ok(Ok(id)) => Json(json!({ "op_id": id })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}


// ── actions/CI (M6-C1) ──

pub 
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
    match state.db.list_workflows(&repo_id) {
        Ok(rows) => {
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
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
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
    let r = tokio::task::spawn_blocking(move || {
        pollster::block_on(jjlab_git::actions::dispatch(&store, &db, &org, &repo, id, &logs_root))
    })
    .await;
    match r {
        Ok(Ok(run_ids)) => Json(json!({ "run_ids": run_ids })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn list_runs(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    match state.db.list_runs(&repo_id) {
        Ok(rows) => {
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
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn list_run_jobs(
    State(state): State<AppState>,
    Path((org, repo, run_id)): Path<(String, String, i64)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let run = match state.db.get_run(run_id) {
        Ok(Some(r)) => r,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, format!("run {run_id} not found")),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    if run.repo_id != repo_id {
        return json_err(StatusCode::NOT_FOUND, "run not in this repo".into());
    }
    match state.db.list_jobs(run_id) {
        Ok(rows) => {
            let items: Vec<Value> = rows.iter().map(job_json).collect();
            Json(json!({ "jobs": items })).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn job_logs(
    State(state): State<AppState>,
    Path((org, repo, job_id)): Path<(String, String, i64)>,
) -> Response {
    let job = match state.db.get_job(job_id) {
        Ok(Some(j)) => j,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, format!("job {job_id} not found")),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    match state.db.get_run(job.run_id) {
        Ok(Some(run)) if run.repo_id == format!("{org}/{repo}") => {}
        _ => return json_err(StatusCode::NOT_FOUND, "job not in this repo".into()),
    }
    let bytes = jjlab_git::actions::job_log(job.log_path.as_deref().unwrap_or_default());
    (StatusCode::OK, [("content-type", "text/plain")], bytes).into_response()
}


// ── releases (M5-A) ──

pub 
fn release_json(r: &jjlab_core::db::ReleaseRow, assets: Vec<jjlab_core::db::ReleaseAssetRow>) -> Value {
    let items: Vec<Value> = assets
        .into_iter()
        .map(|a| {
            json!({
                "name": a.name, "size": a.size, "digest": a.digest,
                "content_type": a.content_type,
                "browser_download_url": format!("/api/v1/repos/{}/releases/{}/assets/{}", r.repo_id, r.tag_name, a.name),
            })
        })
        .collect();
    json!({
        "id": r.id,
        "tag_name": r.tag_name,
        "name": r.name,
        "body": r.body,
        "draft": r.draft,
        "prerelease": r.prerelease,
        "assets": items,
    })
}

pub async fn create_release(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<ReleaseBody>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let _ = state.db.upsert_org(&org, &org);
    let _ = state.db.upsert_repo(&repo_id, &org, &repo, "main", None);
    let store = state.store.clone();
    let db2 = state.db.clone();
    let tag = body.tag_name.clone();
    let ensure = tokio::task::spawn_blocking(move || {
        pollster::block_on(async {
            let repo_arc = jjlab_git::read::open(&store, &org, &repo).await?;
            match jjlab_git::read::resolve_commit(&repo_arc, &tag) {
                Ok(_) => Ok(()),
                Err(_) => {
                    let head = jjlab_git::read::head_sha(&store, &org, &repo).await?;
                    jjlab_git::mutation::set_tag(&store, &db2, &org, &repo, &tag, &head)
                        .await
                        .map(|_| ())
                }
            }
        })
    })
    .await;
    match ensure {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return json_err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
    match state
        .db
        .create_release(&repo_id, &body.tag_name, &body.name, &body.body, body.draft, body.prerelease)
    {
        Ok(r) => (
            StatusCode::CREATED,
            Json(release_json(&r, state.db.list_release_assets(r.id).unwrap_or_default())),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn list_releases(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    match state.db.list_releases(&repo_id) {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    let assets = state.db.list_release_assets(r.id).unwrap_or_default();
                    release_json(&r, assets)
                })
                .collect();
            Json(json!({ "releases": items })).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn get_release(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    match state.db.get_release_by_tag(&repo_id, &tag) {
        Ok(Some(r)) => {
            let assets = state.db.list_release_assets(r.id).unwrap_or_default();
            Json(release_json(&r, assets)).into_response()
        }
        Ok(None) => json_err(StatusCode::NOT_FOUND, format!("release {tag} not found")),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn latest_release(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let all = match state.db.list_releases(&repo_id) {
        Ok(v) => v,
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let Some(first) = all.into_iter().find(|r| !r.draft && !r.prerelease) else {
        return json_err(StatusCode::NOT_FOUND, "no releases".into());
    };
    let assets = state.db.list_release_assets(first.id).unwrap_or_default();
    Json(release_json(&first, assets)).into_response()
}

pub async fn delete_release(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    match state.db.get_release_by_tag(&repo_id, &tag) {
        Ok(Some(r)) => match state.db.delete_release(r.id) {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        },
        Ok(None) => json_err(StatusCode::NOT_FOUND, format!("release {tag} not found")),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn upload_asset(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
    mut multipart: axum::extract::Multipart,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let Some(rel) = (match state.db.get_release_by_tag(&repo_id, &tag) {
        Ok(Some(r)) => Some(r),
        Ok(None) => None,
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }) else {
        return json_err(StatusCode::NOT_FOUND, format!("release {tag} not found"));
    };

    let mut saved = Vec::new();
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.file_name().unwrap_or("asset.bin").to_string();
        let ct = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let Ok(data) = field.bytes().await else {
            return json_err(StatusCode::BAD_REQUEST, "failed to read upload".into());
        };
        let digest = match state.assets.put(&data) {
            Ok(d) => d,
            Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("store asset: {e}")),
        };
        match state
            .db
            .add_release_asset(rel.id, &name, data.len() as i64, &digest, &ct)
        {
            Ok(a) => saved.push(a),
            Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        }
    }
    if saved.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "multipart body required".into());
    }
    (
        StatusCode::CREATED,
        Json(json!({ "assets": saved.iter().map(|a| json!({
            "name": a.name, "size": a.size, "digest": a.digest,
        })).collect::<Vec<_>>() })),
    )
        .into_response()
}

pub async fn download_asset(
    State(state): State<AppState>,
    Path((org, repo, tag, name)): Path<(String, String, String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let asset = match state
        .db
        .get_release_by_tag(&repo_id, &tag)
        .ok()
        .flatten()
        .and_then(|r| state.db.get_release_asset(r.id, &name).ok().flatten())
    {
        Some(a) => a,
        None => return json_err(StatusCode::NOT_FOUND, format!("asset {name} not found")),
    };
    let file = match state.assets.open(&asset.digest) {
        Ok(Some(f)) => f,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, "asset blob missing".into()),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("open blob: {e}")),
    };
    let stream = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file));
    (
        StatusCode::OK,
        [
            ("content-type", asset.content_type.as_str()),
            ("etag", format!("\"{}\"", asset.digest).as_str()),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response()
}

pub async fn delete_asset(
    State(state): State<AppState>,
    Path((org, repo, tag, name)): Path<(String, String, String, String)>,
) -> Response {
    let repo_id = format!("{org}/{repo}");
    let Some(rel) = state.db.get_release_by_tag(&repo_id, &tag).ok().flatten() else {
        return json_err(StatusCode::NOT_FOUND, format!("release {tag} not found"));
    };
    match state.db.delete_release_asset(rel.id, &name) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// Build the full application router. Tests drive this directly via
/// `tower::ServiceExt::oneshot`; `main` serves it with `axum::serve`.
pub fn build_router(state: AppState) -> axum::Router {
        let mut router = Router::new()
            .route("/api/v1/health", get(health))
            .route("/api/v1/repos", get(list_orgs))
            .route("/api/v1/graph/{org}/{repo}", get(graph_handler))
            .route("/api/v1/repos/{org}/{repo}/file-log", get(file_log_handler))
            .route("/api/v1/repos/{org}/{repo}/{branch}/search", get(search_handler))
            // Git-aligned read surface (commit-addressed).
            .route("/api/v1/repos/{org}/{repo}/git/commits/{sha}", get(commit_info))
            .route("/api/v1/repos/{org}/{repo}/branches", get(list_branches))
            .route("/api/v1/repos/{org}/{repo}/raw/{*path}", get(raw_file))
            .route("/api/v1/repos/{org}/{repo}/tree/{sha}", get(tree_at_sha))
            // jj-native: change (change-id-addressed) read.
            .route("/api/v1/repos/{org}/{repo}/changes/{change_id}", get(change_info))
            // Metadata + op-log (jj-native).
            .route("/api/v1/repos/{org}/{repo}/op-log", get(list_op_log))
            .route("/api/v1/repos/{org}/{repo}/conflicts", get(list_conflicts))
            .route("/api/v1/repos/{org}/{repo}/bookmarks", get(list_bookmarks))
            .route("/api/v1/repos/{org}/{repo}/change-ids", get(list_change_ids))
            // Mirror sync (Gitea-aligned names).
            .route("/api/v1/repos/{org}/{repo}/sync/clone", post(clone_remote))
            .route("/api/v1/repos/{org}/{repo}/mirror-sync", post(fetch_remote))
            .route("/api/v1/repos/{org}/{repo}/push_mirrors", post(push_mirror))
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
            // Extended read surface (E), Gitea-aligned.
            .route("/api/v1/repos/{org}/{repo}/commits", get(commit_log_handler))
            .route(
                "/api/v1/repos/{org}/{repo}/tags",
                get(tags_handler),
            )
            .route("/api/v1/repos/{org}/{repo}/git/refs", get(refs_handler))
            .route(
                "/api/v1/repos/{org}/{repo}/contents",
                get(contents_root_handler),
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
            // Operation log cloud sync (M4).
            .route("/api/v1/repos/{org}/{repo}/op-log/stream", get(subscribe_ops))
            .route(
                "/api/v1/repos/{org}/{repo}/op-log/{op_id}/undo",
                post(undo_op),
            )
            .route("/api/v1/repos/{org}/{repo}/op-head", get(op_head))
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
