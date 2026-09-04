use std::path::Path;
use std::process::Command;

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_stdout(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test]
async fn ingest_local_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    run_git(&src, &["init", "-q"]);
    run_git(&src, &["config", "user.name", "test"]);
    run_git(&src, &["config", "user.email", "test@example.com"]);

    std::fs::write(src.join("a.txt"), "hello\n").unwrap();
    run_git(&src, &["add", "a.txt"]);
    run_git(&src, &["commit", "-q", "-m", "initial"]);

    std::fs::write(src.join("b.txt"), "world\n").unwrap();
    run_git(&src, &["add", "b.txt"]);
    run_git(&src, &["commit", "-q", "-m", "second"]);

    std::fs::write(
        src.join("c.txt"),
        "<<<<<<< ours\nleft side\n=======\nright side\n>>>>>>> theirs\n",
    )
    .unwrap();
    run_git(&src, &["add", "c.txt"]);
    run_git(&src, &["commit", "-q", "-m", "conflicted"]);

    let db = jjlab_core::Db::open(&tmp.path().join("meta.db")).unwrap();
    let store = std::sync::Arc::new(jjlab_git::RepoStore::new(tmp.path().join("repos")));

    let outcome = jjlab_git::ingest::ingest_bare_repo(&store, &db, "org", "repo", &src)
        .await
        .unwrap();

    assert_eq!(outcome.commits, 3);
    assert_eq!(outcome.conflicts, 1);

    // A non-trailer commit gets a deterministic 16-byte change-id (reverse hex,
    // 32 chars), derived via jj's synthetic_change_id_from_git_commit_id.
    let head = git_stdout(&src, &["rev-parse", "HEAD"]);
    let head_anchor = db.lookup_anchor("org/repo", &head).unwrap().unwrap();
    assert_eq!(head_anchor.change_id.len(), 32);
    assert!(head_anchor.commit_id.is_some());

    let second_sha = git_stdout(&src, &["rev-parse", "HEAD^"]);
    let second_anchor = db.lookup_anchor("org/repo", &second_sha).unwrap().unwrap();
    assert_eq!(second_anchor.change_id.len(), 32);
    assert_ne!(head_anchor.change_id, second_anchor.change_id);

    // Change rows are persisted.
    let cid = head_anchor.change_id.clone();
    assert!(db.get_change(&cid).unwrap().is_some());

    // The conflict-marker file was uplifted into a first-class conflict.
    let conflicts = db.list_conflicts("org/repo").unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].path, "c.txt");
    assert_eq!(conflicts[0].change_id, head_anchor.change_id);
    let adds: Vec<String> = serde_json::from_str(&conflicts[0].adds).unwrap();
    assert_eq!(adds, vec!["left side\n", "right side\n"]);

    // Every git branch ref maps to a bookmark pointing at its tip change.
    let branch = git_stdout(&src, &["symbolic-ref", "--short", "HEAD"]);
    let bm = db.get_bookmark("org/repo", &branch).unwrap();
    assert!(bm.is_some(), "bookmark should be projected");

    // Idempotent re-ingest: no new commits are created the second time.
    let second = jjlab_git::ingest::ingest_bare_repo(&store, &db, "org", "repo", &src)
        .await
        .unwrap();
    assert_eq!(second.commits, 0);
}

#[tokio::test]
async fn metadata_survives_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    run_git(&src, &["init", "-q"]);
    run_git(&src, &["config", "user.name", "t"]);
    run_git(&src, &["config", "user.email", "t@e"]);
    std::fs::write(src.join("a.txt"), "hi\n").unwrap();
    run_git(&src, &["add", "a.txt"]);
    run_git(&src, &["commit", "-q", "-m", "one"]);

    let db_path = tmp.path().join("meta.db");
    {
        let db = jjlab_core::Db::open(&db_path).unwrap();
        let store = std::sync::Arc::new(jjlab_git::RepoStore::new(tmp.path().join("repos")));
        jjlab_git::ingest::ingest_bare_repo(&store, &db, "org", "repo", &src)
            .await
            .unwrap();
    }

    // Reopen the DB and confirm bookmarks persist (restart replay).
    let db2 = jjlab_core::Db::open(&db_path).unwrap();
    let bookmarks = db2.list_bookmarks("org/repo").unwrap();
    assert_eq!(bookmarks.len(), 1);
    assert_eq!(bookmarks.len(), 1);
    assert!(!bookmarks[0].name.is_empty());
}