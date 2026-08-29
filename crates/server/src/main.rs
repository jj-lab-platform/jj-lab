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
    let assets = Arc::new(jjlab_git::assets::AssetStore::new(
        std::env::var("JJLAB_ASSETS").unwrap_or_else(|_| "/data/assets".to_string()),
    ));
    let state = AppState::new(db.clone(), store.clone(), tokens, assets);

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
    let serve = axum::serve(listener, app);
    tokio::select! {
        r = serve => {
            if let Err(e) = r {
                tracing::error!(err = %e, "jjlab serve failed");
                std::process::exit(1);
            }
        }
        r = scheduler => {
            if let Err(e) = r {
                tracing::error!(err = %e, "jjlab scheduler crashed");
                std::process::exit(1);
            }
        }
    }
}

