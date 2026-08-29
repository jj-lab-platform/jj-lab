//! Unit tests for op-log event helpers (ops.rs) — pure logic, no jj repo.

#[test]
fn ops_since_returns_only_rows_after_marker() {
    let dir = tempfile::tempdir().unwrap();
    let db = jjlab_core::Db::open(&dir.path().join("m.db")).unwrap();
    db.upsert_org("o", "o").unwrap();
    db.upsert_repo("o/r", "o", "r", "main", None).unwrap();
    for i in 0..4 {
        db.append_op_log(&jjlab_core::db::OpLogRow {
            id: format!("op-{i}"),
            repo_id: "o/r".into(),
            op_type: "test".into(),
            payload: format!("{i}"),
            undo_of: None,
        })
        .unwrap();
    }

    // Everything when after is unknown.
    let all = jjlab_git::ops::ops_since(&db, "o/r", "nope");
    assert_eq!(all.len(), 4);

    // Only rows after op-1 (exclusive).
    let tail = jjlab_git::ops::ops_since(&db, "o/r", "op-1");
    let ids: Vec<&str> = tail.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["op-2", "op-3"]);

    // Nothing after the last op.
    assert!(jjlab_git::ops::ops_since(&db, "o/r", "op-3").is_empty());
}

#[test]
fn ops_since_handles_missing_repo() {
    let dir = tempfile::tempdir().unwrap();
    let db = jjlab_core::Db::open(&dir.path().join("m.db")).unwrap();
    // Repo row absent → list_op_log still returns empty vec (not Err).
    assert!(jjlab_git::ops::ops_since(&db, "x/y", "anything").is_empty());
}

#[test]
fn sse_frame_format() {
    let ev = jjlab_git::ops::OpEvent {
        id: "op-1".into(),
        repo_id: "o/r".into(),
        op_type: "project".into(),
        payload: "{}".into(),
        undo_of: None,
    };
    let frame = jjlab_git::ops::sse_frame(&ev);
    assert!(frame.starts_with("id: op-1\n"));
    assert!(frame.contains("event: op\n"));
    assert!(frame.contains(r#"data: {"id":"op-1""#));
    assert!(frame.ends_with("\n\n"));
}

#[tokio::test]
async fn git_dir_of_resolves_internal_and_errors_on_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(jjlab_git::RepoStore::new(tmp.path().join("repos")));
    // Create an internal-git repo via mutation (creates .jj/repo/store/git).
    jjlab_git::mutation::create_repo(
        &store,
        &jjlab_core::Db::open(&tmp.path().join("m.db")).unwrap(),
        "o",
        "r",
        "main",
        ("a".into(), "a@a".into()),
    )
    .await
    .unwrap();
    let dir = jjlab_git::http::git_dir_of(&store, "o", "r").unwrap();
    assert!(dir.join("HEAD").exists(), "resolved git dir must be a git repo");
    // Missing repo → error.
    assert!(jjlab_git::http::git_dir_of(&store, "o", "ghost").is_err());
}

#[test]
fn embed_credentials_variants() {
    // No secret → unchanged.
    assert_eq!(
        jjlab_git::sync::embed_credentials("https://host/x", ""),
        "https://host/x"
    );
    // Secret injected after scheme.
    assert_eq!(
        jjlab_git::sync::embed_credentials("https://host/x", "u:p"),
        "https://u:p@host/x"
    );
    // Existing credentials preserved.
    assert_eq!(
        jjlab_git::sync::embed_credentials("https://a:b@host/x", "u:p"),
        "https://a:b@host/x"
    );
    // Non-URL left alone.
    assert_eq!(jjlab_git::sync::embed_credentials("/local/path", "u:p"), "/local/path");
}

#[test]
fn normalize_url_strips_credentials_and_trailing_slash() {
    assert_eq!(
        jjlab_git::sync::normalize_url("https://u:p@host/repo/"),
        "https://host/repo"
    );
    assert_eq!(jjlab_git::sync::normalize_url("https://host/repo"), "https://host/repo");
    assert_eq!(jjlab_git::sync::normalize_url("/local/path"), "/local/path");
}

#[tokio::test]
async fn push_mirror_to_missing_repo_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(jjlab_git::RepoStore::new(tmp.path().join("repos")));
    assert!(jjlab_git::sync::push_mirror(&store, "o", "ghost", "/tmp/x", "").await.is_err());
}

#[tokio::test]
async fn fetch_remote_missing_repo_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(jjlab_git::RepoStore::new(tmp.path().join("repos")));
    assert!(
        jjlab_git::sync::fetch_remote(&store, "o", "ghost", "origin", "/tmp/x")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn clone_remote_refuses_existing_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(jjlab_git::RepoStore::new(tmp.path().join("repos")));
    let db = std::sync::Arc::new(jjlab_core::Db::open(&tmp.path().join("m.db")).unwrap());
    std::fs::create_dir_all(store.repo_dir("o", "taken")).unwrap();
    assert!(
        jjlab_git::sync::clone_remote(&store, &db, "o", "taken", "/tmp/whatever", None)
            .await
            .is_err()
    );
}
