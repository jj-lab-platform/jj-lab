//! End-to-end bidirectional sync round-trip against a local bare git "remote".
//!
//! Proves clone → import → push → fetch-again all work through jj-lib's official
//! path (git subprocess for the network hop), with no self-researched protocol.

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

/// Build a small seed git repo (the "upstream") that our jj server will clone.
fn seed_upstream(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    run_git(path, &["init", "-q"]);
    run_git(path, &["config", "user.name", "seed"]);
    run_git(path, &["config", "user.email", "seed@example.com"]);
    std::fs::write(path.join("hello.txt"), "hello\n").unwrap();
    run_git(path, &["add", "hello.txt"]);
    run_git(path, &["commit", "-q", "-m", "initial"]);
    std::fs::write(path.join("second.txt"), "two\n").unwrap();
    run_git(path, &["add", "second.txt"]);
    run_git(path, &["commit", "-q", "-m", "second"]);
}

#[tokio::test]
async fn sync_roundtrip_local_remote() {
    let tmp = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(jjlab_git::RepoStore::new(tmp.path().join("repos")));
    let db = std::sync::Arc::new(jjlab_core::Db::open(&tmp.path().join("meta.db")).unwrap());

    // Upstream worktree + a bare "remote" mirror we clone from / push to.
    let upstream = tmp.path().join("upstream");
    seed_upstream(&upstream);
    let remote = tmp.path().join("remote.git");
    let upstream_path = upstream.as_os_str().to_string_lossy().to_string();
    let remote_path = remote.as_os_str().to_string_lossy().to_string();

    // Bare clone upstream -> remote.git with absolute paths and explicit dest.
    let out = Command::new("git")
        .arg("clone")
        .arg("--bare")
        .arg(&upstream_path)
        .arg(&remote_path)
        .output()
        .expect("spawn git clone");
    assert!(
        out.status.success(),
        "bare clone failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // 1. Clone the remote into jj via the official path.
    let head = jjlab_git::sync::clone_remote(&store, &db, "org", "repo", &remote_path, None)
        .await
        .expect("clone_remote");
    assert!(!head.is_empty(), "clone_remote should return the git HEAD");

    // 2. Push --mirror back to the same remote (jj exported refs survive).
    jjlab_git::sync::push_mirror(&store, "org", "repo", &remote_path, "")
        .await
        .expect("push_mirror");

    // 3. Advance upstream with a new commit, then incrementally fetch.
    std::fs::write(upstream.join("third.txt"), "three\n").unwrap();
    run_git(&upstream, &["add", "third.txt"]);
    run_git(&upstream, &["commit", "-q", "-m", "third"]);
    run_git(&upstream, &["push", "--mirror", remote_path.as_str()]);

    let updated = jjlab_git::sync::fetch_remote(&store, "org", "repo", "origin", &remote_path)
        .await
        .expect("fetch_remote");
    assert!(updated > 0, "expected at least one changed remote bookmark");
}