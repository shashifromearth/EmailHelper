//! Job Application Agent: detect opportunities in email, extract and score
//! postings, and drive applications through the browser worker.

pub mod alerts;
pub mod answers;
pub mod application;
pub mod assist;
pub mod benchmark;
pub mod browser;
pub mod cover_letter;
pub mod detect;
pub mod digest;
pub mod employer;
pub mod extract;
pub mod feedback;
pub mod linkqueue;
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
/// Jobs taken from one platform alert email.
const MAX_ALERT_JOBS: usize = 10;
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
    // LinkedIn, Naukri & co. only allow applying inside a logged-in account and
    // forbid bots: never open their pages; work from the email instead. Links
    // straight to an employer's own application page are still followed.
    let platform_links: Vec<detect::JobLink> = signal
        .links
        .iter()
        .filter(|l| alerts::is_login_platform(&l.ats))
        .cloned()
        .collect();
    if app.cfg.jobs.links_enabled {
        return queue_links(app, &e, &signal, &platform_links, &person).await;
    }
    let tag = crate::telegram::format::person_tag(&person.name, e.account_email.as_deref());
    if alerts::is_login_platform(&signal.source) || !platform_links.is_empty() {
        process_platform_alert(app, &e, &signal.source, &platform_links, &person, &tag).await?;
    }
    let vocab = person.profile.all_skills();
    let mut handled = 0usize;
    for link in signal
        .links
        .iter()
        .filter(|l| !alerts::is_login_platform(&l.ats))
        .take(MAX_LINKS_PER_EMAIL)
    {
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

/// The platform an alert email lists jobs for.
fn alert_platform<'a>(source: &'a str, platform_links: &'a [detect::JobLink]) -> &'a str {
    platform_links
        .first()
        .filter(|_| !alerts::is_login_platform(source))
        .map(|l| l.ats.as_str())
        .unwrap_or(source)
}

/// `JOB_LINKS_ENABLED`: hand every job link in the email to the job-links
/// service (the Chrome extension applies from there), scored on what the
/// email says; nothing is browsed.
async fn queue_links(
    app: &Arc<App>,
    e: &EmailRecord,
    signal: &detect::JobSignal,
    platform_links: &[detect::JobLink],
    person: &crate::knowledge::Person,
) -> AppResult<()> {
    let listed = alerts::parse(alert_platform(&signal.source, platform_links), &e.body_text);
    let person_id = person_id_of(person);
    let links: Vec<linkqueue::QueuedLink> = linkqueue::jobs_in_email(e, signal, &listed)
        .into_iter()
        .map(|(link, d)| {
            // Without a title there is nothing to score; such jobs sort last.
            let m = (!d.title.trim().is_empty()).then(|| {
                matcher::score(&d, &person.profile, app.cfg.jobs.auto_apply_threshold, app.cfg.jobs.review_threshold)
            });
            linkqueue::QueuedLink {
                source_detail: Some(e.id.clone()),
                score: m.as_ref().map(|m| m.score.min(100)),
                decision: m.as_ref().map(|m| m.decision.as_str().to_string()),
                person_id: person_id.clone(),
                resume_hint: person.select_resume(&d.skills, None).map(|(r, _)| r.file_name.clone()),
                mailbox: e.account_email.clone(),
                ..linkqueue::QueuedLink::from_job(linkqueue::LinkSource::Email, &link.url, Some(&link.ats), &d)
            }
        })
        .collect();
    if links.is_empty() {
        tracing::info!(email_id = %e.id, "no job links to queue");
        return Ok(());
    }
    let summary = linkqueue::LinkQueueClient::from_app(app).upsert(&links).await?;
    app.audit(
        "job_agent",
        "job_links_queued",
        "email",
        &e.id,
        json!({ "links": links.len(), "created": summary.created, "updated": summary.updated, "failed": summary.failed }),
    )
    .await;
    tracing::info!(email_id = %e.id, links = links.len(), created = summary.created, updated = summary.updated, failed = summary.failed, "job links queued");
    Ok(())
}

/// "5 - 10 years" / "9-13 Yrs" → 5.
fn min_years(exp: &str) -> Option<f64> {
    exp.split(|c: char| !c.is_ascii_digit() && c != '.')
        .find(|s| !s.is_empty())
        .and_then(|s| s.parse().ok())
}

fn remote_of(mode: Option<&str>) -> Option<String> {
    let m = mode?.to_lowercase();
    Some(
        if m.contains("remote") {
            "remote"
        } else if m.contains("hybrid") {
            "hybrid"
        } else if m.contains("office") || m.contains("on-site") || m.contains("onsite") {
            "onsite"
        } else {
            "unknown"
        }
        .to_string(),
    )
}

/// What an alert says about a job.
pub fn details_from_alert(a: &alerts::AlertJob) -> JobDetails {
    JobDetails {
        title: a.title.clone(),
        company: a.company.clone(),
        url: a.url.clone(),
        location: a.location.clone(),
        remote: remote_of(a.work_mode.as_deref()),
        salary: a.salary.clone(),
        experience: a.experience.clone(),
        experience_years: a.experience.as_deref().and_then(min_years),
        skills: a.skills.clone(),
        ..Default::default()
    }
}

/// A LinkedIn/Naukri alert: screen each listed job, find it on the
/// employer's public board and apply there; jobs that can only be applied to
/// on the platform go to the daily digest.
async fn process_platform_alert(
    app: &Arc<App>,
    e: &EmailRecord,
    source: &str,
    platform_links: &[detect::JobLink],
    person: &crate::knowledge::Person,
    tag: &str,
) -> AppResult<()> {
    let platform = alert_platform(source, platform_links);
    let mut listed = alerts::parse(platform, &e.body_text);
    if listed.is_empty() {
        // No job list in the body (recruiter mail, unknown layout): the subject
        // names the job and the platform link is kept for the person.
        if let Some(link) = platform_links.first() {
            let (title, company) = extract::split_title(&e.subject);
            listed.push(alerts::AlertJob {
                title,
                company,
                url: Some(link.url.clone()),
                ..Default::default()
            });
        }
    }
    if listed.is_empty() {
        tracing::info!(email_id = %e.id, platform, "no jobs found in alert email");
        return Ok(());
    }
    let (mut on_board, mut digest, mut skipped) = (0usize, 0usize, 0usize);
    for a in listed.iter().take(MAX_ALERT_JOBS) {
        // 1. Screen on what the alert says (title, company, location).
        let first = matcher::score(
            &details_from_alert(a),
            &person.profile,
            app.cfg.jobs.auto_apply_threshold,
            app.cfg.jobs.review_threshold,
        );
        let posting = if first.decision == Decision::Skip {
            None
        } else {
            match employer::find_posting(app, a).await {
                Ok(p) => p,
                Err(err) => {
                    tracing::warn!(error = %err, company = ?a.company, "employer board lookup failed");
                    None
                }
            }
        };
        match posting {
            // 2a. On the employer's own board: full scoring and the normal application flow.
            Some(raw) => {
                let url = raw.details.url.clone().unwrap_or_default();
                let (job, decision, is_new) =
                    score_and_store(app, person, &raw.source, raw.source_ref.clone(), raw.details).await?;
                on_board += 1;
                if is_new {
                    app.notifier
                        .notify_person(
                            person.telegram_chat_id,
                            Event::JobFound,
                            &format!(
                                "{tag}\n💼 Job Found (via {platform} alert, applying on the employer's site)\nCompany: {}\nRole: {}\nScore: {}/100 → {}\nURL: {url}",
                                job.details.company.as_deref().unwrap_or("-"),
                                job.details.title,
                                job.score.unwrap_or(0),
                                decision.as_str(),
                            ),
                        )
                        .await;
                }
                if decision == Decision::Skip {
                    let jid = job.id.clone();
                    app.db.call(move |c| jdb::set_job_status(c, &jid, "skipped")).await?;
                } else if is_new && app.cfg.jobs.enabled && person.job_agent {
                    match application::start(app, &job, &url).await {
                        Ok(_) => {}
                        Err(crate::error::AppError::Validation(reason)) => {
                            tracing::warn!(job_id = %job.id, %reason, "application not started");
                        }
                        Err(err) => return Err(err),
                    }
                }
            }
            // 2b. Platform-only (or skipped): remember it; good ones go to the digest.
            None => {
                let (job, decision, is_new) =
                    score_and_store(app, person, platform, Some(e.id.clone()), details_from_alert(a)).await?;
                let status = if decision == Decision::Skip {
                    skipped += 1;
                    "skipped"
                } else {
                    digest += 1;
                    "digest"
                };
                if is_new {
                    let jid = job.id.clone();
                    app.db.call(move |c| jdb::set_job_status(c, &jid, status)).await?;
                }
            }
        }
    }
    app.audit(
        "job_agent",
        "platform_alert_processed",
        "email",
        &e.id,
        json!({ "platform": platform, "listed": listed.len(), "on_employer_board": on_board, "digest": digest, "skipped": skipped }),
    )
    .await;
    tracing::info!(email_id = %e.id, platform, listed = listed.len(), on_board, digest, skipped, "platform alert processed");
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
