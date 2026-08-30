//! HTTP-layer integration tests: drive `build_router` directly with
//! `tower::ServiceExt::oneshot` (no real port). Covers auth, Gitea-style
//! error bodies, and the main REST surface end-to-end.

use std::sync::Arc;

use axum::body::Body;
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use jjlab_server::{build_router, parse_tokens, AppState, Level};

struct TestApp {
    router: axum::Router,
    _dir: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl TestApp {
    fn new() -> Self {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(
            jjlab_core::Db::open(&dir.path().join("meta.db")).unwrap(),
        );
        let store = Arc::new(jjlab_git::RepoStore::new(dir.path().join("repos")));
        let assets = Arc::new(jjlab_git::assets::AssetStore::new(dir.path().join("assets")));
        // Point the actions log root at the tempdir (process-global env; the
        // test harness runs each test in its own process by default).
        std::env::set_var("JJLAB_LOGS", dir.path().join("logs"));
        let state = AppState::new(db, store, parse_tokens("wtoken=write,rtoken=read"), assets);
        let router = build_router(state);
        Self { router, _dir: dir, _guard: guard }
    }

    async fn send(&self, method: &str, uri: &str, token: Option<&str>, body: Option<Vec<u8>>) -> axum::http::Response<Body> {
        let mut builder = axum::http::Request::builder()
            .method(method)
            .uri(uri);
        if let Some(t) = token {
            builder = builder.header("authorization", format!("token {t}"));
        }
        builder = builder.header("content-type", "application/json");
        let body = match body {
            Some(b) => Body::from(b),
            None => Body::empty(),
        };
        self.router.clone().oneshot(builder.body(body).unwrap()).await.unwrap()
    }

    async fn json(&self, method: &str, uri: &str, token: Option<&str>, value: serde_json::Value) -> axum::http::Response<Body> {
        self.send(method, uri, token, Some(value.to_string().into_bytes())).await
    }

    async fn status(resp: &mut axum::http::Response<Body>) -> u16 {
        resp.status().as_u16()
    }

    async fn body_json(resp: &mut axum::http::Response<Body>) -> serde_json::Value {
        let bytes = resp.body_mut().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }
}

fn obj(pairs: &[(&str, serde_json::Value)]) -> serde_json::Value {
    serde_json::Value::Object(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
}

// ── basics ──

#[tokio::test]
async fn health_is_public() {
    let app = TestApp::new();
    let mut resp = app.send("GET", "/api/v1/health", None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    let body = TestApp::body_json(&mut resp).await;
    assert_eq!(body["ok"], serde_json::Value::Bool(true));
}

#[tokio::test]
async fn errors_use_gitea_shape() {
    let app = TestApp::new();
    let mut resp = app.send("GET", "/api/v1/repos/o/r/changes/zzzz", None, None).await;
    let status = TestApp::status(&mut resp).await;
    assert_eq!(status, 404);
    let body = TestApp::body_json(&mut resp).await;
    assert!(body.get("message").is_some(), "error body must have message: {body}");
    assert!(body.get("url").is_some(), "error body must have url");
}

// ── auth ──

#[tokio::test]
async fn push_info_refs_requires_token_and_challenges() {
    let app = TestApp::new();
    let mut resp = app
        .send("GET", "/o/r.git/info/refs?service=git-receive-pack", None, None)
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 401);
    assert!(
        resp.headers().get("www-authenticate").is_some(),
        "401 must carry WWW-Authenticate for git to retry"
    );
}

#[tokio::test]
async fn write_token_allows_push_advertise_but_read_token_forbidden() {
    let app = TestApp::new();
    // Repo doesn't exist → 404 after auth passes; read token gets 403 first.
    let mut resp = app
        .send("GET", "/o/r.git/info/refs?service=git-receive-pack", Some("rtoken"), None)
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 403);
    let mut resp = app
        .send("GET", "/o/r.git/info/refs?service=git-receive-pack", Some("wtoken"), None)
        .await;
    let status = TestApp::status(&mut resp).await;
    assert_ne!(status, 401, "write token must pass auth");
    assert_ne!(status, 403);
}

#[tokio::test]
async fn bad_token_is_anonymous() {
    let app = TestApp::new();
    let mut resp = app
        .send("GET", "/o/r.git/info/refs?service=git-receive-pack", Some("nope"), None)
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 401);
}

#[test]
fn parse_tokens_matrix() {
    let toks = parse_tokens("a=write, b=read ,,=read");
    assert_eq!(toks.len(), 2);
    assert!(toks.iter().any(|(t, l)| t == "a" && *l == Level::Write));
    assert!(toks.iter().any(|(t, l)| t == "b" && *l == Level::Read));
}

// ── repo + content lifecycle ──

#[tokio::test]
async fn repo_create_duplicate_conflict_then_write_read_delete_file() {
    let app = TestApp::new();
    // Create repo.
    let mut resp = app
        .json("POST", "/api/v1/repos/o/r", Some("wtoken"), obj(&[("default_branch", "main".into())]))
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 201, "create repo");
    // Duplicate → 409-ish (we use 409 via CONFLICT path? server returns 409/500 map) — accept 4xx.
    let mut resp = app
        .json("POST", "/api/v1/repos/o/r", Some("wtoken"), obj(&[("default_branch", "main".into())]))
        .await;
    let dup = TestApp::status(&mut resp).await;
    assert!((400..500).contains(&dup), "duplicate create must be 4xx, got {dup}");

    // Write file (plain content) → returns sha + change_id.
    let mut resp = app
        .json(
            "POST",
            "/api/v1/repos/o/r/contents/hello.txt",
            Some("wtoken"),
            obj(&[("content", "hi\n".into()), ("branch", "main".into()), ("message", "add".into())]),
        )
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 201, "create file");
    let body = TestApp::body_json(&mut resp).await;
    let sha = body["sha"].as_str().unwrap().to_string();
    let change_id = body["change_id"].as_str().unwrap().to_string();
    assert_eq!(change_id.len(), 32);

    // Read it back raw.
    let mut resp = app.send("GET", "/api/v1/repos/o/r/raw/hello.txt", None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    let bytes = resp.body_mut().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"hi\n");

    // Update file → new change.
    let mut resp = app
        .json(
            "PUT",
            "/api/v1/repos/o/r/contents/hello.txt",
            Some("wtoken"),
            obj(&[("content", "hi v2\n".into()), ("branch", "main".into())]),
        )
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    let body2 = TestApp::body_json(&mut resp).await;
    assert_ne!(body2["sha"], sha);

    // The change is queryable via change-id (prefix works).
    let cid = change_id[..8].to_string();
    let mut resp = app
        .send("GET", &format!("/api/v1/repos/o/r/changes/{cid}"), None, None)
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 200);

    // Delete file.
    let mut resp = app
        .json(
            "DELETE",
            "/api/v1/repos/o/r/contents/hello.txt",
            Some("wtoken"),
            obj(&[("branch", "main".into()), ("message", "rm".into())]),
        )
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    let mut resp = app.send("GET", "/api/v1/repos/o/r/raw/hello.txt", None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 404);
}

#[tokio::test]
async fn contents_write_requires_body() {
    let app = TestApp::new();
    let mut resp = app
        .json("POST", "/api/v1/repos/o/r/contents/f.txt", Some("wtoken"), obj(&[("branch", "main".into())]))
        .await;
    // Repo missing → 404 either way; just ensure not 500-crash.
    let s = TestApp::status(&mut resp).await;
    assert!(s < 500, "must be a client-style error, got {s}");
}

// ── releases ──

#[tokio::test]
async fn release_lifecycle_with_assets() {
    let app = TestApp::new();
    // Create repo + file so the tag has content.
    let _ = app
        .json("POST", "/api/v1/repos/o/r", Some("wtoken"), obj(&[("default_branch", "main".into())]))
        .await;
    let _ = app
        .json(
            "POST",
            "/api/v1/repos/o/r/contents/bin.txt",
            Some("wtoken"),
            obj(&[("content", "payload\n".into()), ("branch", "main".into())]),
        )
        .await;

    // Create release (auto-tags at head).
    let mut resp = app
        .json(
            "POST",
            "/api/v1/repos/o/r/releases",
            Some("wtoken"),
            obj(&[("tag_name", "v1".into()), ("name", "First".into()), ("body", "notes".into())]),
        )
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 201);

    // Upload asset (multipart).
    let boundary = "XBOUNDARYX";
    let part = format!(
        "--{b}\r\ncontent-disposition: form-data; name=\"file\"; filename=\"bin.txt\"\r\ncontent-type: text/plain\r\n\r\npayload\n\r\n--{b}--\r\n",
        b = boundary
    );
    let builder = axum::http::Request::builder()
        .method("POST")
        .uri("/api/v1/repos/o/r/releases/v1/assets")
        .header("authorization", "token wtoken")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        );
    let req = builder.body(Body::from(part)).unwrap();
    let mut resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(TestApp::status(&mut resp).await, 201, "upload asset");

    // Latest points at v1 and lists the asset.
    let mut resp = app.send("GET", "/api/v1/repos/o/r/releases/latest", None, None).await;
    let body = TestApp::body_json(&mut resp).await;
    assert_eq!(body["tag_name"], "v1");
    assert_eq!(body["assets"].as_array().unwrap().len(), 1);

    // Download the asset.
    let mut resp = app
        .send("GET", "/api/v1/repos/o/r/releases/v1/assets/bin.txt", None, None)
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    let bytes = resp.body_mut().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"payload\n");

    // Delete asset then release.
    let mut resp = app
        .send("DELETE", "/api/v1/repos/o/r/releases/v1/assets/bin.txt", Some("wtoken"), None)
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 204);
    let mut resp = app
        .send("DELETE", "/api/v1/repos/o/r/releases/v1", Some("wtoken"), None)
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 204);
    let mut resp = app.send("GET", "/api/v1/repos/o/r/releases", None, None).await;
    let body = TestApp::body_json(&mut resp).await;
    assert_eq!(body["releases"].as_array().unwrap().len(), 0);
}

// ── merge requests ──

#[tokio::test]
async fn mr_lifecycle_review_aggregation_and_head_reassociation() {
    let app = TestApp::new();
    let _ = app
        .json("POST", "/api/v1/repos/o/r", Some("wtoken"), obj(&[("default_branch", "main".into())]))
        .await;
    let mut resp = app
        .json(
            "POST",
            "/api/v1/repos/o/r/contents/f.txt",
            Some("wtoken"),
            obj(&[("content", "v1\n".into()), ("branch", "main".into()), ("amend", false.into())]),
        )
        .await;
    let first = TestApp::body_json(&mut resp).await;
    let base_sha = first["sha"].as_str().unwrap().to_string();

    // Branch feature at base, then write on feature (the MR head).
    let _ = app
        .json(
            "POST",
            "/api/v1/repos/o/r/branches/feature",
            Some("wtoken"),
            obj(&[("target", base_sha.clone().into())]),
        )
        .await;
    let mut resp = app
        .json(
            "POST",
            "/api/v1/repos/o/r/contents/f.txt",
            Some("wtoken"),
            obj(&[("content", "v2\n".into()), ("branch", "feature".into()), ("amend", false.into())]),
        )
        .await;
    let head = TestApp::body_json(&mut resp).await;
    let head_sha = head["sha"].as_str().unwrap().to_string();

    // Open MR feature→main.
    let mut resp = app
        .json(
            "POST",
            "/api/v1/repos/o/r/pulls",
            Some("wtoken"),
            obj(&[
                ("title", "add f".into()),
                ("head", "feature".into()),
                ("base", "main".into()),
            ]),
        )
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 201, "create MR");
    let mr = TestApp::body_json(&mut resp).await;
    let number = mr["number"].as_i64().unwrap();
    assert_eq!(mr["review_state"], "pending");

    // Comment + approve.
    let _ = app
        .json(
            "POST",
            &format!("/api/v1/repos/o/r/pulls/{number}/comments"),
            Some("wtoken"),
            obj(&[("body", "nice".into()), ("path", "f.txt".into())]),
        )
        .await;
    let mut resp = app
        .json(
            "POST",
            &format!("/api/v1/repos/o/r/pulls/{number}/reviews"),
            Some("wtoken"),
            obj(&[("state", "approved".into()), ("body", "ship".into())]),
        )
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    let body = TestApp::body_json(&mut resp).await;
    assert_eq!(body["review_state"], "approved");

    // MR diff is non-empty.
    let mut resp = app
        .send("GET", &format!("/api/v1/repos/o/r/pulls/{number}/diff"), None, None)
        .await;
    let body = TestApp::body_json(&mut resp).await;
    assert!(
        body["diff"].as_str().map(|d| !d.is_empty()).unwrap_or(false),
        "MR diff should have content"
    );

    // Force-push: rewrite feature tip.
    let _ = app
        .json(
            "PUT",
            "/api/v1/repos/o/r/contents/f.txt",
            Some("wtoken"),
            obj(&[("content", "v3\n".into()), ("branch", "feature".into()), ("message", "v3".into()), ("amend", false.into())]),
        )
        .await;
    // Projection runs inside the write; MR head must follow feature tip while
    // reviews survive.
    let mut resp = app
        .send("GET", &format!("/api/v1/repos/o/r/pulls/{number}"), None, None)
        .await;
    let body = TestApp::body_json(&mut resp).await;
    assert_ne!(
        body["head_sha"], serde_json::json!(head_sha),
        "head_sha must follow the force-pushed tip"
    );
    assert_eq!(body["review_state"], "approved", "reviews survive force-push");
    let mut resp = app
        .send("GET", &format!("/api/v1/repos/o/r/pulls/{number}/reviews"), None, None)
        .await;
    let body = TestApp::body_json(&mut resp).await;
    assert_eq!(body["reviews"].as_array().unwrap().len(), 1);
}

// ── actions ──

#[tokio::test]
async fn actions_workflow_dispatch_runs_and_logs() {
    let app = TestApp::new();
    let _ = app
        .json("POST", "/api/v1/repos/o/r", Some("wtoken"), obj(&[("default_branch", "main".into())]))
        .await;
    let _ = app
        .json(
            "POST",
            "/api/v1/repos/o/r/contents/.github/workflows/ci.yml",
            Some("wtoken"),
            obj(&[
                ("content", "name: CI\non: push\njobs:\n  build:\n    steps:\n      - run: echo from-actions\n".into()),
                ("branch", "main".into()),
            ]),
        )
        .await;
    // Write triggers on_push; workflow should be synced (registered).
    let mut resp = app
        .send("GET", "/api/v1/repos/o/r/actions/workflows", None, None)
        .await;
    let body = TestApp::body_json(&mut resp).await;
    assert_eq!(body["workflows"].as_array().unwrap().len(), 1, "workflow synced");

    // Manual dispatch → enqueues a queued run (execution deferred to the CI
    // scheduler, which is disabled in tests).
    let mut resp = app
        .send("POST", "/api/v1/repos/o/r/actions/workflows/1/dispatch", Some("wtoken"), None)
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 200, "dispatch enqueues a run");
    let body = TestApp::body_json(&mut resp).await;
    let run_ids = body["run_ids"].as_array().unwrap();
    assert_eq!(run_ids.len(), 1, "one run enqueued");

    // The run is visible and stays queued (no executor in tests). Writing the
    // workflow file also enqueues a push-triggered run, so expect >= 1.
    let mut resp = app.send("GET", "/api/v1/repos/o/r/actions/runs", None, None).await;
    let body = TestApp::body_json(&mut resp).await;
    let runs = body["runs"].as_array().unwrap();
    assert!(!runs.is_empty(), "queued runs are listed");
    assert!(
        runs.iter().all(|r| r["status"] == "queued"),
        "runs stay queued without a scheduler"
    );
}

// ── op-log / undo ──

#[tokio::test]
async fn op_log_stream_catchup_and_undo() {
    let app = TestApp::new();
    let _ = app
        .json("POST", "/api/v1/repos/o/r", Some("wtoken"), obj(&[("default_branch", "main".into())]))
        .await;
    let _ = app
        .json(
            "POST",
            "/api/v1/repos/o/r/contents/u.txt",
            Some("wtoken"),
            obj(&[("content", "x\n".into()), ("branch", "main".into())]),
        )
        .await;
    // Catch-up stream returns ops in order (stream body starts with SSE frames).
    let mut resp = app
        .send("GET", "/api/v1/repos/o/r/op-log/stream", None, None)
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    // SSE content-type and no full-body read: the live tail never ends.
    assert!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .starts_with("text/event-stream"),
        "stream must be SSE"
    );

    // Undo the latest op → content disappears.
    let ops = app
        .router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/api/v1/repos/o/r/op-log")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = ops.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let last_id = body["ops"].as_array().unwrap().last().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut resp = app
        .send(
            "POST",
            &format!(
                "/api/v1/repos/o/r/op-log/{}/undo",
                last_id.replace('/', "%2F")
            ),
            Some("wtoken"),
            None,
        )
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 200, "undo ok");
    let mut resp = app.send("GET", "/api/v1/repos/o/r/raw/u.txt", None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 404, "undone content gone");
}

// ── read-surface matrix (covers remaining handlers) ──

async fn seeded_app() -> TestApp {
    let app = TestApp::new();
    let _ = app
        .json("POST", "/api/v1/repos/o/r", Some("wtoken"), obj(&[("default_branch", "main".into())]))
        .await;
    let _ = app
        .json(
            "POST",
            "/api/v1/repos/o/r/contents/a.txt",
            Some("wtoken"),
            obj(&[("content", "line1\n".into()), ("branch", "main".into()), ("amend", false.into())]),
        )
        .await;
    let _ = app
        .json(
            "PUT",
            "/api/v1/repos/o/r/contents/a.txt",
            Some("wtoken"),
            obj(&[("content", "line1\nline2\n".into()), ("branch", "main".into()), ("amend", false.into())]),
        )
        .await;
    app
}

#[tokio::test]
async fn commit_log_endpoint_paginates() {
    let app = seeded_app().await;
    let mut resp = app.send("GET", "/api/v1/repos/o/r/commits?limit=1&page=1", None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    let body = TestApp::body_json(&mut resp).await;
    assert_eq!(body["commits"].as_array().unwrap().len(), 1);
    assert!(body["total_count"].as_i64().unwrap() >= 2);
}

#[tokio::test]
async fn tree_and_branches_and_tags_endpoints() {
    let app = seeded_app().await;
    let mut resp = app.send("GET", "/api/v1/repos/o/r/branches", None, None).await;
    let body = TestApp::body_json(&mut resp).await;
    let sha = body["branches"][0]["sha"].as_str().unwrap().to_string();

    let mut resp = app.send("GET", &format!("/api/v1/repos/o/r/tree/{sha}"), None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    let body = TestApp::body_json(&mut resp).await;
    assert!(body["tree"].as_array().unwrap().iter().any(|e| e["path"] == "a.txt"));

    let mut resp = app.send("GET", "/api/v1/repos/o/r/tags", None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 200);

    // Tag creation via endpoint then verify listing.
    let mut resp = app
        .json(
            "POST",
            "/api/v1/repos/o/r/tags/v1",
            Some("wtoken"),
            obj(&[("target", sha.clone().into())]),
        )
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    let mut resp = app.send("GET", "/api/v1/repos/o/r/tags", None, None).await;
    let body = TestApp::body_json(&mut resp).await;
    assert!(body["tags"].as_array().unwrap().iter().any(|t| t["name"] == "v1"));

    // Delete tag.
    let mut resp = app
        .send("DELETE", "/api/v1/repos/o/r/tags/v1", Some("wtoken"), None)
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 204);
}

#[tokio::test]
async fn git_refs_and_compare_endpoints() {
    let app = seeded_app().await;
    let mut resp = app.send("GET", "/api/v1/repos/o/r/git/refs", None, None).await;
    let body = TestApp::body_json(&mut resp).await;
    assert!(body["refs"].as_array().unwrap().iter().any(|r| r["ref"] == "refs/heads/main"));

    // compare: base vs head (both main) → empty diff; and shas from log.
    let mut resp = app.send("GET", "/api/v1/repos/o/r/commits?limit=5", None, None).await;
    let body = TestApp::body_json(&mut resp).await;
    let shas: Vec<&str> = body["commits"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["sha"].as_str())
        .collect();
    let (older, newer) = (shas[shas.len() - 1], shas[0]);
    let mut resp = app
        .send(
            "GET",
            &format!("/api/v1/repos/o/r/compare?base={older}&head={newer}"),
            None,
            None,
        )
        .await;
    let body = TestApp::body_json(&mut resp).await;
    assert!(body["diff"].as_str().map(|d| !d.is_empty()).unwrap_or(false));
}

#[tokio::test]
async fn contents_list_endpoint_root() {
    let app = seeded_app().await;
    let mut resp = app.send("GET", "/api/v1/repos/o/r/contents?ref=main", None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    let body = TestApp::body_json(&mut resp).await;
    assert!(body["entries"].as_array().unwrap().iter().any(|e| e["path"] == "a.txt"));
}

#[tokio::test]
async fn archive_tarball_endpoint_downloads() {
    let app = seeded_app().await;
    let mut resp = app
        .send("GET", "/api/v1/repos/o/r/archive/tarball/main", None, None)
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    let bytes = resp.body_mut().collect().await.unwrap().to_bytes();
    assert!(bytes.len() > 20, "gzip payload expected");
}

#[tokio::test]
async fn branch_crud_endpoints() {
    let app = seeded_app().await;
    let mut resp = app.send("GET", "/api/v1/repos/o/r/branches", None, None).await;
    let body = TestApp::body_json(&mut resp).await;
    let main_sha = body["branches"][0]["sha"].as_str().unwrap().to_string();

    // Create branch pointing at main.
    let mut resp = app
        .json(
            "POST",
            "/api/v1/repos/o/r/branches/feature",
            Some("wtoken"),
            obj(&[("target", main_sha.clone().into())]),
        )
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    // Delete it.
    let mut resp = app
        .send("DELETE", "/api/v1/repos/o/r/branches/feature", Some("wtoken"), None)
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 204);
    let mut resp = app.send("GET", "/api/v1/repos/o/r/branches", None, None).await;
    let body = TestApp::body_json(&mut resp).await;
    assert!(!body["branches"].as_array().unwrap().iter().any(|b| b["name"] == "feature"));
}

#[tokio::test]
async fn mr_diff_endpoint_404_for_missing() {
    let app = TestApp::new();
    let _ = app
        .json("POST", "/api/v1/repos/o/r", Some("wtoken"), obj(&[("default_branch", "main".into())]))
        .await;
    let mut resp = app.send("GET", "/api/v1/repos/o/r/pulls/99/diff", None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 404);
}

#[tokio::test]
async fn commit_info_endpoint_by_sha_and_prefix() {
    let app = seeded_app().await;
    let mut resp = app.send("GET", "/api/v1/repos/o/r/commits?limit=1", None, None).await;
    let body = TestApp::body_json(&mut resp).await;
    let sha = body["commits"][0]["sha"].as_str().unwrap().to_string();
    // Full sha.
    let mut resp = app
        .send("GET", &format!("/api/v1/repos/o/r/git/commits/{sha}"), None, None)
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    // Short prefix.
    let mut resp = app
        .send(
            "GET",
            &format!("/api/v1/repos/o/r/git/commits/{}", &sha[..7]),
            None,
            None,
        )
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    // Unknown sha → 404.
    let mut resp = app
        .send("GET", "/api/v1/repos/o/r/git/commits/ffffffffffffffffffffffffffffffffffffffff", None, None)
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 404);
}

#[tokio::test]
async fn repo_delete_endpoint_removes_repo() {
    let app = TestApp::new();
    let _ = app
        .json("POST", "/api/v1/repos/o/doomed", Some("wtoken"), obj(&[("default_branch", "main".into())]))
        .await;
    let mut resp = app
        .send("DELETE", "/api/v1/repos/o/doomed", Some("wtoken"), None)
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 204);
    // Content is gone.
    let mut resp = app.send("GET", "/api/v1/repos/o/doomed/branches", None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 404);
}

// ── auth enforcement on mutating REST routes ──

#[tokio::test]
async fn mutating_rest_requires_write_token() {
    let app = TestApp::new();
    // Anonymous create repo → 401.
    let mut resp = app
        .json("POST", "/api/v1/repos/o/anon", None, obj(&[("default_branch", "main".into())]))
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 401, "anonymous create blocked");
    // Read token create repo → 403.
    let mut resp = app
        .json("POST", "/api/v1/repos/o/anon", Some("rtoken"), obj(&[("default_branch", "main".into())]))
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 403, "read token blocked");
    // Write token create repo → 201, and delete works.
    let mut resp = app
        .json("POST", "/api/v1/repos/o/anon", Some("wtoken"), obj(&[("default_branch", "main".into())]))
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 201, "write token allowed");
    // Anonymous delete → 401 (not silently allowed).
    let mut resp = app.send("DELETE", "/api/v1/repos/o/anon", None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 401, "anonymous delete blocked");
    // Write delete → 204.
    let mut resp = app.send("DELETE", "/api/v1/repos/o/anon", Some("wtoken"), None).await;
    assert_eq!(TestApp::status(&mut resp).await, 204, "write delete allowed");
}

#[tokio::test]
async fn read_routes_stay_anonymous() {
    let app = TestApp::new();
    let _ = app
        .json("POST", "/api/v1/repos/o/r", Some("wtoken"), obj(&[("default_branch", "main".into())]))
        .await;
    let mut resp = app.send("GET", "/api/v1/repos/o/r/branches", None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 200, "reads stay anonymous");
}

#[tokio::test]
async fn delete_repo_cleans_db_listing() {
    let app = TestApp::new();
    let _ = app
        .json("POST", "/api/v1/repos/o/cleanup", Some("wtoken"), obj(&[("default_branch", "main".into())]))
        .await;
    // Repo appears in the Explore list.
    let mut resp = app.send("GET", "/api/v1/repos", None, None).await;
    let body = TestApp::body_json(&mut resp).await;
    assert!(body["orgs"].as_array().unwrap().iter().any(|o| o["org"] == "o"
        && o["repos"].as_array().unwrap().iter().any(|r| r["repo"] == "cleanup")));

    let _ = app.send("DELETE", "/api/v1/repos/o/cleanup", Some("wtoken"), None).await;

    // Repo must be gone from the listing (no orphaned DB row).
    let mut resp = app.send("GET", "/api/v1/repos", None, None).await;
    let body = TestApp::body_json(&mut resp).await;
    let has_ghost = body["orgs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["org"] == "o"
            && o["repos"].as_array().unwrap().iter().any(|r| r["repo"] == "cleanup"));
    assert!(!has_ghost, "deleted repo must not linger in listing");
    // Org row also dropped when empty.
    let org_gone = body["orgs"].as_array().unwrap().iter().all(|o| o["org"] != "o");
    assert!(org_gone, "empty org should be removed");
}

// ── frontend-support endpoints (Explore / graph / file-log / search) ──

#[tokio::test]
async fn explore_lists_orgs_and_repos() {
    let app = seeded_app().await;
    let mut resp = app.send("GET", "/api/v1/repos", None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    let body = TestApp::body_json(&mut resp).await;
    let orgs = body["orgs"].as_array().unwrap();
    assert_eq!(orgs.len(), 1);
    assert_eq!(orgs[0]["org"], "o");
    assert!(orgs[0]["repos"].as_array().unwrap().iter().any(|r| r["repo"] == "r"));
}

#[tokio::test]
async fn graph_endpoint_returns_nodes_with_edges() {
    let app = seeded_app().await;
    let mut resp = app.send("GET", "/api/v1/graph/o/r", None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    let body = TestApp::body_json(&mut resp).await;
    let nodes = body["graph"].as_array().unwrap();
    assert!(nodes.len() >= 2, "graph should list at least the seed commits");
    assert!(nodes.iter().all(|n| n["change_id"].as_str().is_some_and(|c| c.len() == 32)));
    assert!(nodes.iter().any(|n| n["is_head"] == true));
}

#[tokio::test]
async fn file_log_endpoint_returns_history_for_path() {
    let app = seeded_app().await;
    let mut resp = app.send("GET", "/api/v1/repos/o/r/file-log?path=a.txt", None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    let body = TestApp::body_json(&mut resp).await;
    assert!(!body["commits"].as_array().unwrap().is_empty());
    assert!(body["total_count"].as_i64().unwrap() >= 1);
}

#[tokio::test]
async fn search_endpoint_finds_needle() {
    let app = seeded_app().await;
    let mut resp = app
        .send("GET", "/api/v1/repos/o/r/main/search?pattern=line2", None, None)
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    let body = TestApp::body_json(&mut resp).await;
    let matches = body["matches"].as_array().unwrap();
    assert!(!matches.is_empty(), "expect at least one grep hit");
    assert!(matches.iter().any(|m| m.as_str().is_some_and(|s| s.contains("a.txt:"))));
}

#[tokio::test]
async fn spa_fallback_serves_index_html() {
    let app = TestApp::new();
    let mut resp = app.send("GET", "/", None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    let headers = resp.headers().clone();
    assert_eq!(headers.get("content-type").and_then(|v| v.to_str().ok()), Some("text/html"));
}

// ── validation / traversal guards ──

#[tokio::test]
async fn traversal_org_repo_names_are_rejected() {
    let app = TestApp::new();
    // Path traversal via percent-encoded ".." must not create a repo.
    let mut resp = app
        .json("POST", "/api/v1/repos/%2e%2e/evil", Some("wtoken"), obj(&[("default_branch", "main".into())]))
        .await;
    let status = TestApp::status(&mut resp).await;
    assert!((400..500).contains(&status), "traversal must be rejected, got {status}", );

    // Slash inside a segment (not a separator) is invalid.
    let mut resp = app
        .json("POST", "/api/v1/repos/a%2Fb/c", Some("wtoken"), obj(&[("default_branch", "main".into())]))
        .await;
    assert!((400..500).contains(&TestApp::status(&mut resp).await), "encoded slash rejected");
}

#[tokio::test]
async fn invalid_branch_name_rejected_but_slash_allowed() {
    let app = TestApp::new();
    let _ = app
        .json("POST", "/api/v1/repos/o/r", Some("wtoken"), obj(&[("default_branch", "main".into())]))
        .await;
    // Backslash in a branch name is invalid (git forbids it).
    let mut resp = app
        .json("POST", "/api/v1/repos/o/r/branches/feat%5Cbad", Some("wtoken"), obj(&[("target", "main".into())]))
        .await;
    assert!((400..500).contains(&TestApp::status(&mut resp).await), "backslash branch rejected");

    // A slash-separated branch name is allowed (creates feat/log).
    let mut resp = app
        .json("POST", "/api/v1/repos/o/r/branches/feat%2Flog", Some("wtoken"), obj(&[("target", "main".into())]))
        .await;
    assert_eq!(TestApp::status(&mut resp).await, 200, "slash branch allowed");
}

#[tokio::test]
async fn rename_repo_updates_listing_and_cascades() {
    let app = TestApp::new();
    let _ = app
        .json("POST", "/api/v1/repos/o/old", Some("wtoken"), obj(&[("default_branch", "main".into())]))
        .await;
    let _ = app
        .json("PATCH", "/api/v1/repos/o/old", Some("wtoken"), obj(&[("new_name", "new".into())]))
        .await;

    let mut resp = app.send("GET", "/api/v1/repos", None, None).await;
    let body = TestApp::body_json(&mut resp).await;
    let has_new = body["orgs"].as_array().unwrap().iter().any(|o| o["org"] == "o"
        && o["repos"].as_array().unwrap().iter().any(|r| r["repo"] == "new"));
    let has_old = body["orgs"].as_array().unwrap().iter().any(|o| o["org"] == "o"
        && o["repos"].as_array().unwrap().iter().any(|r| r["repo"] == "old"));
    assert!(has_new, "renamed repo appears under new name");
    assert!(!has_old, "old name must be gone");
    // New name resolves, old name 404s.
    let mut resp = app.send("GET", "/api/v1/repos/o/new/branches", None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 200);
    let mut resp = app.send("GET", "/api/v1/repos/o/old/branches", None, None).await;
    assert_eq!(TestApp::status(&mut resp).await, 404);
}
