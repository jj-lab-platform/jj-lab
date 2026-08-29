//! Verify change-id stability across jj → git → Forgejo → jjlab import.
//!
//! The upstream `root/jjlab-e2e` was manually edited + pushed with `jj`; its
//! `change-id` header must survive export→git→import, so jjlab sees the exact
//! same change-id.
//!
//! Run explicitly:
//!   FORGEJO_URL=... FORGEJO_REPO=root/jjlab-e2e FORGEJO_USER=root FORGEJO_PASS=devpassword \
//!   cargo test -p jjlab-git --test changeid_roundtrip -- --ignored --nocapture

fn env_or(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_string())
}

#[tokio::test]
#[ignore]
async fn change_id_survives_roundtrip() {
    let base = std::env::var("FORGEJO_URL");
    if base.is_err() {
        eprintln!("FORGEJO_URL not set; skipping");
        return;
    }
    let base = base.unwrap().trim_end_matches('/').to_string();
    let repo = env_or("FORGEJO_REPO", "root/jjlab-e2e");
    let user = env_or("FORGEJO_USER", "root");
    let pass = std::env::var("FORGEJO_PASS").unwrap_or_default();

    let plain = format!("{base}/{repo}.git");
    let url = if pass.is_empty() {
        plain
    } else {
        let idx = plain.find("://").expect("valid url") + 3;
        format!("{}{}:{}@{}", &plain[..idx], user, pass, &plain[idx..])
    };

    let tmp = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(jjlab_git::RepoStore::new(tmp.path().join("repos")));
    let db = std::sync::Arc::new(jjlab_core::Db::open(&tmp.path().join("meta.db")).unwrap());

    jjlab_git::sync::clone_remote(&store, &db, "root", "verify", &url, None)
        .await
        .expect("clone_remote from Forgejo");

    let ids = jjlab_git::sync::list_change_ids(&store, "root", "verify")
        .await
        .expect("list_change_ids");

    // The change authored by `jj` in the manual session.
    let expected = "kmtnvpqwpkxmtonmmrpqkotpxltxrvqz";
    assert!(
        ids.iter().any(|c| c == expected),
        "expected change-id {expected} among imported changes, got {ids:?}"
    );

    eprintln!("CHANGE-ID OK: {expected} survives jj→git→Forgejo→jjlab import");
}
