use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::App;
use crate::db::emails::{self, EmailIn, InsertOutcome};
use crate::db::{audit, drafts, jobs as jdb, queue as qdb};
use crate::error::{AppError, AppResult};
use crate::queue;

// ------------------------------------------------------------- emails

pub async fn submit_email(
    State(app): State<Arc<App>>,
    Json(mut e): Json<EmailIn>,
) -> AppResult<Response> {
    e.validate(app.cfg.max_email_text_bytes)?;
    let oversized: Vec<String> = e
        .attachments
        .iter()
        .filter(|a| a.size > app.cfg.max_attachment_bytes)
        .map(|a| a.name.clone())
        .collect();
    let version = app.cfg.workflow_version;
    let key = format!("email:{}:{}:v{}", e.account_id, e.message_id, version);
    let (account, message_id) = (e.account_id.clone(), e.message_id.clone());
    let e2 = e.clone();
    let outcome = app.db.call(move |c| emails::insert(c, &e2)).await?;
    let (email_id, existed) = match outcome {
        InsertOutcome::Inserted(id) => (id, false),
        InsertOutcome::Existing(id) => (id, true),
    };
    if existed {
        // Same message under a new workflow version: process it again.
        let (id, k) = (email_id.clone(), key.clone());
        app.db
            .call(move |c| {
                if !qdb::exists_key(c, &k)? {
                    emails::set_status(c, &id, "received")?;
                }
                Ok(())
            })
            .await?;
    }
    let job_id = app
        .queue
        .enqueue(
            queue::KIND_EMAIL,
            key,
            Some(email_id.clone()),
            None,
            app.cfg.max_retries,
        )
        .await?;
    match job_id {
        Some(job_id) => {
            app.audit(
                "api",
                "email_received",
                "email",
                &email_id,
                json!({ "account_id": account, "message_id": message_id, "oversized_attachments": oversized }),
            )
            .await;
            tracing::info!(email_id = %email_id, account_id = %account, "email queued");
            Ok((
                StatusCode::ACCEPTED,
                Json(json!({ "email_id": email_id, "job_id": job_id, "status": "queued" })),
            )
                .into_response())
        }
        None => {
            app.metrics.email_duplicates_total.inc();
            tracing::debug!(email_id = %email_id, "duplicate email ignored");
            Ok((
                StatusCode::OK,
                Json(json!({ "email_id": email_id, "job_id": null, "status": "duplicate" })),
            )
                .into_response())
        }
    }
}

// ------------------------------------------------------------- drafts

#[derive(Deserialize)]
pub struct WaitQuery {
    #[serde(default)]
    wait: Option<u64>,
}

pub async fn pending_drafts(
    State(app): State<Arc<App>>,
    Query(q): Query<WaitQuery>,
) -> AppResult<Json<Value>> {
    let wait = Duration::from_secs(q.wait.unwrap_or(0).min(55));
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        // Register interest *before* checking, so a draft created between the
        // check and the wait still wakes us.
        let notified = app.drafts_ready.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let list = app.db.call(|c| drafts::claim_pending(c, 10)).await?;
        if !list.is_empty() || tokio::time::Instant::now() >= deadline {
            return Ok(Json(json!({ "drafts": list })));
        }
        tokio::select! {
            _ = &mut notified => {}
            _ = tokio::time::sleep_until(deadline) => {
                return Ok(Json(json!({ "drafts": [] })));
            }
        }
    }
}

#[derive(Deserialize)]
pub struct AckBody {
    status: String,
    #[serde(default)]
    tb_draft_message_id: Option<i64>,
    #[serde(default)]
    error: Option<String>,
}

pub async fn ack_draft(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Json(b): Json<AckBody>,
) -> AppResult<StatusCode> {
    let saved = match b.status.as_str() {
        "saved" => true,
        "failed" => false,
        other => return Err(AppError::Validation(format!("invalid status {other}"))),
    };
    let err = b
        .error
        .clone()
        .map(|e| e.chars().take(500).collect::<String>());
    let id2 = id.clone();
    let outcome = app
        .db
        .call(move |c| drafts::ack(c, &id2, saved, b.tb_draft_message_id, err.as_deref()))
        .await?;
    let action = match outcome {
        drafts::AckOutcome::Saved => "draft_saved",
        drafts::AckOutcome::Retry => {
            app.drafts_ready.notify_waiters();
            "draft_save_retry"
        }
        drafts::AckOutcome::Failed => {
            app.metrics.failure("draft_writer");
            app.notifier
                .notify(
                    crate::telegram::Event::ProcessingFailed,
                    &format!(
                        "❌ Thunderbird could not save draft {id}: {}",
                        b.error.as_deref().unwrap_or("unknown error")
                    ),
                )
                .await;
            "draft_save_failed"
        }
    };
    app.audit(
        "thunderbird",
        action,
        "draft",
        &id,
        json!({ "tb_draft_message_id": b.tb_draft_message_id, "error": b.error }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------- status & dashboard

pub async fn status(State(app): State<Arc<App>>) -> AppResult<Json<Value>> {
    let (processed, counts, depth, dead) = app
        .db
        .call(|c| {
            Ok((
                emails::count(c)?,
                drafts::counts(c)?,
                qdb::depth(c)?,
                qdb::dead_count(c)?,
            ))
        })
        .await?;
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "llm_provider": app.llm.provider_name(),
        "llm_model": app.llm.model_name(),
        "queue_depth": depth,
        "emails_processed": processed,
        "drafts_created": counts.total,
        "drafts_pending": counts.pending,
        "review_required": counts.review_required,
        "failures": dead + counts.failed,
        "telegram_enabled": app.notifier.enabled(),
        "job_agent_enabled": app.cfg.jobs.enabled,
        "research_enabled": app.cfg.research.enabled,
        "uptime_secs": app.started_at.elapsed().as_secs(),
    })))
}

pub async fn dashboard_data(State(app): State<Arc<App>>) -> AppResult<Json<Value>> {
    let data = app
        .db
        .call(|c| {
            let pipeline = jdb::pipeline_counts(c)?;
            let apps = jdb::list_applications(c, None, 100)?;
            let jobs = jdb::list_jobs(c, 50, None)?;
            let recruiters = jdb::list_recruiters(c, None, 50)?;
            let questions = jdb::open_questions(c, None)?;
            let interviews = jdb::interview_count(c)?;
            let drafts = drafts::counts(c)?;
            let emails = emails::count(c)?;
            let mut companies: std::collections::BTreeMap<String, std::collections::BTreeMap<String, i64>> = Default::default();
            for a in &apps {
                *companies
                    .entry(a.company.clone().unwrap_or_else(|| "-".into()))
                    .or_default()
                    .entry(a.status.clone())
                    .or_default() += 1;
            }
            Ok(json!({
                "pipeline": pipeline,
                "interviews": interviews,
                "drafts": drafts,
                "emails_processed": emails,
                "applications": apps.iter().map(|a| json!({
                    "id": a.id, "company": a.company, "role": a.role, "url": a.url, "status": a.status,
                    "step": a.current_step, "blocker": a.blocker_kind, "blocker_detail": a.blocker_detail,
                    "score": a.score, "applied_at": a.applied_at, "updated_at": a.updated_at,
                })).collect::<Vec<_>>(),
                "jobs": jobs.iter().map(|j| json!({
                    "id": j.id, "title": j.details.title, "company": j.details.company, "url": j.details.url,
                    "location": j.details.location, "remote": j.details.remote, "salary": j.details.salary,
                    "score": j.score, "decision": j.decision, "status": j.status, "source": j.source,
                    "discovered_at": j.discovered_at,
                })).collect::<Vec<_>>(),
                "recruiters": recruiters,
                "questions": questions,
                "company_pipeline": companies,
            }))
        })
        .await?;
    Ok(Json(data))
}

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    company: Option<String>,
    #[serde(default)]
    entity_id: Option<String>,
}

pub async fn list_jobs(
    State(app): State<Arc<App>>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Value>> {
    let limit = q.limit.unwrap_or(50).min(500);
    let jobs = app.db.call(move |c| jdb::list_jobs(c, limit, None)).await?;
    Ok(Json(json!({ "jobs": jobs })))
}

pub async fn list_applications(
    State(app): State<Arc<App>>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Value>> {
    let limit = q.limit.unwrap_or(100).min(500);
    let apps = app
        .db
        .call(move |c| jdb::list_applications(c, q.status.as_deref(), limit))
        .await?;
    Ok(Json(json!({ "applications": apps })))
}

#[derive(Deserialize)]
pub struct AnswerBody {
    question_id: String,
    answer: String,
}

pub async fn answer_application(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Json(b): Json<AnswerBody>,
) -> AppResult<Json<Value>> {
    if b.answer.len() > 4000 {
        return Err(AppError::Validation("answer too long".into()));
    }
    let qid = b.question_id.clone();
    let q = app.db.call(move |c| jdb::get_question(c, &qid)).await?;
    if q.application_id.as_deref() != Some(id.as_str()) {
        return Err(AppError::Validation(
            "question does not belong to this application".into(),
        ));
    }
    let reply = crate::jobs::application::handle_human_answer(&app, &q, &b.answer).await?;
    Ok(Json(json!({ "message": reply })))
}

pub async fn resume_application(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let id2 = id.clone();
    let a = app.db.call(move |c| jdb::get_application(c, &id2)).await?;
    if a.status == "submitted" || a.status == "skipped" {
        return Err(AppError::Validation(format!("application is {}", a.status)));
    }
    let id2 = id.clone();
    app.db
        .call(move |c| {
            jdb::cancel_questions(c, &id2)?;
            jdb::update_application(
                c,
                &id2,
                &jdb::AppUpdate {
                    status: Some("pending"),
                    current_step: Some("manual_resume"),
                    blocker: Some(None),
                    ..Default::default()
                },
            )
        })
        .await?;
    crate::jobs::application::enqueue_run(&app, &id).await?;
    Ok(Json(json!({ "status": "queued" })))
}

pub async fn list_recruiters(
    State(app): State<Arc<App>>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Value>> {
    let limit = q.limit.unwrap_or(100).min(500);
    let r = app
        .db
        .call(move |c| jdb::list_recruiters(c, q.company.as_deref(), limit))
        .await?;
    Ok(Json(json!({ "recruiters": r })))
}

pub async fn run_research(State(app): State<Arc<App>>) -> AppResult<(StatusCode, Json<Value>)> {
    let key = format!("research:manual:{}", crate::db::new_id());
    let id = app
        .queue
        .enqueue(queue::KIND_RESEARCH, key, None, None, 1)
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "job_id": id, "status": "queued" })),
    ))
}

pub async fn audit_log(
    State(app): State<Arc<App>>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Value>> {
    let limit = q.limit.unwrap_or(100).min(1000);
    let entries = app
        .db
        .call(move |c| audit::recent(c, limit, q.entity_id.as_deref()))
        .await?;
    Ok(Json(json!({ "entries": entries })))
}

// ------------------------------------------------------------- operational

pub async fn health(State(app): State<Arc<App>>) -> Json<Value> {
    let db_ok = app.db.ping().await;
    let llm_ok = app.llm.health().await;
    let browser = match &app.browser {
        None => "disabled",
        Some(b) => {
            if b.health().await {
                "ok"
            } else {
                "down"
            }
        }
    };
    Json(json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": app.started_at.elapsed().as_secs(),
        "checks": {
            "database": if db_ok { "ok" } else { "down" },
            "llm": if llm_ok { "ok" } else { "degraded" },
            "llm_provider": app.llm.provider_name(),
            "browser_worker": browser,
            "telegram": if app.notifier.enabled() { "ok" } else { "disabled" },
        }
    }))
}

pub async fn readiness(State(app): State<Arc<App>>) -> (StatusCode, Json<Value>) {
    if app.db.ping().await {
        (StatusCode::OK, Json(json!({ "ready": true })))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "ready": false })),
        )
    }
}

pub async fn metrics(State(app): State<Arc<App>>) -> impl IntoResponse {
    if let Ok(d) = app.db.call(|c| qdb::depth(c)).await {
        app.metrics.queue_depth.set(d);
    }
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        app.metrics.render(),
    )
}

pub async fn dashboard_page() -> impl IntoResponse {
    (
        [(
            header::CONTENT_SECURITY_POLICY,
            "default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:",
        )],
        Html(include_str!("../../static/dashboard.html")),
    )
}
