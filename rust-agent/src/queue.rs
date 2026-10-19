//! Durable, bounded-concurrency job dispatcher.
//!
//! * Rows in `processing_jobs` are the source of truth (survive restarts).
//! * Job ids travel over an unbounded channel; no polling loops.
//! * A semaphore caps concurrent work at `EH_MAX_CONCURRENCY`.
//! * Failures retry with exponential backoff, then go `dead` and alert.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Mutex, Semaphore};
use tracing::Instrument;

use crate::app::App;
use crate::db::{queue as qdb, Db};
use crate::error::AppResult;
use crate::metrics::Metrics;
use crate::telegram::{format, Event};

pub const KIND_EMAIL: &str = "email";
pub const KIND_JOB_EMAIL: &str = "job_email";
pub const KIND_APPLICATION: &str = "job_application";
pub const KIND_RECRUITER: &str = "recruiter_discovery";
pub const KIND_RESEARCH: &str = "research";

pub struct Queue {
    db: Db,
    metrics: Arc<Metrics>,
    tx: mpsc::UnboundedSender<String>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
}

impl Queue {
    pub fn new(db: Db, metrics: Arc<Metrics>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            db,
            metrics,
            tx,
            rx: Mutex::new(Some(rx)),
        }
    }

    /// Enqueue a job. Returns `None` if the idempotency key was already used.
    pub async fn enqueue(
        &self,
        kind: &'static str,
        key: String,
        ref_id: Option<String>,
        payload: Option<String>,
        max_attempts: u32,
    ) -> AppResult<Option<String>> {
        let id = self
            .db
            .call(move |c| {
                qdb::enqueue(
                    c,
                    kind,
                    &key,
                    ref_id.as_deref(),
                    payload.as_deref(),
                    max_attempts,
                )
            })
            .await?;
        if let Some(id) = &id {
            self.metrics.queue_depth.inc();
            let _ = self.tx.send(id.clone());
        }
        Ok(id)
    }

    fn resend_after(&self, id: String, delay: Duration) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = tx.send(id);
        });
    }

    /// Start the dispatcher. Recovers interrupted work first.
    pub async fn start(app: Arc<App>) -> AppResult<tokio::task::JoinHandle<()>> {
        let mut rx = app
            .queue
            .rx
            .lock()
            .await
            .take()
            .expect("queue dispatcher started twice");
        let recovered = app.db.call(|c| qdb::recover(c)).await?;
        app.metrics.queue_depth.set(recovered.len() as i64);
        let now = chrono::Utc::now();
        for job in recovered {
            let delay = chrono::DateTime::parse_from_rfc3339(&job.run_after)
                .map(|t| {
                    (t.with_timezone(&chrono::Utc) - now)
                        .to_std()
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            if delay.is_zero() {
                let _ = app.queue.tx.send(job.id);
            } else {
                app.queue.resend_after(job.id, delay);
            }
        }
        let sem = Arc::new(Semaphore::new(app.cfg.max_concurrency));
        Ok(tokio::spawn(async move {
            while let Some(id) = rx.recv().await {
                let permit = match sem.clone().acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => break,
                };
                let app = app.clone();
                tokio::spawn(async move {
                    run_one(&app, &id).await;
                    drop(permit);
                });
            }
        }))
    }
}

async fn run_one(app: &Arc<App>, id: &str) {
    let id_owned = id.to_string();
    let job = match app.db.call(move |c| qdb::claim(c, &id_owned)).await {
        Ok(Some(j)) => j,
        Ok(None) => return, // already claimed/finished (e.g. duplicate wake-up)
        Err(e) => {
            tracing::error!(error = %e, job_id = id, "failed to claim job");
            return;
        }
    };
    let span =
        tracing::info_span!("job", job_id = %job.id, kind = %job.kind, attempt = job.attempts);
    let result = dispatch(app, &job).instrument(span).await;
    let id_owned = job.id.clone();
    match result {
        Ok(()) => {
            app.metrics.queue_depth.dec();
            if let Err(e) = app.db.call(move |c| qdb::complete(c, &id_owned)).await {
                tracing::error!(error = %e, "failed to mark job complete");
            }
        }
        Err(err) => {
            let msg = err.to_string();
            app.metrics.failure(&job.kind);
            let retry = job.attempts < job.max_attempts;
            let delay = Duration::from_secs(2u64.pow(job.attempts.clamp(1, 8) as u32));
            let retry_at = retry.then(|| {
                (chrono::Utc::now() + chrono::Duration::from_std(delay).unwrap_or_default())
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            });
            let m2 = msg.clone();
            let _ = app
                .db
                .call(move |c| qdb::fail(c, &id_owned, &m2, retry_at.as_deref()))
                .await;
            if retry {
                tracing::warn!(job_id = %job.id, kind = %job.kind, error = %msg, retry_in_secs = delay.as_secs(), "job failed; will retry");
                app.queue.resend_after(job.id.clone(), delay);
            } else {
                app.metrics.queue_depth.dec();
                tracing::error!(job_id = %job.id, kind = %job.kind, error = %msg, "job failed permanently");
                app.audit(
                    "queue",
                    "job_dead",
                    "processing_job",
                    &job.id,
                    serde_json::json!({ "kind": job.kind, "error": msg, "ref": job.ref_id }),
                )
                .await;
                on_dead(app, &job.kind, job.ref_id.as_deref()).await;
                app.notifier
                    .notify(
                        Event::ProcessingFailed,
                        &format::processing_failed(
                            &job.kind,
                            job.ref_id.as_deref().unwrap_or("-"),
                            &msg,
                        ),
                    )
                    .await;
            }
        }
    }
}

async fn dispatch(app: &Arc<App>, job: &qdb::QueuedJob) -> AppResult<()> {
    let ref_id = job.ref_id.clone().unwrap_or_default();
    match job.kind.as_str() {
        KIND_EMAIL => crate::agents::pipeline::process_email(app, &ref_id).await,
        KIND_JOB_EMAIL => crate::jobs::process_job_email(app, &ref_id).await,
        KIND_APPLICATION => crate::jobs::application::run(app, &ref_id).await,
        KIND_RECRUITER => crate::research::recruiters::discover_for_email(app, &ref_id).await,
        KIND_RESEARCH => crate::research::discovery::run(app).await.map(|_| ()),
        other => {
            tracing::error!(kind = other, "unknown job kind");
            Ok(())
        }
    }
}

/// Final-failure bookkeeping per workflow.
async fn on_dead(app: &Arc<App>, kind: &str, ref_id: Option<&str>) {
    let Some(r) = ref_id.map(str::to_string) else {
        return;
    };
    let res = match kind {
        KIND_EMAIL => {
            app.db
                .call(move |c| crate::db::emails::set_status(c, &r, "failed"))
                .await
        }
        KIND_APPLICATION => {
            app.metrics
                .applications_failed_total
                .with_label_values(&["exhausted_retries"])
                .inc();
            app.db
                .call(move |c| {
                    crate::db::jobs::update_application(
                        c,
                        &r,
                        &crate::db::jobs::AppUpdate {
                            status: Some("failed"),
                            current_step: Some("exhausted_retries"),
                            ..Default::default()
                        },
                    )
                })
                .await
        }
        _ => Ok(()),
    };
    if let Err(e) = res {
        tracing::error!(error = %e, "failed to record dead job state");
    }
}
