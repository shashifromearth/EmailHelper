//! Shared application state.

use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Notify;

use crate::config::Config;
use crate::db::Db;
use crate::error::AppResult;
use crate::jobs::browser::BrowserClient;
use crate::knowledge::KnowledgeBase;
use crate::llm::{LlmProvider, LlmRouter};
use crate::metrics::Metrics;
use crate::queue::Queue;
use crate::telegram::{Notifier, TelegramClient};

pub struct App {
    pub cfg: Arc<Config>,
    pub db: Db,
    pub http: reqwest::Client,
    pub llm: LlmRouter,
    pub metrics: Arc<Metrics>,
    pub notifier: Notifier,
    pub knowledge: KnowledgeBase,
    pub browser: Option<BrowserClient>,
    pub queue: Queue,
    /// Woken whenever a draft becomes available for the extension.
    pub drafts_ready: Notify,
    pub started_at: Instant,
}

/// Optional overrides used by tests to inject fakes.
#[derive(Default)]
pub struct AppOverrides {
    pub llm: Option<Arc<dyn LlmProvider>>,
    pub db: Option<Db>,
}

impl App {
    pub fn build(cfg: Config, overrides: AppOverrides) -> AppResult<Arc<Self>> {
        let cfg = Arc::new(cfg);
        let db = match overrides.db {
            Some(db) => db,
            None => Db::open(&cfg.database_path)?,
        };
        let metrics = Arc::new(Metrics::new());
        let http = reqwest::Client::builder()
            .user_agent(concat!("EmailHelper/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(10))
            .pool_idle_timeout(std::time::Duration::from_secs(60))
            .build()?;
        let llm = match overrides.llm {
            Some(p) => LlmRouter::new(p, None, cfg.llm.timeout, metrics.clone()),
            None => LlmRouter::from_config(&cfg.llm, http.clone(), metrics.clone()),
        };
        let notifier = if cfg.telegram_active() {
            Notifier::new(
                Some(TelegramClient::new(
                    http.clone(),
                    &cfg.telegram.api_base,
                    cfg.telegram.bot_token.as_deref().unwrap_or_default(),
                    cfg.telegram.chat_id.unwrap_or_default(),
                )),
                metrics.clone(),
            )
        } else {
            if cfg.telegram.enabled {
                tracing::warn!(
                    "TELEGRAM_ENABLED=true but bot token or chat id missing; Telegram disabled"
                );
            }
            Notifier::disabled(metrics.clone())
        };
        let browser = (cfg.jobs.enabled
            || cfg.research.recruiter_linkedin_enabled
            || !cfg.research.career_pages.is_empty())
        .then(|| {
            BrowserClient::new(
                http.clone(),
                &cfg.jobs.worker_url,
                &cfg.jobs.worker_token,
                cfg.jobs.worker_timeout,
            )
        });
        let knowledge = KnowledgeBase::new(cfg.knowledge_dir.clone(), db.clone());
        if let Some(url) = &cfg.knowledge_service_url {
            knowledge.set_remote(crate::knowledge::remote::KnowledgeClient::new(
                http.clone(),
                url,
                &cfg.knowledge_service_token,
            ));
        }
        let queue = Queue::new(db.clone(), metrics.clone());
        Ok(Arc::new(Self {
            cfg,
            db,
            http,
            llm,
            metrics,
            notifier,
            knowledge,
            browser,
            queue,
            drafts_ready: Notify::new(),
            started_at: Instant::now(),
        }))
    }

    /// Append an audit record, logging (not propagating) storage errors.
    pub async fn audit(
        &self,
        actor: &'static str,
        action: &'static str,
        entity_type: &'static str,
        entity_id: &str,
        details: serde_json::Value,
    ) {
        let id = entity_id.to_string();
        if let Err(e) = self
            .db
            .call(move |c| crate::db::audit::log(c, actor, action, entity_type, &id, details))
            .await
        {
            tracing::error!(error = %e, action, "failed to write audit log");
        }
    }
}
