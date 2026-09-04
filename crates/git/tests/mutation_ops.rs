//! Integration tests for the write surface (mutation.rs): repo lifecycle,
//! bookmark/tag ops, and content edits producing changes. All against a local
//! bare-git remote (no network), consistent with the sync tests.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use jj_lib::object_id::ObjectId as _;
use jj_lib::repo::Repo as _;

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_SSL_NO_VERIFY", "true")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}


const AUTHOR: (&str, &str) = ("tester", "t@example.com");

fn author() -> (String, String) {
    (AUTHOR.0.to_string(), AUTHOR.1.to_string())
}

/// Seed an upstream repo with one commit and expose it as a bare remote.
fn seed_remote(tmp: &Path) -> (Arc<jjlab_git::RepoStore>, Arc<jjlab_core::Db>, String) {
    let upstream = tmp.join("upstream");
    std::fs::create_dir_all(&upstream).unwrap();
    run_git(&upstream, &["init", "-q"]);
    run_git(&upstream, &["config", "user.name", "seed"]);
    run_git(&upstream, &["config", "user.email", "s@s"]);
    std::fs::write(upstream.join("seed.txt"), "seed\n").unwrap();
    run_git(&upstream, &["add", "."]);
    run_git(&upstream, &["commit", "-q", "-m", "seed"]);

    let remote = tmp.join("remote.git");
    run_git(
        tmp,
        &["clone", "--bare", "upstream", &remote.to_string_lossy()],
    );

    let store = Arc::new(jjlab_git::RepoStore::new(tmp.join("repos")));
    let db = Arc::new(jjlab_core::Db::open(&tmp.join("meta.db")).unwrap());
    let url = remote.to_string_lossy().to_string();
    (store, db, url)
}

#[tokio::test]
async fn repo_lifecycle_create_write_branch_tag_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, db, _url) = seed_remote(tmp.path());

    // 1. Create repo (fresh, no clone needed).
    jjlab_git::mutation::create_repo(&store, &db, "o", "r", "main", author())
        .await
        .expect("create_repo");
    assert!(store.exists("o", "r"));
    // Duplicate create is rejected.
    assert!(jjlab_git::mutation::create_repo(&store, &db, "o", "r", "main", author())
        .await
        .is_err());

    // 2. Write a file → new change on main.
    let write = |path: String, content: Vec<u8>| {
        let s2 = store.clone(); let d2 = db.clone();
        async move {
            let edits = vec![jjlab_git::mutation::BatchEdit { path, content, base_sha: None }];
            jjlab_git::mutation::commit_edits(&s2, &d2, "o", "r", "main", &edits, &[], "add", author(), false).await
        }
    };
    let out = write("docs/a.md".to_string(), b"# hi\n".to_vec()).await.expect("commit_edits write");
    assert!(!out.sha.is_empty());
    assert_eq!(out.change_id.len(), 32);

    // Content is readable at main's tip.
    let tip = jjlab_git::read::head_sha(&store, "o", "r").await.unwrap();
    let bytes = jjlab_git::read::read_file_at(&store, "o", "r", &tip, "docs/a.md")
        .await
        .unwrap();
    assert_eq!(bytes, b"# hi\n");

    // 3. Update produces a *new* commit + change (amend=false).
    let out2 = write("docs/a.md".to_string(), b"# hi v2\n".to_vec()).await.unwrap();
    assert_ne!(out.sha, out2.sha);
    assert_ne!(out.change_id, out2.change_id);

    // 4. Branch ops: create at main's tip, then delete.
    let sha = jjlab_git::mutation::set_bookmark(&store, &db, "o", "r", "feature", "main", "")
        .await
        .unwrap();
    let bookmarks = jjlab_git::read::bookmarks(&store, "o", "r").await.unwrap();
    assert!(bookmarks.iter().any(|b| b.name == "feature" && b.sha == sha));
    jjlab_git::mutation::delete_bookmark(&store, &db, "o", "r", "feature")
        .await
        .unwrap();
    let bookmarks = jjlab_git::read::bookmarks(&store, "o", "r").await.unwrap();
    assert!(!bookmarks.iter().any(|b| b.name == "feature"));

    // 5. Tag ops.
    let tsha = jjlab_git::mutation::set_tag(&store, &db, "o", "r", "v1", "main", "")
        .await
        .unwrap();
    let tags = jjlab_git::read::tags(&store, "o", "r").await.unwrap();
    assert!(tags.iter().any(|t| t.name == "v1" && t.sha == tsha));
    jjlab_git::mutation::delete_tag(&store, &db, "o", "r", "v1")
        .await
        .unwrap();
    assert!(jjlab_git::read::tags(&store, "o", "r").await.unwrap().is_empty());

    // 6. Delete file → change; content 404s after.
    let dels = vec![jjlab_git::mutation::BatchDelete { path: "docs/a.md".to_string(), base_sha: None }];
    jjlab_git::mutation::commit_edits(&store, &db, "o", "r", "main", &[], &dels, "rm", author(), false)
        .await
        .unwrap();
    let tip = jjlab_git::read::head_sha(&store, "o", "r").await.unwrap();
    assert!(jjlab_git::read::read_file_at(&store, "o", "r", &tip, "docs/a.md")
        .await
        .is_err());

    // 7. Delete repo removes the tree.
    jjlab_git::mutation::delete_repo(&store, "o", "r").await.unwrap();
    assert!(!store.exists("o", "r"));
}

#[tokio::test]
async fn write_file_to_missing_branch_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, db, _url) = seed_remote(tmp.path());
    jjlab_git::mutation::create_repo(&store, &db, "o", "r", "main", author())
        .await
        .unwrap();
    let edits = vec![jjlab_git::mutation::BatchEdit { path: "f.txt".to_string(), content: b"x".to_vec(), base_sha: None }];
    let err = jjlab_git::mutation::commit_edits(&store, &db, "o", "r", "nope", &edits, &[], "m", author(), false)
        .await;
    assert!(err.is_err(), "writing to a non-existent bookmark must fail");
}

#[tokio::test]
async fn write_file_with_amend_keeps_change_id() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, db, _url) = seed_remote(tmp.path());
    jjlab_git::mutation::create_repo(&store, &db, "o", "r", "main", author())
        .await
        .unwrap();
    let first = jjlab_git::mutation::commit_edits(
        &store, &db, "o", "r", "main",
        &vec![jjlab_git::mutation::BatchEdit { path: "f.txt".to_string(), content: b"v1\n".to_vec(), base_sha: None }],
        &[], "m1", author(), false,
    ).await.unwrap();
    // Amend the head change with new content: change-id must survive, and the
    // head commit must be *rewritten* (not extended with a same-id child).
    let amended = jjlab_git::mutation::commit_edits(
        &store, &db, "o", "r", "main",
        &vec![jjlab_git::mutation::BatchEdit { path: "f.txt".to_string(), content: b"v2\n".to_vec(), base_sha: None }],
        &[], "m2", author(), true,
    ).await.unwrap();
    assert_eq!(
        first.change_id, amended.change_id,
        "amend must keep the stable change-id"
    );
    assert_ne!(first.sha, amended.sha);
}

#[tokio::test]
async fn amend_rewrites_head_not_extends() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, db, _url) = seed_remote(tmp.path());
    jjlab_git::mutation::create_repo(&store, &db, "o", "r", "main", author())
        .await
        .unwrap();
    let first = jjlab_git::mutation::commit_edits(
        &store, &db, "o", "r", "main",
        &vec![jjlab_git::mutation::BatchEdit { path: "f.txt".to_string(), content: b"v1\n".to_vec(), base_sha: None }],
        &[], "m1", author(), false,
    ).await.unwrap();
    let amended = jjlab_git::mutation::commit_edits(
        &store, &db, "o", "r", "main",
        &vec![jjlab_git::mutation::BatchEdit { path: "f.txt".to_string(), content: b"v2\n".to_vec(), base_sha: None }],
        &[], "m2", author(), true,
    ).await.unwrap();
    // The rewritten head must keep the ORIGINAL parent (not the old head).
    let repo = jjlab_git::read::open(&store, "o", "r").await.unwrap();
    let id = jjlab_git::read::resolve_snapshot(&repo, "main").unwrap();
    let commit = repo.store().get_commit(&id).unwrap();
    let parent_ids: Vec<String> = commit.parent_ids().iter().map(|p| p.hex()).collect();
    assert!(
        !parent_ids.contains(&first.sha),
        "amend must rewrite the head in place (no same-change-id child), parents={parent_ids:?}"
    );
    assert_eq!(commit.change_id().reverse_hex(), first.change_id);
    let _ = amended;
}

#[tokio::test]
async fn amend_on_root_falls_back_to_new_change() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, db, _url) = seed_remote(tmp.path());
    jjlab_git::mutation::create_repo(&store, &db, "o", "r", "main", author())
        .await
        .unwrap();
    // The only commit on a fresh repo is the init commit; its parent is the
    // root commit, so amend must degrade to a new change without panicking.
    let out = jjlab_git::mutation::commit_edits(
        &store, &db, "o", "r", "main",
        &vec![jjlab_git::mutation::BatchEdit { path: "f.txt".to_string(), content: b"v1\n".to_vec(), base_sha: None }],
        &[], "m1", author(), true,
    ).await.unwrap();
    assert_eq!(out.change_id.len(), 32);
}

#[tokio::test]
async fn delete_repo_missing_is_404() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, _db, _url) = seed_remote(tmp.path());
    let err = jjlab_git::mutation::delete_repo(&store, "o", "ghost").await;
    assert!(err.is_err());
}

// ── projection (project.rs) ──

#[tokio::test]
async fn projection_populates_bookmarks_and_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, db, url) = seed_remote(tmp.path());
    jjlab_git::sync::clone_remote(&store, &db, "o", "r", &url, None)
        .await
        .unwrap();
    // Clone itself projects; verify DB rows match the jj view.
    let bookmarks = jjlab_git::read::bookmarks(&store, "o", "r").await.unwrap();
    assert!(!bookmarks.is_empty(), "clone must import at least one bookmark");
    for b in &bookmarks {
        let bm = db.get_bookmark("o/r", &b.name).unwrap();
        assert!(bm.is_some(), "bookmark {} should be projected", b.name);
    }
    let ids = jjlab_git::sync::list_change_ids(&store, "o", "r").await.unwrap();
    assert!(!ids.is_empty());
    // Every change-id row is present in changes table.
    for cid in &ids {
        assert!(db.get_change(cid).unwrap().is_some(), "change {cid} projected");
    }
}

#[tokio::test]
async fn force_push_reassociates_open_mr_head() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, db, url) = seed_remote(tmp.path());
    jjlab_git::sync::clone_remote(&store, &db, "o", "r", &url, None).await.unwrap();

    // Push a feature bookmark via the mutation path (simulates receive-pack):
    // write a file on main, then point a bookmark at a rewritten tip.
    let bookmarks = jjlab_git::read::bookmarks(&store, "o", "r").await.unwrap();
    let base_name = bookmarks.first().unwrap().name.clone();
    let base_sha = bookmarks.first().unwrap().sha.clone();
    let first = jjlab_git::mutation::commit_edits(
        &store, &db, "o", "r", &base_name,
        &vec![jjlab_git::mutation::BatchEdit { path: "f.txt".to_string(), content: b"v1\n".to_vec(), base_sha: None }],
        &[], "one", author(), false,
    ).await.unwrap();
    assert_ne!(first.sha, base_sha, "write must advance the base bookmark");
    jjlab_git::mutation::set_bookmark(&store, &db, "o", "r", "feature", &first.sha, "")
        .await
        .unwrap();

    // Open an MR against the feature bookmark (bookmark-name association).
    let repo_id = "o/r";
    let _ = db.upsert_org("o", "o");
    let _ = db.upsert_repo(repo_id, "o", "r", "main", None);
    let mr = db
        .create_mr(repo_id, "t", "", "a", &first.change_id, Some(&first.sha), Some("feature"), &base_name)
        .unwrap();

    // Force-push: rewrite the tip (new change-id, same bookmark name) — the
    // plain-git case where no change-id header survives.
    let second = jjlab_git::mutation::commit_edits(
        &store, &db, "o", "r", "feature",
        &vec![jjlab_git::mutation::BatchEdit { path: "f.txt".to_string(), content: b"v2\n".to_vec(), base_sha: None }],
        &[], "two", author(), false,
    ).await.unwrap();
    let _ = base_sha;

    // Re-run projection: MR head must follow the bookmark tip.
    jjlab_git::project::project_repo(&store, &db, "o", "r")
        .await
        .unwrap();
    let updated = db.get_mr_by_number(repo_id, mr.number).unwrap().unwrap();
    assert_eq!(
        updated.head_sha.as_deref(),
        Some(second.sha.as_str()),
        "MR head must follow force-pushed bookmark tip"
    );
}

#[tokio::test]
async fn delete_bookmark_refuses_to_remove_last_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, db, _url) = seed_remote(tmp.path());
    jjlab_git::mutation::create_repo(&store, &db, "o2", "r2", "main", author())
        .await
        .expect("create_repo");

    // Repo has exactly one local bookmark: main. Deleting it must be rejected
    // so a repo is never left without a branch.
    let err = jjlab_git::mutation::delete_bookmark(&store, &db, "o2", "r2", "main")
        .await
        .expect_err("deleting the last bookmark must fail");
    assert!(
        format!("{err:?}").contains("at least one bookmark"),
        "unexpected error: {err:?}"
    );

    // main still present.
    let bookmarks = jjlab_git::read::bookmarks(&store, "o2", "r2").await.unwrap();
    assert!(bookmarks.iter().any(|b| b.name == "main"));
}
