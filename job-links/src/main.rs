//! `job-links`: serve the job-link queue on 127.0.0.1.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use anyhow::Context;
use job_links::api::{self, AppState};
use job_links::config::Config;
use job_links::snapshot;
use job_links::store::Store;
use tokio::sync::watch;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn".into()),
        )
        .json()
        .init();

    let cfg = Config::from_env().inspect_err(|e| {
        tracing::error!(error = format!("{e:#}"), "invalid configuration");
    })?;
    let records = snapshot::load(&cfg.snapshot_path).inspect_err(|e| {
        tracing::error!(error = format!("{e:#}"), "cannot load snapshot");
    })?;
    tracing::info!(links = records.len(), path = %cfg.snapshot_path.display(), "snapshot loaded");
    let store = Arc::new(Store::from_records(records));

    let (stop_tx, stop_rx) = watch::channel(false);
    let snapshots = tokio::spawn(snapshot::run(
        store.clone(),
        cfg.snapshot_path.clone(),
        cfg.snapshot_interval,
        stop_rx,
    ));

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, cfg.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "job-links listening");
    let app = api::router(AppState {
        store,
        token: cfg.token.into(),
        default_lease: cfg.default_lease,
        snapshot_path: Arc::new(cfg.snapshot_path.clone()),
    });
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;

    let _ = stop_tx.send(true);
    let _ = snapshots.await;
    served.context("server error")
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(windows)]
    {
        let mut brk = tokio::signal::windows::ctrl_break().expect("Ctrl+Break handler");
        let mut close = tokio::signal::windows::ctrl_close().expect("console close handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = brk.recv() => {}
            _ = close.recv() => {}
        }
    }
    #[cfg(not(any(unix, windows)))]
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
