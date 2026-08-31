//! Image-build primitive for the `/ops` surface.
//!
//! jjlab does NOT know about protocols, publish templates, or CI workflows;
//! it only turns a build request (repo checkout or a raw Containerfile) into a
//! `buildctl` invocation against the configured buildkitd, with a three-state
//! export:
//!
//!   - `push` — build and push an image to the registry (the in-process one)
//!   - `oci` — build, export an OCI layout to a temp path (no push)
//!   - `none` — build with no export (RUN side-effects only, the publish
//!     substrate: the caller's Containerfile does the publishing)
//!
//! The caller (ops-extension) owns the Containerfile content, image naming,
//! and all protocol specifics.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Deserialize;

use crate::repo::{RepoError, RepoResult};

/// A build request: either a repo checkout (`org`/`repo`/`bookmark`) or a raw
/// Containerfile carried in the request body.
#[derive(Debug, Clone, Deserialize)]
pub struct BuildRequest {
    #[serde(default)]
    pub org: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub bookmark: String,
    /// Raw build: Containerfile content carried in `containerfile`.
    #[serde(default)]
    pub raw: bool,
    /// Containerfile path relative to the repo root (repo builds).
    #[serde(default)]
    pub dockerfile: Option<String>,
    /// Containerfile body, used when `raw` is true.
    #[serde(default)]
    pub containerfile: String,
    #[serde(default)]
    pub image: Option<String>,
    /// "push" | "oci" | "none" (default "oci").
    #[serde(default = "default_export")]
    pub export: String,
    #[serde(default)]
    pub build_args: Vec<String>,
    #[serde(default)]
    pub no_cache: bool,
}

fn default_export() -> String {
    "oci".to_string()
}

/// buildkitd address (single, configurable via env; not namespace-scoped).
pub fn buildkit_addr() -> String {
    std::env::var("JJLAB_BUILDKIT_ADDR")
        .unwrap_or_else(|_| "tcp://buildkitd.temp.svc.cluster.local:1234".to_string())
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// A unique scratch dir under the system temp dir. Callers best-effort remove
/// it; a leftover is small and inert.
fn scratch_dir(tag: &str) -> RepoResult<PathBuf> {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&path).map_err(|e| RepoError::Other(format!("mkdir {path:?}: {e}")))?;
    Ok(path)
}

/// Run a build; `sink` receives merged buildctl progress lines. Returns the
/// pushed image ref (push) or the OCI layout path (oci) on success.
pub async fn run_build(
    ctx_dir: Option<&Path>,
    containerfile_body: Option<&str>,
    image: Option<&str>,
    export: &str,
    build_args: &[String],
    no_cache: bool,
    sink: impl FnMut(&str) + Send + 'static,
) -> RepoResult<String> {
    let mut args: Vec<String> = vec![
        "--addr".into(),
        buildkit_addr(),
        "build".into(),
        "--frontend".into(),
        "dockerfile.v0".into(),
        "--progress".into(),
        "plain".into(),
    ];

    // Build context: a repo checkout dir, or a synthetic temp dir holding the
    // raw Containerfile. One of the two must be present.
    let context: PathBuf = if let Some(dir) = ctx_dir {
        dir.to_path_buf()
    } else if let Some(body) = containerfile_body {
        let dir = scratch_dir("jjlab-build-ctx")?;
        std::fs::write(dir.join("Containerfile"), body)
            .map_err(|e| RepoError::Other(format!("write raw Containerfile: {e}")))?;
        dir
    } else {
        return Err(RepoError::Invalid("build requires a repo context or a raw Containerfile".into()));
    };

    args.push("--local".into());
    args.push(format!("context={}", context.to_string_lossy()));
    args.push("--local".into());
    args.push(format!("dockerfile={}", context.to_string_lossy()));

    for ba in build_args {
        args.push("--opt".into());
        args.push(format!("build-arg:{ba}"));
    }
    if no_cache {
        args.push("--no-cache".into());
    }

    let result_ref: String = match export {
        "push" => {
            let img = image
                .ok_or_else(|| RepoError::Invalid("export=push requires an image ref".into()))?;
            args.push("--output".into());
            args.push(format!("type=image,name={img},push=true"));
            img.to_string()
        }
        "none" => String::new(),
        _ => {
            let dir = scratch_dir("jjlab-build-oci")?;
            let dest = dir.join("out.tar");
            let dest_str = dest.to_string_lossy().to_string();
            args.push("--output".into());
            args.push(format!("type=oci,dest={dest_str}"));
            dest_str
        }
    };

    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let code = crate::runtime::run_cli_stream("buildctl", &arg_refs, Duration::from_secs(3600), sink).await?;
    if code != 0 {
        return Err(RepoError::Other(format!("buildctl exited with status {code}")));
    }
    Ok(result_ref)
}