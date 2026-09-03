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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
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
    dockerfile_name: Option<&str>,
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

    // buildkit's dockerfile.v0 frontend looks for a file named `Dockerfile`
    // by default. Honour a caller-supplied filename (e.g. `Containerfile`)
    // and force `Containerfile` for raw builds. Without this, repo builds that
    // use a non-default Dockerfile name fail with "open Dockerfile: no such
    // file or directory".
    let fname = if containerfile_body.is_some() {
        "Containerfile".to_string()
    } else {
        dockerfile_name.unwrap_or("Dockerfile").to_string()
    };
    if fname != "Dockerfile" {
        args.push("--opt".into());
        args.push(format!("filename={fname}"));
    }

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

/// The in-process registry base (scheme://host), used to qualify the derived
/// sandbox-image reference with the correct host.
fn self_base() -> String {
    std::env::var("JJLAB_SELF_BASE")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "http://localhost:8080".to_string())
}

/// In-memory cache of derived sandbox-image names so a base image is only
/// worker-ized once per process (buildkit caches the layer anyway; this avoids
/// a redundant buildctl invocation for repeated sandbox creation).
static DERIVED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Derive (or reuse) a sandbox runtime image from a base image by appending the
/// worker binary and a worker entrypoint, pushed to the in-process OCI registry
/// under a deterministic name. Only images reachable from jjlab (OCI
/// pull-through, incl. `library/*` from Docker Hub) are accepted.
///
/// Returns the fully-qualified derived image reference.
pub async fn derive_sandbox_image(base: &str, worker_ref: &str) -> RepoResult<String> {
    let base = base.trim().to_string();
    if base.is_empty() {
        return Err(RepoError::Invalid("sandbox image is empty".into()));
    }
    // Deterministic name from the base image string: sandbox/<sanitized>-<sha256[:12]>.
    let mut hash = sha2::Sha256::new();
    use sha2::Digest as _;
    hash.update(base.as_bytes());
    let digest = hex::encode(hash.finalize());
    let tag = format!("{}-{}", sanitize_image_name(&base), &digest[..12]);
    let derived = format!("{}/sandbox/{}", host_of(&self_base()), tag);

    // Cache: skip the build when we already derived it this process.
    if {
        let mut guard = DERIVED.lock().unwrap_or_else(|e| e.into_inner());
        let set = guard.get_or_insert_with(HashSet::new);
        !set.insert(derived.clone())
    } {
        return Ok(derived);
    }

    // Build a scratch context with the derived Containerfile.
    let dir = scratch_dir("jjlab-sandbox-derive")?;
    let containerfile = format!(
        "FROM {base}\nCOPY --from={worker_ref} /usr/local/bin/worker-go /usr/local/bin/worker-go\nENTRYPOINT [\"worker-go\"]\n"
    );
    std::fs::write(dir.join("Containerfile"), &containerfile)
        .map_err(|e| RepoError::Other(format!("write derived Containerfile: {e}")))?;

    let mut args: Vec<String> = vec![
        "--addr".into(),
        buildkit_addr(),
        "build".into(),
        "--frontend".into(),
        "dockerfile.v0".into(),
        "--progress".into(),
        "plain".into(),
    ];
    args.push("--local".into());
    args.push(format!("context={}", dir.to_string_lossy()));
    args.push("--local".into());
    args.push(format!("dockerfile={}", dir.to_string_lossy()));
    args.push("--opt".into());
    args.push("filename=Containerfile".into());
    args.push("--output".into());
    args.push(format!("type=image,name={derived},push=true"));

    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let code = crate::runtime::run_cli_stream(
        "buildctl",
        &arg_refs,
        Duration::from_secs(600),
        |_| {},
    )
    .await?;
    if code != 0 {
        if let Some(set) = DERIVED.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            set.remove(&derived);
        }
        return Err(RepoError::Other(format!("derive sandbox image exited with status {code}")));
    }
    Ok(derived)
}

/// Host[:port] of an (optional) URL, for building registry-qualified refs.
fn host_of(raw: &str) -> String {
    let s = raw.trim();
    let after = if s.contains("://") {
        s.split_once("://").map(|x| x.1).unwrap_or(s)
    } else {
        s
    };
    let host = after.split('/').next().unwrap_or(after);
    host.trim_end_matches(':').to_string()
}

/// Sanitize a reference into a filesystem-safe tag fragment.
fn sanitize_image_name(ref_: &str) -> String {
    let mut out = String::new();
    for c in ref_.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_' {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    let base = if trimmed.is_empty() { "img" } else { trimmed };
    let tail = if base.len() > 40 { &base[base.len() - 40..] } else { base };
    tail.to_string()
}