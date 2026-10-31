use std::process::ExitCode;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use knowledge_service::api::{self, AppState};
use knowledge_service::config::Config;
use knowledge_service::embed::{Embedder, Gemini, Ollama};
use knowledge_service::index::Indexer;
use knowledge_service::search::Searcher;
use knowledge_service::store::{self, Store};

#[tokio::main]
async fn main() -> ExitCode {
    let _ = dotenvy::from_filename(std::env::var("EH_ENV_FILE").unwrap_or_else(|_| ".env".into()));
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("configuration error: {e}");
            return ExitCode::from(2);
        }
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    if cfg.json_logs {
        tracing_subscriber::fmt().with_env_filter(filter).json().flatten_event(true).init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    let store = match Store::open(&cfg.db_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, path = %cfg.db_path.display(), "cannot open the knowledge database");
            return ExitCode::FAILURE;
        }
    };
    let http = reqwest::Client::builder().timeout(Duration::from_secs(90)).build().unwrap_or_default();
    // Gemini first (better quality), the local model as the always-available fallback.
    let mut embedders: Vec<Arc<dyn Embedder>> = Vec::new();
    if let Some(key) = &cfg.gemini_key {
        embedders.push(Arc::new(Gemini { http: http.clone(), key: key.clone(), model: cfg.gemini_model.clone(), base: cfg.gemini_base.clone() }));
    }
    embedders.push(Arc::new(Ollama { http: http.clone(), base: cfg.ollama_base.clone(), model: cfg.local_model.clone() }));

    let indexer = Arc::new(Indexer::new(cfg.knowledge_dir.clone(), store.clone(), embedders.clone()));
    let state = Arc::new(AppState {
        token: cfg.token.clone(),
        indexer,
        searcher: Searcher::new(store.clone(), embedders),
        people: RwLock::new(Arc::new(eh_knowledge::People::load(&cfg.knowledge_dir))),
    });

    // Daily ETL: at startup and every few hours, run if today's run hasn't happened.
    let s2 = state.clone();
    let every = Duration::from_secs(cfg.etl_check_hours * 3600);
    // missing vectors are topped up hourly; the full ETL still runs once a day
    let every = every.min(Duration::from_secs(3600));
    tokio::spawn(async move {
        loop {
            let today = chrono::Local::now().format("%Y-%m-%d").to_string();
            let last = s2.searcher.store.call(|c| store::get_meta(c, "last_etl_date")).await.ok().flatten();
            if last.as_deref() != Some(today.as_str()) {
                tracing::info!(last_run = ?last, "knowledge ETL has not run today; running now");
                if let Err(e) = s2.reindex().await {
                    tracing::error!(error = %e, "scheduled knowledge ETL failed");
                }
            } else if s2.indexer.missing_vectors().await > 0 {
                // e.g. Gemini's free quota stopped the last pass: continue it
                match s2.indexer.top_up().await {
                    Ok(done) => tracing::info!(?done, "embedding top-up"),
                    Err(e) => tracing::info!(error = %e, "embedding top-up skipped"),
                }
            }
            tokio::time::sleep(every).await;
        }
    });

    let listener = match tokio::net::TcpListener::bind(cfg.addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, addr = %cfg.addr, "cannot bind");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(addr = %cfg.addr, knowledge = %cfg.knowledge_dir.display(), "knowledge service listening");
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("shutting down");
    };
    if let Err(e) = axum::serve(listener, api::router(state)).with_graceful_shutdown(shutdown).await {
        tracing::error!(error = %e, "server error");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
