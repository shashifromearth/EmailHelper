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
    body: Result<Json<EmailIn>, axum::extract::rejection::JsonRejection>,
) -> AppResult<Response> {
    let mut e = match body {
        Ok(Json(e)) => e,
        Err(r) => {
            // Malformed or oversized payloads are otherwise rejected without a trace.
            tracing::warn!(status = r.status().as_u16(), reason = %r.body_text(), "email payload rejected");
            app.metrics.failure("api_payload");
            return Ok(r.into_response());
        }
    };
    // Excluded senders (banks etc.) never enter the system: nothing is stored,
    // classified or sent to a model.
    if eh_knowledge::text::SenderFilter::new(&app.cfg.excluded_senders).excludes(&e.from) {
        let domain = emails::email_address(&e.from).rsplit_once('@').map(|(_, d)| d.to_string()).unwrap_or_default();
        tracing::info!(sender_domain = %domain, "email from an excluded sender dropped");
        app.metrics.emails_excluded_total.inc();
        return Ok((StatusCode::OK, Json(json!({ "status": "excluded" }))).into_response());
    }
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
        let mut list = app.db.call(|c| drafts::claim_pending(c, 10)).await?;
        for d in &mut list {
            d.attachments = load_attachments(app.knowledge.dir(), &d.attachment_paths, &d.draft_id).await;
        }
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

/// Largest file sent with a draft.
const MAX_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;

/// Read a draft's attachment files. Only files inside the knowledge folder
/// are sent; anything else is skipped with a warning.
async fn load_attachments(knowledge: &std::path::Path, paths: &[String], draft_id: &str) -> Vec<drafts::DraftAttachment> {
    use base64::Engine;
    let Ok(root) = tokio::fs::canonicalize(knowledge).await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for p in paths {
        let Ok(real) = tokio::fs::canonicalize(p).await else {
            tracing::warn!(draft = draft_id, file = %p, "attachment missing; draft goes without it");
            continue;
        };
        if !real.starts_with(&root) {
            tracing::warn!(draft = draft_id, file = %p, "attachment outside the knowledge folder; skipped");
            continue;
        }
        match tokio::fs::metadata(&real).await {
            Ok(m) if m.len() <= MAX_ATTACHMENT_BYTES => {}
            _ => {
                tracing::warn!(draft = draft_id, file = %p, "attachment too large or unreadable; skipped");
                continue;
            }
        }
        let Ok(bytes) = tokio::fs::read(&real).await else {
            continue;
        };
        let name = real
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "attachment".into());
        out.push(drafts::DraftAttachment {
            content_type: content_type_for(&name).into(),
            name,
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
    }
    out
}

fn content_type_for(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "doc" => "application/msword",
        "rtf" => "application/rtf",
        "md" => "text/markdown",
        "txt" => "text/plain",
        _ => "application/octet-stream",
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
        drafts::AckOutcome::Saved => {
            if let Some(w) = b.error.as_deref().filter(|w| !w.trim().is_empty()) {
                tracing::warn!(draft = %id, warning = w, "draft saved with a warning");
                app.notifier
                    .notify(
                        crate::telegram::Event::ProcessingFailed,
                        &format!("⚠️ Draft saved, but: {w}. Please attach the file yourself before sending."),
                    )
                    .await;
            }
            "draft_saved"
        }
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
    let research_today = crate::research::discovery::ran_today(&app).await;
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
        // the Thunderbird extension skips these senders before reading the mail
        "excluded_senders": app.cfg.excluded_senders,
        "research_ran_today": research_today.0,
        "draft_quality_7d": crate::eval::live::summary(&app, 7).await.ok(),
        "research_last_run": research_today.1,
        "uptime_secs": app.started_at.elapsed().as_secs(),
    })))
}

pub async fn dashboard_data(State(app): State<Arc<App>>) -> AppResult<Json<Value>> {
    let people = app.knowledge.people();
    let names: std::collections::BTreeMap<String, String> = people
        .persons
        .iter()
        .map(|p| (p.id.clone(), p.name.clone()))
        .collect();
    let multi = !people.legacy;
    let data = app
        .db
        .call(move |c| {
            let pipeline = jdb::pipeline_counts(c)?;
            let apps = jdb::list_applications(c, None, 100)?;
            let jobs = jdb::list_jobs(c, 50, None)?;
            let recruiters = jdb::list_recruiters(c, None, 50)?;
            let questions = jdb::open_questions(c, None)?;
            let interviews = jdb::interview_count(c)?;
            let drafts = drafts::counts(c)?;
            let emails = emails::count(c)?;
            let unassigned = if multi { emails::unassigned_mailboxes(c)? } else { Vec::new() };
            let app_person: std::collections::BTreeMap<String, Option<String>> =
                apps.iter().map(|a| (a.id.clone(), a.person_id.clone())).collect();
            let person_name = |id: Option<&str>| -> String {
                id.map(|i| names.get(i).cloned().unwrap_or_else(|| i.to_string())).unwrap_or_default()
            };
            let questions: Vec<Value> = questions
                .iter()
                .map(|q| {
                    let pid = match q.field_id.as_deref().and_then(|f| f.strip_prefix("fact:")) {
                        Some(rest) => rest.split(':').next().map(str::to_string),
                        None => q
                            .application_id
                            .as_ref()
                            .and_then(|a| app_person.get(a).cloned().flatten()),
                    };
                    let mut v = serde_json::to_value(q).unwrap_or(Value::Null);
                    v["person"] = Value::String(person_name(pid.as_deref()));
                    v
                })
                .collect();
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
                    "person": person_name(a.person_id.as_deref()), "resume": a.resume_file,
                })).collect::<Vec<_>>(),
                "unassigned_mailboxes": unassigned,
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

#[derive(Deserialize)]
pub struct SubmitJobBody {
    url: String,
    /// Start an application right away (still subject to submit mode, the
    /// approval gate and blockers). Requires `JOB_AGENT_ENABLED`.
    #[serde(default)]
    apply: bool,
    /// Person id from people.json (the default person when omitted).
    #[serde(default)]
    person: Option<String>,
}

/// Add a job by URL (e.g. one you found yourself): open it, extract details,
/// score it against the profile, store it, and optionally start applying.
pub async fn submit_job(
    State(app): State<Arc<App>>,
    Json(b): Json<SubmitJobBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    let url = url::Url::parse(b.url.trim())
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .ok_or_else(|| AppError::Validation("url must be an absolute http(s) URL".into()))?
        .to_string();
    if b.apply && !app.cfg.jobs.enabled {
        return Err(AppError::Validation(
            "apply=true requires JOB_AGENT_ENABLED=true".into(),
        ));
    }
    let people = app.knowledge.people();
    let person = match b.person.as_deref() {
        Some(id) => people
            .person(id)
            .ok_or_else(|| AppError::Validation(format!("unknown person {id}")))?,
        None => people.default_person(),
    };
    let page = crate::jobs::fetch_page(&app, &url).await?;
    let mut details = crate::jobs::extract::parse_job(&page, &person.profile.all_skills());
    details.url = Some(url.clone());
    if details.title.trim().is_empty() {
        return Err(AppError::Validation(
            "could not find a job title on that page".into(),
        ));
    }
    let (job, decision, is_new) =
        crate::jobs::score_and_store(&app, &person, "manual", None, details).await?;
    app.audit(
        "api",
        "job_submitted",
        "job",
        &job.id,
        json!({ "url": url, "score": job.score, "decision": decision.as_str() }),
    )
    .await;
    let application_id = if b.apply {
        Some(crate::jobs::application::start(&app, &job, &url).await?)
    } else {
        None
    };
    Ok((
        if is_new {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(json!({
            "job": job,
            "decision": decision.as_str(),
            "application_id": application_id,
        })),
    ))
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

#[derive(Deserialize)]
pub struct QuestionAnswerBody {
    answer: String,
}

/// Answer any open question (application or person fact) - same effect as a
/// Telegram reply.
pub async fn answer_question(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Json(b): Json<QuestionAnswerBody>,
) -> AppResult<Json<Value>> {
    if b.answer.len() > 4000 {
        return Err(AppError::Validation("answer too long".into()));
    }
    let q = app.db.call(move |c| jdb::get_question(c, &id)).await?;
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

/// Chrome extension: answers for the fields of an application form.
pub async fn apply_answers(
    State(app): State<Arc<App>>,
    Json(req): Json<crate::jobs::assist::AnswersRequest>,
) -> AppResult<Json<crate::jobs::assist::AnswersResponse>> {
    Ok(Json(crate::jobs::assist::answer(&app, req).await?))
}

/// Chrome extension: answers the person gave on a form, for next time.
pub async fn apply_remember(
    State(app): State<Arc<App>>,
    Json(req): Json<crate::jobs::assist::RememberRequest>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!({ "remembered": crate::jobs::assist::remember(&app, req).await? })))
}

/// Chrome extension: stuck on a page; ask the person over Telegram.
pub async fn apply_escalate(
    State(app): State<Arc<App>>,
    Json(req): Json<crate::db::apply::EscalationIn>,
) -> AppResult<Json<Value>> {
    let (e, telegram) = crate::jobs::feedback::escalate(&app, req).await?;
    Ok(Json(json!({ "id": e.id, "telegram": telegram })))
}

/// Chrome extension: poll an escalation for the person's reply.
pub async fn apply_escalation(State(app): State<Arc<App>>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let e = app.db.call(move |c| crate::db::apply::get_escalation(c, &id)).await?;
    Ok(Json(crate::jobs::feedback::view(&e)))
}

#[derive(Deserialize)]
pub struct ResolveBody {
    status: String,
}

/// Chrome extension: the page got fixed (`done`) or it gave up (`skip`).
pub async fn apply_resolve(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Json(b): Json<ResolveBody>,
) -> AppResult<Json<Value>> {
    let e = crate::jobs::feedback::resolve(&app, id, b.status).await?;
    Ok(Json(crate::jobs::feedback::view(&e)))
}

#[derive(Deserialize)]
pub struct EventsBody {
    events: Vec<crate::db::apply::EventIn>,
}

/// Chrome extension: every failure (and outcome), for the feedback loop.
pub async fn apply_events(State(app): State<Arc<App>>, Json(b): Json<EventsBody>) -> AppResult<Json<Value>> {
    Ok(Json(json!({ "stored": crate::jobs::feedback::record_events(&app, b.events).await? })))
}

#[derive(Deserialize)]
pub struct DaysQuery {
    #[serde(default)]
    days: Option<i64>,
}

/// Dashboard: what keeps failing in the applier, and where.
pub async fn apply_events_summary(State(app): State<Arc<App>>, Query(q): Query<DaysQuery>) -> AppResult<Json<Value>> {
    Ok(Json(crate::jobs::feedback::summary(&app, q.days.unwrap_or(7)).await?))
}

/// Dashboard: start a knowledge index run on the knowledge service.
pub async fn knowledge_reindex(State(app): State<Arc<App>>) -> AppResult<(StatusCode, Json<Value>)> {
    let r = app
        .knowledge
        .remote()
        .ok_or_else(|| AppError::Validation("the knowledge service is not configured (KNOWLEDGE_SERVICE_URL)".into()))?;
    Ok((StatusCode::ACCEPTED, Json(r.reindex().await?)))
}

/// Dashboard: the shared job database's counts by status and platform.
pub async fn job_links_status(State(app): State<Arc<App>>) -> AppResult<Json<Value>> {
    if !app.cfg.jobs.links_enabled {
        return Err(AppError::Validation("the job-links service is off (JOB_LINKS_ENABLED)".into()));
    }
    Ok(Json(crate::jobs::linkqueue::LinkQueueClient::from_app(&app).stats().await?))
}

/// Dashboard: index size, embedding coverage and the last run's report.
pub async fn knowledge_status(State(app): State<Arc<App>>) -> AppResult<Json<Value>> {
    let r = app
        .knowledge
        .remote()
        .ok_or_else(|| AppError::Validation("the knowledge service is not configured (KNOWLEDGE_SERVICE_URL)".into()))?;
    Ok(Json(r.status().await?))
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

/// "ok" when `url` answers 2xx within 2 s, else "down".
async fn probe(app: &App, url: String) -> &'static str {
    match app.http.get(url).timeout(Duration::from_secs(2)).send().await {
        Ok(r) if r.status().is_success() => "ok",
        _ => "down",
    }
}

pub async fn health(State(app): State<Arc<App>>) -> Json<Value> {
    let db_ok = app.db.ping().await;
    let llm_ok = app.llm.health().await;
    // Optional services: the agent keeps working without them.
    let (knowledge, links) = tokio::join!(
        async {
            match app.knowledge.remote() {
                Some(r) => probe(&app, format!("{}/readyz", r.base())).await,
                None => "off",
            }
        },
        async {
            if app.cfg.jobs.links_enabled {
                probe(&app, format!("{}/livez", app.cfg.jobs.links_url.trim_end_matches('/'))).await
            } else {
                "off"
            }
        }
    );
    let ok = db_ok && knowledge != "down" && links != "down";
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
        "status": if ok { "ok" } else { "degraded" },
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": app.started_at.elapsed().as_secs(),
        "checks": {
            "database": if db_ok { "ok" } else { "down" },
            "llm": if llm_ok { "ok" } else { "degraded" },
            "llm_provider": app.llm.provider_name(),
            "browser_worker": browser,
            "telegram": if app.notifier.enabled() { "ok" } else { "disabled" },
            "knowledge_service": knowledge,
            "job_links": links,
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
