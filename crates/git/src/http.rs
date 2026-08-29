//! Inbound Git Smart HTTP (ADR-009): serve `git-upload-pack` /
//! `git-receive-pack` by spawning the system git binary in `--stateless-rpc`
//! mode against the jj store's git directory — exactly what Gitea does.
//!
//! No protocol is reimplemented: the request body is piped verbatim to the
//! subprocess, its stdout is streamed back. After a successful
//! `receive-pack`, the caller runs `import_refs` so the received commits
//! become native jj changes (change-id header preserved).

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;

use crate::repo::{RepoError, RepoStore};

/// The git service to invoke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitService {
    UploadPack,
    ReceivePack,
}

impl GitService {
    pub fn as_str(self) -> &'static str {
        match self {
            GitService::UploadPack => "upload-pack",
            GitService::ReceivePack => "receive-pack",
        }
    }

    pub fn content_type(self, suffix: &str) -> String {
        format!("application/x-git-{}-{}", self.as_str(), suffix)
    }

    /// Parse `?service=git-<svc>` from an info/refs query.
    pub fn from_service_param(param: Option<&str>) -> Option<GitService> {
        let param = param?;
        let svc = param.strip_prefix("git-")?;
        match svc {
            "upload-pack" => Some(GitService::UploadPack),
            "receive-pack" => Some(GitService::ReceivePack),
            _ => None,
        }
    }
}

/// The directory git should operate on: the jj store's git dir.
///
/// For internal-git repos this is `<dir>/.jj/repo/store/git`; for
/// external-git (cloned) repos the store's `git_target` points at the repo
/// root itself. Reading `git_target` handles both uniformly.
pub fn git_dir_of(store: &Arc<RepoStore>, org: &str, repo: &str) -> Result<std::path::PathBuf, RepoError> {
    let dir = store.repo_dir_checked(org, repo)?;
    let target_file = dir.join(".jj/repo/store/git_target");
    let relative = std::fs::read_to_string(&target_file)
        .map_err(|e| RepoError::Other(format!("read git_target: {e}")))?;
    let base = dir.join(".jj/repo/store");
    let resolved = base.join(relative.trim_end_matches('\n'));
    let resolved = std::fs::canonicalize(&resolved)
        .map_err(|e| RepoError::Other(format!("canonicalize git dir: {e}")))?;
    Ok(resolved)
}

fn spawn(service: GitService, git_dir: &Path, advertise: bool) -> Command {
    let mut cmd = Command::new("git");
    // Don't advertise jj's bookkeeping namespaces (refs/jj/...). They are
    // internal GC anchors, not user-facing branches/tags; leaking them on
    // ls-remote/clone is noise and leaks commit shas. `-c` is a git *global*
    // option, so it must precede the subcommand.
    cmd.arg("-c");
    cmd.arg("uploadpack.hideRefs=refs/jj");
    cmd.arg(service.as_str());
    if advertise {
        cmd.args(["--stateless-rpc", "--advertise-refs"]);
    } else {
        cmd.arg("--stateless-rpc");
    }
    cmd.arg(git_dir);
    cmd.env("GIT_HTTP_EXPORT_ALL", "1");
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd
}

/// Advertise refs (GET /info/refs?service=git-<svc>).
pub async fn advertise_refs(
    service: GitService,
    git_dir: &Path,
) -> Result<Vec<u8>, RepoError> {
    let mut child = spawn(service, git_dir, true)
        .spawn()
        .map_err(|e| RepoError::Other(format!("spawn git {svc}: {e}", svc = service.as_str())))?;
    // No stdin for advertise; close it so git doesn't wait.
    drop(child.stdin.take());
    let mut out = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout
            .read_to_end(&mut out)
            .await
            .map_err(|e| RepoError::Other(format!("read advertise: {e}")))?;
    }
    let status = child
        .wait()
        .await
        .map_err(|e| RepoError::Other(format!("wait advertise: {e}")))?;
    if !status.success() {
        return Err(RepoError::Other(format!(
            "git {svc} advertise failed: {status}",
            svc = service.as_str()
        )));
    }

    // The smart-HTTP response wraps the advertisement in a service header.
    let header = format!("# service=git-{}\n", service.as_str());
    let mut pkt = Vec::with_capacity(out.len() + header.len() + 12);
    pkt.push_pkt_line(header.as_bytes());
    pkt.extend_from_slice(b"0000");
    pkt.extend_from_slice(&out);
    Ok(pkt)
}

/// Run the stateless RPC (POST /git-upload-pack or /git-receive-pack).
///
/// Returns (stdout bytes, stderr bytes, exit success).
pub async fn run_rpc(
    service: GitService,
    git_dir: &Path,
    body: Vec<u8>,
) -> Result<(Vec<u8>, Vec<u8>, bool), RepoError> {
    let mut child = spawn(service, git_dir, false)
        .spawn()
        .map_err(|e| RepoError::Other(format!("spawn git {svc}: {e}", svc = service.as_str())))?;

    // Write the buffered request body into stdin (stateless RPC is one shot).
    if let Some(mut stdin) = child.stdin.take() {
        let writer = tokio::spawn(async move {
            if stdin.write_all(&body).await.is_err() {
                return;
            }
            let _ = stdin.shutdown().await;
        });
        let mut out = Vec::new();
        if let Some(mut stdout) = child.stdout.take() {
            stdout
                .read_to_end(&mut out)
                .await
                .map_err(|e| RepoError::Other(format!("read rpc: {e}")))?;
        }
        let mut err = Vec::new();
        if let Some(mut stderr) = child.stderr.take() {
            stderr
                .read_to_end(&mut err)
                .await
                .map_err(|e| RepoError::Other(format!("read rpc stderr: {e}")))?;
        }
        let status = child
            .wait()
            .await
            .map_err(|e| RepoError::Other(format!("wait rpc: {e}")))?;
        let _ = writer.await;
        Ok((out, err, status.success()))
    } else {
        Err(RepoError::Other("no stdin".to_string()))
    }
}

trait PushPktLine {
    fn push_pkt_line(&mut self, data: &[u8]);
}

impl PushPktLine for Vec<u8> {
    fn push_pkt_line(&mut self, data: &[u8]) {
        let len = data.len() + 4;
        self.extend_from_slice(format!("{len:04x}").as_bytes());
        self.extend_from_slice(data);
    }
}

/// Packet-write helper used by the info/refs response (kept pub for tests).
pub fn pkt_line(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 4);
    out.push_pkt_line(data);
    out
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkt_line_encodes_hex_length() {
        // 4-hex length prefix includes the 4 header bytes themselves.
        assert_eq!(pkt_line(b"hi"), b"0006hi");
        assert_eq!(pkt_line(b""), b"0004");
    }

    #[test]
    fn service_param_parsing() {
        assert_eq!(
            GitService::from_service_param(Some("git-upload-pack")),
            Some(GitService::UploadPack)
        );
        assert_eq!(
            GitService::from_service_param(Some("git-receive-pack")),
            Some(GitService::ReceivePack)
        );
        assert_eq!(GitService::from_service_param(Some("git-unknown")), None);
        assert_eq!(GitService::from_service_param(None), None);
        assert_eq!(GitService::from_service_param(Some("upload-pack")), None);
    }

    #[test]
    fn content_types_follow_spec() {
        assert_eq!(
            GitService::UploadPack.content_type("advertisement"),
            "application/x-git-upload-pack-advertisement"
        );
        assert_eq!(
            GitService::ReceivePack.content_type("request"),
            "application/x-git-receive-pack-request"
        );
    }

    #[test]
    fn spawn_hides_jj_bookkeeping_refs() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = spawn(GitService::UploadPack, dir.path(), true);
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // `-c uploadpack.hideRefs=refs/jj` must precede the subcommand.
        assert_eq!(&args[0], "-c", "hideRefs global option must be first: {args:?}");
        assert_eq!(&args[1], "uploadpack.hideRefs=refs/jj", "args: {args:?}");
        assert!(args.iter().any(|a| a == "upload-pack"));
    }
}
