//! Bidirectional Git sync (ADR-008-revision): pull and push reuse jj-lib's
//! official path exactly as `jj git fetch` / `jj git push` do.
//!
//! - Clone:  `git clone --bare` → `Workspace::init_external_git` → `import_refs`
//! - Fetch:  `add_remote` → `GitFetch::fetch` (git fetch) → `import_refs`
//! - Push:   `export_refs` → `git push --mirror` (jj is the sole source of truth)
//!
//! The only network transport is the `git` binary (jj-lib has no pure-Rust
//! send-pack); all object/change/tree semantics stay inside jj-lib. There is no
//! self-researched Git protocol here.

use std::collections::HashMap;
use std::io;
use std::process::Command;
use std::sync::Arc;

use jj_lib::git::{
    export_refs, expand_fetch_refspecs, add_remote, import_refs, GitFetch, GitFetchRefExpression,
    GitImportOptions, GitSettings, GitSubprocessCallback, GitSubprocessOptions, GitProgress,
    GitSidebandLineTerminator,
};
use jj_lib::repo::Repo;
use jj_lib::str_util::StringExpression;

use crate::repo::{RepoError, RepoStore};
use crate::settings;

/// Validate a sync URL. `http`/`https` must not resolve to a private,
/// loopback, link-local, or cloud-metadata host (SSRF guard); `file`, other
/// schemes, and plain local paths are treated as operator-controlled local
/// imports and are allowed (they require a write token anyway).
pub fn validate_url(url: &str) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    let Some((scheme, rest)) = lower.split_once("://") else {
        // No scheme (local path or SCP-style remote) — operator-controlled.
        return Ok(());
    };
    match scheme {
        "http" | "https" => {}
        _ => return Ok(()), // file:, ssh:, etc. — not network SSRF we guard here.
    }
    let host_port = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = host_port.rsplit('@').next().unwrap_or(host_port);
    let host = host_port
        .strip_prefix('[')
        .and_then(|h| h.split(']').next())
        .unwrap_or_else(|| host_port.split(':').next().unwrap_or(""))
        .to_ascii_lowercase();
    if host.is_empty() {
        return Err("url has no host".to_string());
    }
    if is_private_host(&host) {
        return Err(format!("url host {host} is not allowed"));
    }
    Ok(())
}

fn is_private_host(host: &str) -> bool {
    // IPv4.
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        if ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_broadcast() || ip.is_documentation() || ip.is_unspecified() {
            return true;
        }
    }
    // IPv6.
    if let Ok(ip) = host.parse::<std::net::Ipv6Addr>() {
        if ip.is_loopback() || ip.is_unspecified() {
            return true;
        }
    }
    // Hostnames: block special registry-host meta names and known cloud
    // metadata hostnames, plus any bare `.local`/`.internal`.
    let lower = host.to_ascii_lowercase();
    for suffix in [
        ".local",
        ".internal",
        ".localhost",
        ".lan",
        ".home.arpa",
        "metadata.google.internal",
        "169.254.169.254",
    ] {
        if lower == suffix || lower.ends_with(suffix) {
            return true;
        }
    }
    false
}

/// Read `GIT_SSL_NO_VERIFY` from env; default secure (verify on).
fn git_ssl_no_verify() -> bool {
    std::env::var("JJLAB_GIT_INSECURE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// A callback that discards progress and forwards sideband lines to tracing.
pub struct NoopCallback;

impl GitSubprocessCallback for NoopCallback {
    fn needs_progress(&self) -> bool {
        false
    }

    fn progress(&mut self, _progress: &GitProgress) -> io::Result<()> {
        Ok(())
    }

    fn local_sideband(
        &mut self,
        message: &[u8],
        _term: Option<GitSidebandLineTerminator>,
    ) -> io::Result<()> {
        let line = String::from_utf8_lossy(message).trim_end().to_string();
        if !line.is_empty() {
            tracing::info!("git: {line}");
        }
        Ok(())
    }

    fn remote_sideband(
        &mut self,
        message: &[u8],
        _term: Option<GitSidebandLineTerminator>,
    ) -> io::Result<()> {
        let line = String::from_utf8_lossy(message).trim_end().to_string();
        if !line.is_empty() {
            tracing::info!("remote: {line}");
        }
        Ok(())
    }
}

pub fn git_import_options(settings: &jj_lib::settings::UserSettings) -> Result<GitImportOptions, RepoError> {
    let git_settings = GitSettings::from_settings(settings)
        .map_err(|e| RepoError::Other(format!("git settings: {e}")))?;
    Ok(GitImportOptions {
        abandon_unreachable_commits: git_settings.abandon_unreachable_commits,
        record_synthetic_predecessors: git_settings.record_synthetic_predecessors,
        // Track all remote bookmarks so imports materialize as local
        // bookmarks (git-clone semantics for a hosting server).
        remote_auto_track_bookmarks: HashMap::from([(
            jj_lib::ref_name::RemoteNameBuf::from(
                jj_lib::git::REMOTE_NAME_FOR_LOCAL_GIT_REPO.as_str().to_string(),
            ),
            jj_lib::str_util::StringMatcher::all(),
        )]),
    })
}

/// Clone a remote git URL into a fresh jj repository (`jh` external-git
/// workspace). Returns the git HEAD hex of the cloned remote.
pub async fn clone_remote(
    store: &Arc<RepoStore>,
    db: &jjlab_core::Db,
    org: &str,
    repo: &str,
    url: &str,
    bookmark: Option<&str>,
) -> Result<String, RepoError> {
    jjlab_core::validate_segment(org, "org").map_err(RepoError::Invalid)?;
    jjlab_core::validate_segment(repo, "repo").map_err(RepoError::Invalid)?;
    validate_url(url).map_err(RepoError::Invalid)?;
    let dir = store.repo_dir_checked(org, repo)?;
    if dir.exists() {
        return Err(RepoError::Conflict(format!("repository {org}/{repo} already exists")));
    }
    std::fs::create_dir_all(&dir)?;

    // Clone into a staging sibling then rename into place, so a failed clone
    // never leaves a half-initialized repo at the final path.
    let staging = dir.with_extension("clone-staging");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;

    let head = {
        let mut cmd = Command::new("git");
        cmd.arg("clone").arg("--bare");
        if let Some(bookmark) = bookmark {
            cmd.arg("--branch").arg(bookmark);
        }
        cmd.arg(url).arg(&staging);
        if git_ssl_no_verify() {
            cmd.env("GIT_SSL_NO_VERIFY", "true");
        }
        let out = cmd.output().map_err(|e| RepoError::Other(format!("git clone: {e}")))?;
        if !out.status.success() {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(RepoError::Other(format!(
                "git clone {url}: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }

        let head_out = Command::new("git")
            .arg("--git-dir")
            .arg(&staging)
            .arg("rev-parse")
            .arg("HEAD")
            .output()
            .map_err(|e| RepoError::Other(format!("git rev-parse: {e}")))?;
        if !head_out.status.success() {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(RepoError::Other(format!(
                "git rev-parse: {}",
                String::from_utf8_lossy(&head_out.stderr)
            )));
        }
        String::from_utf8_lossy(&head_out.stdout).trim().to_string()
    };

    let settings = settings::user_settings().map_err(RepoError::Other)?;

    pollster::block_on(async {
        let (_workspace, repo_arc) = jj_lib::workspace::Workspace::init_external_git(
            &settings,
            &staging,
            &staging,
        )
        .await
        .map_err(|e| RepoError::Other(format!("jj init on clone: {e}")))?;

        let mut tx = repo_arc.start_transaction();
        {
            let mut_repo = tx.repo_mut();
            let options = git_import_options(&settings)?;
            import_refs(mut_repo, &options)
                .await
                .map_err(|e| RepoError::Other(format!("import refs: {e}")))?;

            if let Some(bookmark) = bookmark {
                let name: jj_lib::ref_name::RefNameBuf = bookmark.to_string().into();
                if mut_repo.get_local_bookmark(&name).is_absent() {
                    let symbol = jj_lib::ref_name::RemoteRefSymbolBuf {
                        name: bookmark.to_string().into(),
                        remote: jj_lib::git::REMOTE_NAME_FOR_LOCAL_GIT_REPO.to_owned(),
                    };
                    let remote_ref = mut_repo.get_remote_bookmark(symbol.as_ref());
                    if let Some(id) = remote_ref.target.as_normal() {
                        mut_repo.set_local_bookmark_target(
                            &name,
                            jj_lib::op_store::RefTarget::normal(id.clone()),
                        );
                    }
                }
            }
        }
        tx.commit("jjlab: clone").await.map_err(|e| RepoError::Other(e.to_string()))?;
        Ok::<(), RepoError>(())
    })?;

    // Promote the fully-initialized staging dir into place.
    std::fs::rename(&staging, &dir)?;
    crate::project::project_repo(store, db, org, repo).await?;
    Ok(head)
}

/// Incremental fetch from a configured remote, then import the new refs.
///
/// Adds the remote if not already present. After this, remote bookmarks
/// (`refs/remotes/<remote>/...`) and their commits are reflected in the jj repo.
pub async fn fetch_remote(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    remote: &str,
    url: &str,
) -> Result<usize, RepoError> {
    validate_url(url).map_err(RepoError::Invalid)?;
    let handle = store.open(org, repo).await?;
    let settings = settings::user_settings().map_err(RepoError::Other)?;
    let remote_name: jj_lib::ref_name::RemoteNameBuf = remote.to_string().into();

    pollster::block_on(async {
        let mut tx = handle.repo.start_transaction();
        let subprocess_options = GitSubprocessOptions::from_settings(&settings)
            .map_err(|e| RepoError::Other(e.to_string()))?;

        {
            let mut_repo = tx.repo_mut();
            // Ensure the remote exists in the git config.
            let git_repo = jj_lib::git::get_git_repo(mut_repo.store())
                .map_err(|e| RepoError::Other(e.to_string()))?;
            let remote_there = match git_repo.try_find_remote(remote_name.as_str()) {
                Some(Ok(_)) => true,
                Some(Err(e)) => return Err(RepoError::Other(e.to_string())),
                None => false,
            };
            if !remote_there {
                add_remote(mut_repo, &remote_name, url, None)
                    .map_err(|e| RepoError::Other(e.to_string()))?;
            }
        }

        let import_options = git_import_options(&settings)?;
        let mut git_fetch = GitFetch::new(tx.repo_mut(), subprocess_options, &import_options)
            .map_err(|e| RepoError::Other(e.to_string()))?;

        let ref_expr = GitFetchRefExpression {
            bookmark: StringExpression::all(),
            tag: StringExpression::all(),
        };
        let expanded = expand_fetch_refspecs(&remote_name, ref_expr)
            .map_err(|e| RepoError::Other(e.to_string()))?;

        let mut callback = NoopCallback;
        git_fetch
            .fetch(&remote_name, expanded, &mut callback, None)
            .map_err(|e| RepoError::Other(e.to_string()))?;
        let stats = git_fetch
            .import_refs()
            .await
            .map_err(|e| RepoError::Other(e.to_string()))?;

        let updated = stats.changed_remote_bookmarks.len();
        tx.commit("jjlab: fetch").await.map_err(|e| RepoError::Other(e.to_string()))?;
        Ok::<usize, RepoError>(updated)
    })
}

/// Pull a mirror remote: `git fetch` with `--prune`, then import refs and
/// drop local bookmarks that no longer exist on the remote (mirror semantics —
/// the remote is the sole source of truth). `remote` names the git remote,
/// `url` is its fetch URL.
pub async fn pull_mirror(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    remote: &str,
    url: &str,
) -> Result<usize, RepoError> {
    validate_url(url).map_err(RepoError::Invalid)?;
    let handle = store.open(org, repo).await?;
    let settings = settings::user_settings().map_err(RepoError::Other)?;
    let remote_name: jj_lib::ref_name::RemoteNameBuf = remote.to_string().into();

    pollster::block_on(async {
        let mut tx = handle.repo.start_transaction();
        let subprocess_options = GitSubprocessOptions::from_settings(&settings)
            .map_err(|e| RepoError::Other(e.to_string()))?;

        {
            let mut_repo = tx.repo_mut();
            let git_repo = jj_lib::git::get_git_repo(mut_repo.store())
                .map_err(|e| RepoError::Other(e.to_string()))?;
            let remote_there = match git_repo.try_find_remote(remote_name.as_str()) {
                Some(Ok(_)) => true,
                Some(Err(e)) => return Err(RepoError::Other(e.to_string())),
                None => false,
            };
            if !remote_there {
                add_remote(mut_repo, &remote_name, url, None)
                    .map_err(|e| RepoError::Other(e.to_string()))?;
            }
        }

        let import_options = git_import_options(&settings)?;
        let mut git_fetch = GitFetch::new(tx.repo_mut(), subprocess_options, &import_options)
            .map_err(|e| RepoError::Other(e.to_string()))?;

        let ref_expr = GitFetchRefExpression {
            bookmark: StringExpression::all(),
            tag: StringExpression::all(),
        };
        let expanded = expand_fetch_refspecs(&remote_name, ref_expr)
            .map_err(|e| RepoError::Other(e.to_string()))?;

        let mut callback = NoopCallback;
        // `GitFetch::fetch` already runs `git fetch --prune`; the resulting
        // import (import_refs) then prunes remote bookmarks/tags that vanished
        // on the remote, so the local view mirrors the remote.
        git_fetch
            .fetch(&remote_name, expanded, &mut callback, None)
            .map_err(|e| RepoError::Other(e.to_string()))?;
        let stats = git_fetch
            .import_refs()
            .await
            .map_err(|e| RepoError::Other(e.to_string()))?;

        let updated = stats.changed_remote_bookmarks.len();
        tx.commit("jjlab: pull-mirror")
            .await
            .map_err(|e| RepoError::Other(e.to_string()))?;
        Ok::<usize, RepoError>(updated)
    })
}

/// Push all bookmarks/tags to a mirror remote (`git push --mirror`).
///
/// jj is the sole source of truth: local refs are synced and remote refs absent
/// locally are pruned. `secret`, if non-empty, is embedded into the URL as
/// basic-auth credentials.
pub async fn push_mirror(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
    mirror_url: &str,
    secret: &str,
) -> Result<(), RepoError> {
    validate_url(mirror_url).map_err(RepoError::Invalid)?;
    let push_url = embed_credentials(mirror_url, secret);
    let handle = store.open(org, repo).await?;

    pollster::block_on(async {
        let mut tx = handle.repo.start_transaction();
        {
            let mut_repo = tx.repo_mut();
            export_refs(mut_repo).map_err(|e| RepoError::Other(format!("export refs: {e}")))?;
        }
        tx.commit("jjlab: mirror export")
            .await
            .map_err(|e| RepoError::Other(e.to_string()))?;
        Ok::<(), RepoError>(())
    })?;

    let git_dir = {
        let git_repo = jj_lib::git::get_git_repo(handle.repo.store())
            .map_err(|e| RepoError::Other(format!("get git repo: {e}")))?;
        git_repo.path().to_owned()
    };
    let mut cmd = Command::new("git");
    cmd.arg("--git-dir").arg(&git_dir).arg("push").arg("--mirror");
    if git_ssl_no_verify() {
        cmd.env("GIT_SSL_NO_VERIFY", "true");
    }
    cmd.arg(&push_url);
    let out = cmd.output().map_err(|e| RepoError::Other(format!("git push: {e}")))?;
    if !out.status.success() {
        return Err(RepoError::Other(format!(
            "git push -> {push_url}: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

/// Inject `user:pass@` into an https URL if not already present.
pub fn embed_credentials(url: &str, secret: &str) -> String {
    if secret.is_empty() {
        return url.to_string();
    }
    if let Some(scheme_end) = url.find("://") {
        let idx = scheme_end + 3;
        if url[idx..].contains('@') {
            return url.to_string();
        }
        return format!("{}{}@{}", &url[..idx], secret, &url[idx..]);
    }
    url.to_string()
}

/// Normalize a mirror URL: strip embedded credentials for same-remote comparison.
pub fn normalize_url(url: &str) -> String {
    let no_cred = if let Some(scheme_end) = url.find("://") {
        let idx = scheme_end + 3;
        if let Some(at) = url[idx..].find('@') {
            let at = idx + at;
            format!("{}{}", &url[..idx], &url[at + 1..])
        } else {
            url.to_string()
        }
    } else {
        url.to_string()
    };
    no_cred.trim_end_matches('/').to_string()
}

/// Enumerate the change-id (reverse-hex) of every reachable commit in a repo,
/// for round-trip change-id stability checks.
pub async fn list_change_ids(
    store: &Arc<RepoStore>,
    org: &str,
    repo: &str,
) -> Result<Vec<String>, RepoError> {
    let handle = store.open(org, repo).await?;
    let repo_arc = handle.repo.clone();
    let ids = tokio::task::spawn_blocking(move || -> Result<Vec<String>, RepoError> {
        let mut out = Vec::new();
        let mut stack: Vec<jj_lib::backend::CommitId> = repo_arc.view().heads().iter().cloned().collect();
        let mut seen = std::collections::HashSet::new();
        while let Some(id) = stack.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Ok(commit) = repo_arc.store().get_commit(&id) {
                out.push(commit.change_id().reverse_hex());
                for p in commit.parent_ids() {
                    stack.push(p.clone());
                }
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    })
    .await
    .map_err(|e| RepoError::Other(e.to_string()))??;
    Ok(ids)
}

/// Import refs after an inbound receive-pack, so pushed commits become native
/// jj changes. Also projects the view into SQLite metadata.
pub async fn import_after_receive(
    store: &Arc<RepoStore>,
    db: &jjlab_core::Db,
    org: &str,
    repo: &str,
) -> Result<(), RepoError> {
    let handle = store.open(org, repo).await?;
    let settings = settings::user_settings().map_err(RepoError::Other)?;
    pollster::block_on(async {
        let mut tx = handle.repo.start_transaction();
        {
            let mut_repo = tx.repo_mut();
            let options = git_import_options(&settings)?;
            import_refs(mut_repo, &options)
                .await
                .map_err(|e| RepoError::Other(e.to_string()))?;
            // Importing existing history rewrites commits (synthetic
            // predecessors / abandon); those rewrites' descendants must be
            // rebased before commit, or jj's transaction consistency assert
            // panics ("Descendants have not been rebased"). rebase_descendants
            // folds those rewrites into the view so commit() succeeds. Errors
            // here surface as a 500 (never silently swallowed upstream).
            mut_repo
                .rebase_descendants()
                .await
                .map_err(|e| RepoError::Other(format!("rebase descendants: {e}")))?;
        }
        tx.commit("jjlab: receive-pack import")
            .await
            .map_err(|e| RepoError::Other(e.to_string()))?;
        Ok::<(), RepoError>(())
    })?;
    crate::project::project_repo(store, db, org, repo).await?;

    // Actions/CI: scan workflows at every branch tip and enqueue runs.
    let tips = crate::read::bookmark_tips(store, org, repo).await?;
    let logs_root = std::path::PathBuf::from(
        std::env::var("JJLAB_LOGS").unwrap_or_else(|_| "/data/logs".to_string()),
    );
    for (_name, sha) in tips {
        if let Err(e) = crate::actions::on_push(store, db, org, repo, &sha, &logs_root).await {
            tracing::warn!(org, repo, err = %e, "actions on_push failed");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_url_rejects_private_http() {
        assert!(validate_url("http://127.0.0.1/repo.git").is_err());
        assert!(validate_url("http://169.254.169.254/latest/meta-data").is_err());
        assert!(validate_url("http://192.168.1.10/repo.git").is_err());
        assert!(validate_url("http://10.0.0.1/repo.git").is_err());
        assert!(validate_url("http://172.16.0.5/repo.git").is_err());
        assert!(validate_url("https://foo.localhost/repo.git").is_err());
        assert!(validate_url("https://metadata.google.internal/computeMetadata").is_err());
        assert!(validate_url("http://example.com/repo.git").is_ok());
        assert!(validate_url("https://github.com/o/r.git").is_ok());
    }

    #[test]
    fn validate_url_allows_local_paths() {
        assert!(validate_url("/tmp/some/repo.git").is_ok());
        assert!(validate_url("file:///tmp/repo.git").is_ok());
        assert!(validate_url("ssh://git@host/repo.git").is_ok());
    }

    #[test]
    fn embed_credentials_only_when_absent() {
        assert_eq!(
            embed_credentials("https://h/r.git", "u:p"),
            "https://u:p@h/r.git"
        );
        assert_eq!(embed_credentials("https://u:p@h/r.git", "x:y"), "https://u:p@h/r.git");
        assert_eq!(embed_credentials("https://h/r.git", ""), "https://h/r.git");
    }
}
