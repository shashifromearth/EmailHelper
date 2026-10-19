//! Job Application Agent: detect opportunities in email, extract and score
//! postings, and drive applications through the browser worker.

pub mod answers;
pub mod application;
pub mod browser;
pub mod cover_letter;
pub mod detect;
pub mod extract;
pub mod matcher;

use std::sync::Arc;

use serde_json::json;

use crate::agents::classifier::Classification;
use crate::app::App;
use crate::db::emails::{self, EmailRecord};
use crate::db::jobs::{self as jdb, JobDetails, JobScore};
use crate::error::AppResult;
use crate::jobs::detect::SignalKind;
use crate::jobs::extract::ExtractedPage;
use crate::jobs::matcher::Decision;
use crate::telegram::Event;

const MAX_LINKS_PER_EMAIL: usize = 5;
const MAX_DIRECT_FETCH_BYTES: usize = 2 * 1024 * 1024;

/// Fetch a page: browser worker first (renders JS, keeps sessions), plain HTTP
/// as a fallback.
pub async fn fetch_page(app: &App, url: &str) -> AppResult<ExtractedPage> {
    if let Some(b) = &app.browser {
        match b.extract(url).await {
            Ok(p) => return Ok(p),
            Err(e) => {
                app.metrics
                    .playwright_failures_total
                    .with_label_values(&["extract"])
                    .inc();
                tracing::warn!(error = %e, url, "browser extract failed; trying direct fetch");
            }
        }
    }
    let resp = app
        .http
        .get(url)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await?
        .error_for_status()?;
    let final_url = resp.url().to_string();
    let bytes = resp.bytes().await?;
    let html =
        String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_DIRECT_FETCH_BYTES)]).to_string();
    Ok(extract::page_from_html(&final_url, &html))
}

/// Score a job, store it, and return `(record, decision, is_new)`.
pub async fn score_and_store(
    app: &App,
    person: &crate::knowledge::Person,
    source: &str,
    source_ref: Option<String>,
    details: JobDetails,
) -> AppResult<(jdb::JobRecord, Decision, bool)> {
    let m = matcher::score(
        &details,
        &person.profile,
        app.cfg.jobs.auto_apply_threshold,
        app.cfg.jobs.review_threshold,
    );
    let reasons = m.reasons();
    let breakdown = json!({ "parts": m.breakdown, "matched_skills": m.matched_skills, "missing_skills": m.missing_skills });
    let source = source.to_string();
    let decision = m.decision;
    let score = m.score as i64;
    let person_id = person_id_of(person);
    let (id, is_new) = app
        .db
        .call(move |c| {
            jdb::upsert(
                c,
                &source,
                source_ref.as_deref(),
                &details,
                &JobScore {
                    score,
                    breakdown: &breakdown,
                    decision: decision.as_str(),
                    reasons: &reasons,
                    person_id: person_id.as_deref(),
                },
            )
        })
        .await?;
    let rec = app.db.call(move |c| jdb::get_job(c, &id)).await?;
    Ok((rec, decision, is_new))
}

/// Person id stored on jobs/applications (None in legacy single-person mode).
pub fn person_id_of(p: &crate::knowledge::Person) -> Option<String> {
    (!p.rel_dir.is_empty()).then(|| p.id.clone())
}

/// The person a stored email/job belongs to (the only person in legacy mode).
pub fn person_by_id(app: &App, id: Option<&str>) -> Option<Arc<crate::knowledge::Person>> {
    let people = app.knowledge.people();
    match id {
        Some(id) => people.person(id),
        None if people.legacy => Some(people.default_person()),
        None => None,
    }
}

/// Queue handler for `job_email`: process every job link in an email.
pub async fn process_job_email(app: &Arc<App>, email_id: &str) -> AppResult<()> {
    let id = email_id.to_string();
    let e = app.db.call(move |c| emails::get(c, &id)).await?;
    let Some(signal) = detect::detect_job_email(&e) else {
        return Ok(());
    };
    let eid = e.id.clone();
    let pid = app.db.call(move |c| emails::person_of(c, &eid)).await?;
    let Some(person) = person_by_id(app, pid.as_deref()) else {
        tracing::info!(
            email_id,
            "job email for an unassigned mailbox; not processed"
        );
        return Ok(());
    };
    let tag = crate::telegram::format::person_tag(&person.name, e.account_email.as_deref());
    let vocab = person.profile.all_skills();
    let mut handled = 0usize;
    for link in signal.links.iter().take(MAX_LINKS_PER_EMAIL) {
        let page = match fetch_page(app, &link.url).await {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(error = %err, url = %link.url, "could not open job link");
                ExtractedPage {
                    final_url: link.url.clone(),
                    ..Default::default()
                }
            }
        };
        let mut d = extract::parse_job(&page, &vocab);
        d.url = Some(link.url.clone());
        if d.title.trim().is_empty() || page.blocker.is_some() {
            // Login walls etc.: fall back to what the email itself says.
            let (t, c) = extract::split_title(&e.subject);
            if d.title.trim().is_empty() {
                d.title = t;
            }
            if d.company.is_none() {
                d.company = c;
            }
            extract::enrich_from_text(&mut d, &e.body_text, &vocab);
        }
        if d.company.is_none() {
            d.company = signal.company_hint.clone();
        }
        let (job, decision, is_new) =
            score_and_store(app, &person, &signal.source, Some(e.id.clone()), d).await?;
        handled += 1;
        if is_new {
            app.notifier
                .notify_person(
                    person.telegram_chat_id,
                    Event::JobFound,
                    &format!(
                        "{tag}\n💼 Job Found\nCompany: {}\nRole: {}\nScore: {}/100 → {}\nURL: {}{}",
                        job.details.company.as_deref().unwrap_or("-"),
                        job.details.title,
                        job.score.unwrap_or(0),
                        decision.as_str(),
                        link.url,
                        if job.decision_reasons.is_empty() {
                            String::new()
                        } else {
                            format!("\nNotes: {}", job.decision_reasons.join("; "))
                        }
                    ),
                )
                .await;
        }
        app.audit(
            "job_agent",
            "job_scored",
            "job",
            &job.id,
            json!({ "email_id": e.id, "score": job.score, "decision": decision.as_str(), "url": link.url }),
        )
        .await;
        if decision != Decision::Skip && app.cfg.jobs.enabled && person.job_agent {
            let apply_url = link.url.clone();
            match application::start(app, &job, &apply_url).await {
                Ok(_) => {}
                // A profile problem is a configuration issue, not a retryable failure.
                Err(crate::error::AppError::Validation(reason)) => {
                    tracing::warn!(job_id = %job.id, %reason, "application not started");
                }
                Err(e) => return Err(e),
            }
        } else if decision == Decision::Skip {
            let jid = job.id.clone();
            app.db
                .call(move |c| jdb::set_job_status(c, &jid, "skipped"))
                .await?;
        }
    }
    tracing::info!(email_id, links = handled, "job email processed");
    Ok(())
}

/// Track interviews and recruiter responses (called from the email pipeline).
pub async fn track_recruiting_email(
    app: &Arc<App>,
    e: &EmailRecord,
    c: &Classification,
    person: &crate::knowledge::Person,
    mailbox: Option<&str>,
) {
    let Some(sig) = &c.job_signal else { return };
    let tag = crate::telegram::format::person_tag(&person.name, mailbox);
    match sig.kind {
        SignalKind::InterviewInvite => {
            let (eid, company, subject) =
                (e.id.clone(), sig.company_hint.clone(), e.subject.clone());
            let res = app
                .db
                .call(move |c| jdb::record_interview(c, &eid, company.as_deref(), None, &subject))
                .await;
            if let Err(err) = res {
                tracing::error!(error = %err, "failed to record interview");
            }
            app.notifier
                .notify_person(
                    person.telegram_chat_id,
                    Event::InterviewRequest,
                    &format!(
                        "{tag}\n📅 Interview Request Received\nFrom: {}\nSubject: {}\nCompany: {}",
                        e.from,
                        e.subject,
                        sig.company_hint.as_deref().unwrap_or("-")
                    ),
                )
                .await;
        }
        SignalKind::RecruiterOutreach | SignalKind::Referral if e.in_reply_to.is_some() => {
            app.notifier
                .notify_person(
                    person.telegram_chat_id,
                    Event::RecruiterResponse,
                    &format!(
                        "{tag}\n📨 Recruiter Response Received\nFrom: {}\nSubject: {}",
                        e.from, e.subject
                    ),
                )
                .await;
        }
        _ => {}
    }
}
