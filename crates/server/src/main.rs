//! jjlab server binary — thin shell over the library router.

use std::net::SocketAddr;
use std::sync::Arc;

use jjlab_core::Db;
use jjlab_server::{parse_tokens, AppState};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // The dependency tree pins two rustls crypto providers (kube → ring,
    // gix's curl backend → aws-lc-rs); rustls can't auto-select between them.
    // Pin the process to `ring` explicitly so kube's TLS works.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let port: u16 = std::env::var("JJLAB_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let db_path = std::env::var("JJLAB_DB").unwrap_or_else(|_| "/data/data.db".to_string());

    let db = Arc::new(Db::open(std::path::Path::new(&db_path)).expect("open db"));
    let repos_root = std::env::var("JJLAB_REPOS").unwrap_or_else(|_| "/data/repos".to_string());
    let store = Arc::new(jjlab_git::RepoStore::new(repos_root.into()));
    let tokens = parse_tokens(&std::env::var("JJLAB_TOKENS").unwrap_or_default());
    let assets = Arc::new(
        pkglab_core::blob::FsBlobStore::new(
            &std::path::PathBuf::from(
                std::env::var("JJLAB_ASSETS").unwrap_or_else(|_| "/data/assets".to_string()),
            )
            .join("sha256"),
        )
        .expect("open asset blob store"),
    );
    let state = AppState::new(db.clone(), store.clone(), tokens, assets);

    // In-process package registry (pkglab): OCI + language protocols served
    // by this process. Substrate lives under JJLAB_PKGLAB_ROOT (default
    // /data/pkglab), independent of the git metadata DB.
    let state = {
        let root = std::env::var("JJLAB_PKGLAB_ROOT")
            .unwrap_or_else(|_| "/data/pkglab".to_string());
        if std::env::var("JJLAB_PKGLAB_ENABLED").map(|v| v != "0" && !v.is_empty()).unwrap_or(true) {
            match pkglab_core::Registry::open(std::path::Path::new(&root)) {
                Ok(reg) => {
                    let common = Arc::new(pkglab_common::Registry::new(
                        reg.blobs.clone(),
                        reg.meta.clone(),
                        reg.upstreams.clone(),
                    ));
                    tracing::info!(%root, "pkglab registry enabled");
                    state.with_registry(common)
                }
                Err(e) => {
                    tracing::warn!(%root, err = %e, "pkglab registry failed to open; serving without registry");
                    state
                }
            }
        } else {
            state
        }
    };

    let app = jjlab_server::build_router(state);

    // Out-of-band CI scheduler: drains queued runs into k8s sandbox pods.
    // Never runs user code in this process; only starts if JJLAB_CI_ENABLED=1.
    let scheduler = {
        let db = db.clone();
        let store = store.clone();
        let logs_root = std::path::PathBuf::from(
            std::env::var("JJLAB_LOGS").unwrap_or_else(|_| "/data/logs".to_string()),
        );
        tokio::spawn(jjlab_git::scheduler::run_loop(
            db,
            store,
            logs_root,
            std::time::Duration::from_secs(2),
        ))
    };

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| {
            tracing::error!(%addr, err = %e, "jjlab bind failed");
            std::process::exit(1);
        });
    tracing::info!(%addr, %db_path, "jjlab listening");

    // Serve HTTP/1.1 (and HTTP/2 where the client negotiates) with a long
    // header-read timeout. hyper's default 30s idle reaper closes keep-alive
    // connections that buildkit leaves idle between a manifest HEAD (401
    // challenge) and the follow-up token POST; buildkit's Go client keeps
    // pooled conns ~90s and errors with "server closed idle connection".
    // Setting header_read_timeout >> that window removes the race. h2c is NOT
    // enabled here (registry stays HTTP/1.1 like buildkit expects).
    let app_svc = tower::ServiceBuilder::new().service(app);
    let hyper_service = hyper_util::service::TowerToHyperService::new(app_svc);

    // `serve` is an infinite accept loop (never returns). Each connection gets
    // its own fresh auto::Builder so the borrow of `&self` inside
    // serve_connection stays alive for that connection; we set a long
    // header-read timeout so keep-alive connections waited on by buildkit
    // (~90s idle) are not reaped by the default 30s timeout.
    let serve = async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let io = hyper_util::rt::TokioIo::new(stream);
                    let svc = hyper_service.clone();
                    tokio::spawn(async move {
                        let mut builder = hyper_util::server::conn::auto::Builder::new(
                            hyper_util::rt::TokioExecutor::new(),
                        );
                        builder
                            .http1()
                            .timer(hyper_util::rt::TokioTimer::new())
                            .header_read_timeout(
                                Some(std::time::Duration::from_secs(300)),
                            );
                        let _ = builder.serve_connection(io, svc).await;
                    });
                }
                Err(e) => {
                    tracing::warn!(err = %e, "jjlab accept failed");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    };

    // Drive the accept loop forever (it never returns); run scheduler alongside.
    let serve_pending = async move { serve.await; std::future::pending::<()>().await };
    tokio::select! {
        _ = serve_pending => { unreachable!("accept loop never returns") }
        r = scheduler => {
            if let Err(e) = r {
                tracing::error!(err = %e, "jjlab scheduler crashed");
                std::process::exit(1);
            }
        }
    }
}

