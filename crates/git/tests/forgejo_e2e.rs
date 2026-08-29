//! Live end-to-end push+pull against a real Forgejo instance.
//!
//! Run explicitly (network + creds required):
//!   FORGEJO_URL=https://forgejo.develop.10.199.64.20.nip.io \
//!   FORGEJO_REPO=root/jjlab-e2e \
//!   FORGEJO_USER=root FORGEJO_PASS=devpassword \
//!   cargo test -p jjlab-git --test forgejo_e2e -- --ignored --nocapture
//!
//! The target repo must already exist (created once via the Forgejo API).

use std::path::Path;
use std::process::Command;

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

#[tokio::test]
#[ignore]
async fn forgejo_push_and_pull_roundtrip() {
    let base = match std::env::var("FORGEJO_URL") {
        Ok(v) => v.trim_end_matches('/').to_string(),
        Err(_) => {
            eprintln!("FORGEJO_URL not set; skipping");
            return;
        }
    };
    let repo = std::env::var("FORGEJO_REPO").unwrap_or_else(|_| "root/jjlab-e2e".into());
    let user = std::env::var("FORGEJO_USER").unwrap_or_else(|_| "root".into());
    let pass = std::env::var("FORGEJO_PASS").unwrap_or_default();

    // Authenticated https URL for push/fetch.
    let plain = format!("{base}/{repo}.git");
    let url = if pass.is_empty() {
        plain
    } else {
        let idx = plain.find("://").expect("valid url") + 3;
        format!("{}{}:{}@{}", &plain[..idx], user, pass, &plain[idx..])
    };

    // Seed a local upstream worktree, then use jj-lab to mirror it to Forgejo.
    let tmp = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(jjlab_git::RepoStore::new(tmp.path().join("repos")));
    let db = std::sync::Arc::new(jjlab_core::Db::open(&tmp.path().join("meta.db")).unwrap());

    let upstream = tmp.path().join("upstream");
    std::fs::create_dir_all(&upstream).unwrap();
    run_git(&upstream, &["init", "-q"]);
    run_git(&upstream, &["config", "user.name", "e2e"]);
    run_git(&upstream, &["config", "user.email", "e2e@example.com"]);
    std::fs::write(upstream.join("a.txt"), "hello from jjlab\n").unwrap();
    run_git(&upstream, &["add", "a.txt"]);
    run_git(&upstream, &["commit", "-q", "-m", "initial"]);

    // 1. Clone the seed into jj (local path), then push --mirror to Forgejo.
    jjlab_git::sync::clone_remote(&store, &db, "root", "jjlab-e2e", &upstream.to_string_lossy(), None)
        .await
        .expect("clone_remote local seed");

    jjlab_git::sync::push_mirror(&store, "root", "jjlab-e2e", &url, "")
        .await
        .expect("push_mirror to Forgejo");

    // Verify Forgejo now has the commit.
    let ls = Command::new("git")
        .arg("ls-remote")
        .arg(&url)
        .env("GIT_SSL_NO_VERIFY", "true")
        .output()
        .expect("git ls-remote");
    assert!(
        ls.status.success(),
        "ls-remote Forgejo failed: {}",
        String::from_utf8_lossy(&ls.stderr)
    );
    let remote_heads = String::from_utf8_lossy(&ls.stdout);
    let local_head = run_git_and_stdout(&upstream, &["rev-parse", "HEAD"]);
    assert!(
        remote_heads.contains(&local_head),
        "Forgejo should contain local HEAD {local_head}\ngot:\n{remote_heads}"
    );

    // 2. Round-trip: clone back from Forgejo into a fresh jj repo.
    let store2 = std::sync::Arc::new(jjlab_git::RepoStore::new(tmp.path().join("repos2")));
    let db2 = std::sync::Arc::new(jjlab_core::Db::open(&tmp.path().join("meta2.db")).unwrap());
    let head2 = jjlab_git::sync::clone_remote(&store2, &db2, "root", "roundtrip", &url, None)
        .await
        .expect("clone_remote from Forgejo");
    assert_eq!(head2, local_head, "clone-back HEAD should match");

    eprintln!("PUSH+PULL OK: mirror {} -> Forgejo {repo} and back", local_head);
}

fn run_git_and_stdout(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_SSL_NO_VERIFY", "true")
        .output()
        .expect("spawn git");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}