//! EmailHelper: local-first AI orchestrator for email drafting, job
//! applications and research. See README.md and documents/.

pub mod agents;
pub mod api;
pub mod app;
pub mod config;
pub mod db;
pub mod error;
pub mod jobs;
pub mod knowledge;
pub mod llm;
pub mod metrics;
pub mod prompts;
pub mod queue;
pub mod research;
pub mod telegram;

use std::sync::Arc;

use app::App;
use error::AppResult;

/// Background tasks started by [`start_background`].
pub struct Background {
    pub handles: Vec<tokio::task::JoinHandle<()>>,
}

impl Background {
    pub fn abort(&self) {
        for h in &self.handles {
            h.abort();
        }
    }
}

/// Start the queue dispatcher, Telegram bot, research scheduler and recovery.
pub async fn start_background(app: &Arc<App>) -> AppResult<Background> {
    let mut handles = Vec::new();
    if let Err(e) = app.knowledge.reindex().await {
        tracing::warn!(error = %e, "initial knowledge indexing failed");
    }
    handles.push(queue::Queue::start(app.clone()).await?);
    jobs::application::recover(app).await?;
    if let Some(h) = telegram::bot::spawn(app.clone()) {
        handles.push(h);
    }
    if app.cfg.research.enabled {
        handles.push(research::discovery::spawn_scheduler(app.clone()));
    }
    if app.cfg.jobs.enabled {
        handles.push(jobs::digest::spawn_scheduler(app.clone()));
    }
    Ok(Background { handles })
}
