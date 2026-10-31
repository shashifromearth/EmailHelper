//! Job application workflow (a resumable state machine persisted in SQLite).
//!
//! ```text
//! pending → running ─┬─ inspect form ─ blocker? ──────────────► blocked (ask human, wait)
//!                    ├─ answer fields ─ unknown? ─────────────► awaiting_human (ask, wait)
//!                    ├─ review decision & not approved ───────► awaiting_approval (ask, wait)
//!                    ├─ fill + upload (+ submit when allowed) ─► submitted
//!                    └─ dry run ──────────────────────────────► ready
//! human reply ─► answers stored ─► (no open questions) ─► re-queued ─► running …
//! ```
//!
//! Safety: never submits when the score is below threshold, required answers
//! are missing, validation failed, or a CAPTCHA/OTP is unresolved.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Map, Value};

use crate::app::App;
use crate::config::SubmitMode;
use crate::db::jobs::{self as jdb, AppUpdate, ApplicationRecord, HumanQuestion, JobRecord};
use crate::error::{AppError, AppResult};
use crate::jobs::answers::{self, Answer};
use crate::jobs::browser::{Blocker, Field};
use crate::jobs::cover_letter;
use crate::knowledge::Person;
use crate::queue;
use crate::telegram::{format, Event};

pub const FIELD_APPROVE: &str = "__approve__";
pub const FIELD_BLOCKER: &str = "__blocker__";

const TERMINAL: &[&str] = &["submitted", "skipped", "failed"];

/// Why the profile can't be used for real applications, if it can't.
pub fn profile_problem(p: &crate::knowledge::Profile) -> Option<String> {
    let email = p.identity.email.trim().to_ascii_lowercase();
    if p.identity.full_name.trim().is_empty() || email.is_empty() {
        return Some("profile.json is missing your name or email".into());
    }
    let domain = email.rsplit('@').next().unwrap_or("");
    if ["example.com", "example.org", "example.net"].contains(&domain) {
        return Some("knowledge/ still contains the sample profile; replace it with your own before applying".into());
    }
    None
}

/// The person an application/job belongs to.
fn person_for(app: &App, person_id: Option<&str>) -> AppResult<Arc<Person>> {
    crate::jobs::person_by_id(app, person_id).ok_or_else(|| {
        AppError::Validation(format!(
            "person {} is not in knowledge/people.json",
            person_id.unwrap_or("-")
        ))
    })
}

/// Create (or reuse) the application for a job and queue it.
pub async fn start(app: &Arc<App>, job: &JobRecord, url: &str) -> AppResult<String> {
    let person = person_for(app, job.person_id.as_deref())?;
    if let Some(problem) = profile_problem(&person.profile) {
        tracing::warn!(job_id = %job.id, reason = %problem, "not starting application");
        return Err(AppError::Validation(problem));
    }
    let (job2, url2) = (job.clone(), url.to_string());
    let (id, created) = app
        .db
        .call(move |c| jdb::create_application(c, &job2, &url2))
        .await?;
    if created {
        app.metrics.applications_started_total.inc();
        let jid = job.id.clone();
        app.db
            .call(move |c| jdb::set_job_status(c, &jid, "applying"))
            .await?;
        app.audit(
            "job_agent",
            "application_created",
            "application",
            &id,
            json!({ "job_id": job.id, "url": url, "score": job.score }),
        )
        .await;
    }
    enqueue_run(app, &id).await?;
    Ok(id)
}

pub async fn enqueue_run(app: &Arc<App>, id: &str) -> AppResult<()> {
    let id2 = id.to_string();
    let seq = app.db.call(move |c| jdb::next_run_seq(c, &id2)).await?;
    app.queue
        .enqueue(
            queue::KIND_APPLICATION,
            format!("application:{id}:run:{seq}"),
            Some(id.to_string()),
            None,
            app.cfg.jobs.max_attempts,
        )
        .await?;
    Ok(())
}

/// After a restart, re-queue applications that were mid-flight without a live
/// queue entry (the queue itself already recovers its own rows).
pub async fn recover(app: &Arc<App>) -> AppResult<usize> {
    let running = app
        .db
        .call(|c| jdb::list_applications(c, Some("running"), 500))
        .await?;
    let mut n = 0;
    for a in running {
        let key_prefix = format!("application:{}:run:{}", a.id, a.run_seq);
        let exists = app
            .db
            .call(move |c| crate::db::queue::exists_key(c, &key_prefix))
            .await?;
        if !exists {
            enqueue_run(app, &a.id).await?;
            n += 1;
        }
    }
    if n > 0 {
        tracing::info!(count = n, "resumed interrupted applications");
    }
    Ok(n)
}

struct InProgress<'a>(&'a App);
impl<'a> InProgress<'a> {
    fn new(app: &'a App) -> Self {
        app.metrics.applications_in_progress.inc();
        Self(app)
    }
}
impl Drop for InProgress<'_> {
    fn drop(&mut self) {
        self.0.metrics.applications_in_progress.dec();
    }
}

async fn update(app: &App, id: &str, u: AppUpdate<'static>) -> AppResult<()> {
    let id = id.to_string();
    app.db
        .call(move |c| jdb::update_application(c, &id, &u))
        .await
}

/// Run one step of the workflow for application `id`.
pub async fn run(app: &Arc<App>, id: &str) -> AppResult<()> {
    let started = Instant::now();
    let id_s = id.to_string();
    let a = app.db.call(move |c| jdb::get_application(c, &id_s)).await?;
    if TERMINAL.contains(&a.status.as_str()) {
        return Ok(());
    }
    let open = {
        let id_s = id.to_string();
        app.db
            .call(move |c| jdb::open_questions(c, Some(&id_s)))
            .await?
    };
    if !open.is_empty() {
        tracing::info!(
            application_id = id,
            open = open.len(),
            "waiting for human answers"
        );
        return Ok(());
    }
    let jid = a.job_id.clone();
    let job = app.db.call(move |c| jdb::get_job(c, &jid)).await?;
    let person = person_for(app, a.person_id.as_deref().or(job.person_id.as_deref()))?;
    let resume = choose_resume(app, &person, &job);
    if let Some(name) = &resume.file_name {
        if a.resume_file.as_deref() != Some(name.as_str()) {
            let (id2, n2) = (id.to_string(), name.clone());
            app.db
                .call(move |c| {
                    jdb::update_application(
                        c,
                        &id2,
                        &AppUpdate {
                            resume_file: Some(&n2),
                            ..Default::default()
                        },
                    )
                })
                .await?;
        }
    }
    let Some(browser) = app.browser.clone() else {
        return Err(AppError::other(
            "browser worker is not configured (JOB_AGENT_ENABLED / BROWSER_WORKER_URL)",
        ));
    };
    let _guard = InProgress::new(app);
    let mode = app.cfg.jobs.submit_mode;
    let decision = job.decision.clone().unwrap_or_else(|| "review".into());

    update(
        app,
        id,
        AppUpdate {
            status: Some("running"),
            current_step: Some("inspecting"),
            bump_attempts: true,
            ..Default::default()
        },
    )
    .await?;

    // Approval gate for mid-score jobs before touching the site.
    if decision == "review" && !a.approved && mode != SubmitMode::DryRun {
        ask_approval(
            app,
            &a,
            &job,
            "Score is in the review band",
            &resume.describe(),
        )
        .await?;
        return Ok(());
    }

    // 1. Inspect the application form.
    let insp = match browser.inspect(id, &a.url).await {
        Ok(r) => r,
        Err(e) => {
            app.metrics
                .playwright_failures_total
                .with_label_values(&["inspect"])
                .inc();
            return Err(e);
        }
    };
    if let Some(b) = insp.blocker.clone() {
        return handle_blocker(app, &a, &job, b).await;
    }
    if insp.fields.is_empty() {
        return handle_blocker(
            app,
            &a,
            &job,
            Blocker {
                kind: "unexpected_form".into(),
                detail: "no fillable fields found".into(),
            },
        )
        .await;
    }
    let fields_json = serde_json::to_value(&insp.fields)?;
    {
        let (id2, fu, ats) = (id.to_string(), insp.final_url.clone(), insp.ats.clone());
        app.db
            .call(move |c| {
                jdb::update_application(
                    c,
                    &id2,
                    &AppUpdate {
                        current_step: Some("answering"),
                        page_url: Some(&fu),
                        ats: Some(&ats),
                        fields: Some(&fields_json),
                        blocker: Some(None),
                        ..Default::default()
                    },
                )
            })
            .await?;
    }

    // 2. Answer fields (persisting partial answers as we go).
    let profile = person.profile.clone();
    let scope =
        crate::knowledge::Scope::for_person(&person, Some(crate::knowledge::people::JOB_SEEKING));
    let memory_ns = memory_ns(&person);
    let job_ctx = format!(
        "{} at {}\nLocation: {}\n{}",
        job.details.title,
        job.details.company.as_deref().unwrap_or(""),
        job.details.location.as_deref().unwrap_or(""),
        job.details.description.as_deref().unwrap_or("")
    );
    let mut answers: Map<String, Value> = a.answers.clone();
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    let mut cover = a.cover_letter.clone();
    let mut asks: Vec<(Field, String)> = Vec::new();
    for f in &insp.fields {
        if answers.get(&f.id).is_some_and(|v| !v.is_null() && v != "") {
            continue;
        }
        let ans = answers::answer_field(
            f,
            &profile,
            &app.db,
            &app.knowledge,
            &app.llm,
            &job_ctx,
            &scope,
            &memory_ns,
        )
        .await?;
        match ans {
            Answer::Value(v) => {
                answers.insert(f.id.clone(), v);
            }
            Answer::ResumeFile => {
                let path = resume.path.clone();
                if !path.exists() {
                    return handle_blocker(
                        app,
                        &a,
                        &job,
                        Blocker {
                            kind: "upload_failure".into(),
                            detail: format!("resume file not found at {}", path.display()),
                        },
                    )
                    .await;
                }
                files.insert(f.id.clone(), path.to_string_lossy().to_string());
            }
            Answer::CoverLetter => {
                if cover.is_none() {
                    cover = Some(
                        cover_letter::generate(
                            &app.llm,
                            &app.knowledge,
                            &job.details,
                            &profile,
                            &scope,
                        )
                        .await,
                    );
                }
                answers.insert(
                    f.id.clone(),
                    Value::String(cover.clone().unwrap_or_default()),
                );
            }
            Answer::Skip => {}
            Answer::Ask { reason } => asks.push((f.clone(), reason)),
        }
    }
    {
        let (id2, ans2, cov2) = (id.to_string(), answers.clone(), cover.clone());
        app.db
            .call(move |c| {
                jdb::update_application(
                    c,
                    &id2,
                    &AppUpdate {
                        answers: Some(&ans2),
                        cover_letter: cov2.as_deref(),
                        ..Default::default()
                    },
                )
            })
            .await?;
    }
    if !asks.is_empty() {
        return ask_fields(app, &a, &job, &asks).await;
    }

    // 3. Fill (and submit when allowed).
    let submit = match mode {
        SubmitMode::DryRun => false,
        SubmitMode::Auto => decision == "apply" || a.approved,
        SubmitMode::Review => a.approved,
    };
    update(
        app,
        id,
        AppUpdate {
            current_step: Some(if submit { "submitting" } else { "filling" }),
            ..Default::default()
        },
    )
    .await?;
    let res = match browser.fill(id, &a.url, &answers, &files, submit).await {
        Ok(r) => r,
        Err(e) => {
            app.metrics
                .playwright_failures_total
                .with_label_values(&["fill"])
                .inc();
            return Err(e);
        }
    };
    let uploaded = !files.is_empty()
        && res
            .blocker
            .as_ref()
            .map(|b| b.kind != "upload_failure")
            .unwrap_or(true);
    if let Some(b) = res.blocker.clone() {
        if uploaded {
            update(
                app,
                id,
                AppUpdate {
                    resume_uploaded: Some(true),
                    ..Default::default()
                },
            )
            .await?;
        }
        return handle_blocker(app, &a, &job, b).await;
    }
    if !res.missing_required.is_empty() {
        let fields: Vec<Field> = insp.fields.clone();
        let asks: Vec<(Field, String)> = res
            .missing_required
            .iter()
            .map(|fid| {
                let f = fields
                    .iter()
                    .find(|f| &f.id == fid)
                    .cloned()
                    .unwrap_or(Field {
                        id: fid.clone(),
                        label: fid.clone(),
                        kind: "text".into(),
                        required: true,
                        options: vec![],
                        accept: None,
                    });
                (f, "required field still empty after filling".to_string())
            })
            .collect();
        return ask_fields(app, &a, &job, &asks).await;
    }
    let shot = res.screenshot_path.clone();
    match res.status.as_str() {
        "submitted" => {
            let now = crate::db::now();
            let conf = res.confirmation_text.clone().unwrap_or_default();
            let (id2, fu, shot2) = (id.to_string(), res.final_url.clone(), shot.clone());
            app.db
                .call(move |c| {
                    jdb::update_application(
                        c,
                        &id2,
                        &AppUpdate {
                            status: Some("submitted"),
                            current_step: Some("done"),
                            page_url: Some(&fu),
                            resume_uploaded: Some(uploaded),
                            confirmation_text: Some(&conf),
                            screenshot_path: shot2.as_deref(),
                            applied_at: Some(&now),
                            ..Default::default()
                        },
                    )
                })
                .await?;
            let jid = job.id.clone();
            app.db
                .call(move |c| jdb::set_job_status(c, &jid, "applied"))
                .await?;
            app.metrics.applications_completed_total.inc();
            app.metrics
                .processing_duration_seconds
                .with_label_values(&["job_application"])
                .observe(started.elapsed().as_secs_f64());
            app.audit(
                "job_agent",
                "application_submitted",
                "application",
                id,
                json!({ "company": job.details.company, "role": job.details.title, "url": a.url,
                        "final_url": res.final_url, "score": job.score, "mode": mode.as_str(),
                        "answers": answers, "files": files, "confirmation": res.confirmation_text,
                        "screenshot": shot }),
            )
            .await;
            let (tag, chat) = who(app, &a);
            app.notifier
                .notify_person(
                    chat,
                    Event::ApplicationSubmitted,
                    &format!(
                        "{tag}\n{}\nResume: {}",
                        format::submitted(
                            job.details.company.as_deref().unwrap_or("-"),
                            &job.details.title,
                            &a.url,
                            job.score,
                        ),
                        resume.file_name.as_deref().unwrap_or("-")
                    ),
                )
                .await;
            Ok(())
        }
        "filled" => {
            update(
                app,
                id,
                AppUpdate {
                    resume_uploaded: Some(uploaded),
                    ..Default::default()
                },
            )
            .await?;
            if mode == SubmitMode::DryRun {
                let id2 = id.to_string();
                app.db
                    .call(move |c| {
                        jdb::update_application(
                            c,
                            &id2,
                            &AppUpdate {
                                status: Some("ready"),
                                current_step: Some("dry_run_filled"),
                                screenshot_path: shot.as_deref(),
                                ..Default::default()
                            },
                        )
                    })
                    .await?;
                app.audit(
                    "job_agent",
                    "application_dry_run",
                    "application",
                    id,
                    json!({ "answers": answers, "files": files }),
                )
                .await;
                let (tag, chat) = who(app, &a);
                app.notifier
                    .notify_person(chat, Event::ApprovalRequired, &format!("{tag}\n🧪 Dry run: form filled (not submitted)\nCompany: {}\nRole: {}\nURL: {}", job.details.company.as_deref().unwrap_or("-"), job.details.title, a.url))
                    .await;
                Ok(())
            } else {
                // Review mode (or auto-mode below threshold): ask before submitting.
                let a2 = ApplicationRecord {
                    approved: false,
                    ..a.clone()
                };
                ask_approval(
                    app,
                    &a2,
                    &job,
                    "Form filled and validated; ready to submit",
                    &resume.describe(),
                )
                .await
            }
        }
        other => {
            app.metrics
                .playwright_failures_total
                .with_label_values(&["fill"])
                .inc();
            Err(AppError::other(format!(
                "browser worker returned status {other}"
            )))
        }
    }
}

/// The resume chosen for an application and why.
struct ResumeChoice {
    path: std::path::PathBuf,
    file_name: Option<String>,
    matched: Vec<String>,
}

impl ResumeChoice {
    fn describe(&self) -> String {
        match (&self.file_name, self.matched.is_empty()) {
            (Some(f), false) => format!("Resume: {f} (matched {})", self.matched.join(", ")),
            (Some(f), true) => format!("Resume: {f} (default)"),
            (None, _) => String::new(),
        }
    }
}

/// Pick the person's resume whose tech stack best matches the job, falling
/// back to `resume.json`'s file and then `JOB_RESUME_PATH`.
fn choose_resume(app: &App, person: &Person, job: &JobRecord) -> ResumeChoice {
    let mut vocab = person.profile.all_skills();
    for r in &person.resumes {
        for t in r.name_tags.iter().chain(r.tags.iter()) {
            if !vocab.iter().any(|v| v.eq_ignore_ascii_case(t)) {
                vocab.push(t.clone());
            }
        }
    }
    let mut d = job.details.clone();
    let text = format!("{}\n{}", d.title, d.description.clone().unwrap_or_default());
    crate::jobs::extract::enrich_from_text(&mut d, &text, &vocab);
    if let Some((r, matched)) = person.select_resume(&d.skills, Some(crate::knowledge::people::JOB_SEEKING)) {
        return ResumeChoice {
            path: std::fs::canonicalize(&r.path).unwrap_or_else(|_| r.path.clone()),
            file_name: Some(r.file_name.clone()),
            matched,
        };
    }
    let p = &person.profile;
    if !p.resume.resume_file.is_empty() {
        let candidate = std::path::PathBuf::from(&p.resume.resume_file);
        let resolved = if candidate.is_absolute() {
            candidate
        } else {
            person.dir.join(candidate)
        };
        if resolved.exists() {
            return ResumeChoice {
                file_name: resolved
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string()),
                path: std::fs::canonicalize(&resolved).unwrap_or(resolved),
                matched: Vec::new(),
            };
        }
    }
    let p = app.cfg.jobs.resume_path.clone();
    ResumeChoice {
        file_name: p.file_name().map(|n| n.to_string_lossy().to_string()),
        path: std::fs::canonicalize(&p).unwrap_or(p),
        matched: Vec::new(),
    }
}

/// Namespace for remembered form answers ("" in legacy single-person mode).
pub(crate) fn memory_ns(person: &Person) -> String {
    if person.rel_dir.is_empty() {
        String::new()
    } else {
        format!("{}:", person.id)
    }
}

/// Telegram tag and chat for an application's person.
fn who(app: &App, a: &ApplicationRecord) -> (String, Option<i64>) {
    match crate::jobs::person_by_id(app, a.person_id.as_deref()) {
        Some(p) => (format::person_tag(&p.name, None), p.telegram_chat_id),
        None => (String::new(), None),
    }
}

async fn create_and_send_question(
    app: &App,
    a: &ApplicationRecord,
    job: &JobRecord,
    field_id: &str,
    question: &str,
    reason: &str,
    options: &[String],
) -> AppResult<HumanQuestion> {
    let (aid, fid, q, r, o) = (
        a.id.clone(),
        field_id.to_string(),
        question.to_string(),
        reason.to_string(),
        options.to_vec(),
    );
    let (hq, created) = app
        .db
        .call(move |c| jdb::create_question(c, &aid, &fid, &q, &r, &o))
        .await?;
    if created {
        let mut text = format::blocked(
            job.details.company.as_deref().unwrap_or("-"),
            &job.details.title,
            &a.url,
            reason,
            question,
            &hq.ref_code,
        );
        if !options.is_empty() {
            text.push_str(&format!("\nOptions: {}", options.join(" | ")));
        }
        let (tag, chat) = who(app, a);
        let text = if tag.is_empty() {
            text
        } else {
            format!("{tag}\n{text}")
        };
        if let Some(mid) = app
            .notifier
            .notify_person(chat, Event::ApplicationBlocked, &text)
            .await
        {
            let qid = hq.id.clone();
            app.db
                .call(move |c| jdb::set_question_telegram_id(c, &qid, mid))
                .await?;
        }
    }
    Ok(hq)
}

async fn ask_fields(
    app: &App,
    a: &ApplicationRecord,
    job: &JobRecord,
    asks: &[(Field, String)],
) -> AppResult<()> {
    for (f, reason) in asks {
        create_and_send_question(app, a, job, &f.id, &f.label, reason, &f.options).await?;
    }
    let labels: Vec<&str> = asks.iter().map(|(f, _)| f.label.as_str()).collect();
    let detail = labels.join("; ");
    let id = a.id.clone();
    app.db
        .call(move |c| {
            jdb::update_application(
                c,
                &id,
                &AppUpdate {
                    status: Some("awaiting_human"),
                    current_step: Some("awaiting_answers"),
                    blocker: Some(Some(("unknown_question", &detail))),
                    ..Default::default()
                },
            )
        })
        .await?;
    app.metrics
        .applications_blocked_total
        .with_label_values(&["unknown_question"])
        .inc();
    app.audit(
        "job_agent",
        "human_input_requested",
        "application",
        &a.id,
        json!({ "fields": labels }),
    )
    .await;
    Ok(())
}

async fn ask_approval(
    app: &App,
    a: &ApplicationRecord,
    job: &JobRecord,
    why: &str,
    resume_note: &str,
) -> AppResult<()> {
    let q = format!(
        "{why}. Score {}/100. Reply \"submit\" to apply or \"skip\".",
        job.score.unwrap_or(0)
    );
    let (aid, qq) = (a.id.clone(), q.clone());
    let (hq, created) = app
        .db
        .call(move |c| {
            jdb::create_question(
                c,
                &aid,
                FIELD_APPROVE,
                &qq,
                "approval_required",
                &["submit".into(), "skip".into()],
            )
        })
        .await?;
    if created {
        let text = format!(
            "{}🟡 Approval Required\nCompany: {}\nRole: {}\nURL: {}\nScore: {}\nReasons: {}\n{}\nRef: {}\n\nReply \"submit\" to apply or \"skip\".",
            {
                let (tag, _) = who(app, a);
                if tag.is_empty() { tag } else { format!("{tag}\n") }
            },
            job.details.company.as_deref().unwrap_or("-"),
            job.details.title,
            a.url,
            job.score.unwrap_or(0),
            if job.decision_reasons.is_empty() { "-".to_string() } else { job.decision_reasons.join("; ") },
            resume_note,
            hq.ref_code
        );
        let (_, chat) = who(app, a);
        if let Some(mid) = app
            .notifier
            .notify_person(chat, Event::ApprovalRequired, &text)
            .await
        {
            let qid = hq.id.clone();
            app.db
                .call(move |c| jdb::set_question_telegram_id(c, &qid, mid))
                .await?;
        }
    }
    update(
        app,
        &a.id,
        AppUpdate {
            status: Some("awaiting_approval"),
            current_step: Some("awaiting_approval"),
            ..Default::default()
        },
    )
    .await
}

async fn handle_blocker(
    app: &App,
    a: &ApplicationRecord,
    job: &JobRecord,
    b: Blocker,
) -> AppResult<()> {
    // Transient problems are retried by the queue with backoff.
    if matches!(
        b.kind.as_str(),
        "network_error" | "site_error" | "rate_limited"
    ) {
        app.metrics
            .playwright_failures_total
            .with_label_values(&[b.kind.as_str()])
            .inc();
        return Err(AppError::other(format!(
            "transient blocker {}: {}",
            b.kind, b.detail
        )));
    }
    let question = if b.needs_browser_action() {
        format!(
            "{} detected ({}). Complete it in the browser worker window (BROWSER_HEADLESS=false), then reply \"done\" — or \"skip\".",
            b.kind, b.detail
        )
    } else {
        format!(
            "{}: {}. Fix it and reply \"done\", reply \"skip\", or apply yourself using the link above.",
            b.kind, b.detail
        )
    };
    create_and_send_question(app, a, job, FIELD_BLOCKER, &question, &b.kind, &[]).await?;
    let (id, kind, detail) = (a.id.clone(), b.kind.clone(), b.detail.clone());
    app.db
        .call(move |c| {
            jdb::update_application(
                c,
                &id,
                &AppUpdate {
                    status: Some("blocked"),
                    current_step: Some("blocked"),
                    blocker: Some(Some((&kind, &detail))),
                    ..Default::default()
                },
            )
        })
        .await?;
    app.metrics
        .applications_blocked_total
        .with_label_values(&[b.kind.as_str()])
        .inc();
    app.audit(
        "job_agent",
        "application_blocked",
        "application",
        &a.id,
        json!({ "kind": b.kind, "detail": b.detail }),
    )
    .await;
    Ok(())
}

/// Apply a human answer (from Telegram or the API). Returns a short reply text.
pub async fn handle_human_answer(
    app: &Arc<App>,
    q: &HumanQuestion,
    raw_answer: &str,
) -> AppResult<String> {
    let answer = raw_answer.trim().to_string();
    if answer.is_empty() {
        return Ok("Empty answer ignored.".into());
    }
    let (qid, ans) = (q.id.clone(), answer.clone());
    if !app
        .db
        .call(move |c| jdb::answer_question(c, &qid, &ans))
        .await?
    {
        return Ok(format!("{} was already answered.", q.ref_code));
    }
    // A fact about a person (notice period, CTC, …) asked while drafting a
    // reply: store it so future drafts can answer it directly.
    if let Some(rest) = q.field_id.as_deref().and_then(|f| f.strip_prefix("fact:")) {
        let (person_id, fact) = rest.split_once(':').unwrap_or((rest, ""));
        if fact.is_empty() || answer.eq_ignore_ascii_case("skip") {
            return Ok("Okay, skipped.".into());
        }
        let v = crate::agents::facts::clean_answer(fact, &answer);
        let (pid, f) = (person_id.to_string(), fact.to_string());
        app.db
            .call(move |c| jdb::set_person_fact(c, &pid, &f, &v, "telegram"))
            .await?;
        app.audit(
            "human",
            "fact_recorded",
            "person",
            person_id,
            json!({ "ref": q.ref_code, "fact": fact }),
        )
        .await;
        return Ok(format!(
            "Thanks, saved. Future drafts will use this for {}.",
            fact.replace('_', " ")
        ));
    }
    let Some(app_id) = q.application_id.clone() else {
        return Ok("Recorded.".into());
    };
    let lower = answer.to_lowercase();
    let field = q.field_id.clone().unwrap_or_default();
    app.audit(
        "human",
        "question_answered",
        "application",
        &app_id,
        json!({ "ref": q.ref_code, "field": field, "answer": answer }),
    )
    .await;

    if lower == "skip"
        || (field == FIELD_APPROVE && ["no", "n", "reject", "cancel"].contains(&lower.as_str()))
    {
        let aid = app_id.clone();
        let a = app
            .db
            .call(move |c| {
                jdb::cancel_questions(c, &aid)?;
                jdb::update_application(
                    c,
                    &aid,
                    &AppUpdate {
                        status: Some("skipped"),
                        current_step: Some("skipped_by_user"),
                        ..Default::default()
                    },
                )?;
                let a = jdb::get_application(c, &aid)?;
                jdb::set_job_status(c, &a.job_id, "skipped")?;
                Ok(a)
            })
            .await?;
        return Ok(format!(
            "Skipped {} – {}.",
            a.company.unwrap_or_default(),
            a.role.unwrap_or_default()
        ));
    }

    match field.as_str() {
        FIELD_APPROVE => {
            if !["submit", "yes", "y", "approve", "ok", "apply", "go"].contains(&lower.as_str()) {
                // Re-open: unclear approval must not count as consent.
                let (aid, qq) = (app_id.clone(), q.question.clone());
                app.db
                    .call(move |c| {
                        jdb::create_question(
                            c,
                            &aid,
                            FIELD_APPROVE,
                            &qq,
                            "approval_required",
                            &["submit".into(), "skip".into()],
                        )
                        .map(|_| ())
                    })
                    .await?;
                return Ok("Please reply \"submit\" or \"skip\".".into());
            }
            update(
                app,
                &app_id,
                AppUpdate {
                    approved: Some(true),
                    ..Default::default()
                },
            )
            .await?;
        }
        FIELD_BLOCKER => {
            update(
                app,
                &app_id,
                AppUpdate {
                    blocker: Some(None),
                    ..Default::default()
                },
            )
            .await?;
        }
        _ => {
            let value = if q.options.is_empty() {
                answer.clone()
            } else {
                answers::match_option(&q.options, &answer).unwrap_or_else(|| answer.clone())
            };
            let (aid, f2, v2, question) = (
                app_id.clone(),
                field.clone(),
                value.clone(),
                q.question.clone(),
            );
            app.db
                .call(move |c| {
                    let mut a = jdb::get_application(c, &aid)?;
                    a.answers.insert(f2, Value::String(v2.clone()));
                    jdb::update_application(
                        c,
                        &aid,
                        &AppUpdate {
                            answers: Some(&a.answers),
                            ..Default::default()
                        },
                    )?;
                    // Remembered per person, so family members' answers never mix.
                    let ns = a
                        .person_id
                        .as_deref()
                        .map(|p| format!("{p}:"))
                        .unwrap_or_default();
                    jdb::remember_answer(c, &format!("{ns}{question}"), &v2)
                })
                .await?;
        }
    }

    let aid = app_id.clone();
    let remaining = app
        .db
        .call(move |c| jdb::open_questions(c, Some(&aid)))
        .await?;
    if remaining.is_empty() {
        update(
            app,
            &app_id,
            AppUpdate {
                status: Some("pending"),
                current_step: Some("resuming"),
                ..Default::default()
            },
        )
        .await?;
        enqueue_run(app, &app_id).await?;
        Ok("Thanks — resuming the application.".into())
    } else {
        Ok(format!(
            "Got it. {} more question(s) pending: {}",
            remaining.len(),
            remaining
                .iter()
                .map(|q| q.ref_code.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}
