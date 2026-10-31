//! Email workflow orchestration:
//! route (mailbox → person + purposes) → classify → choose purpose →
//! (job / recruiter hand-off) → context (that person + purpose only) →
//! facts the email asks for → draft as that person → validate → store draft
//! for Thunderbird (from the receiving mailbox) → notify. Never sends email.

use std::sync::Arc;
use std::time::Instant;

use serde_json::json;

use crate::agents::classifier::{Category, Classification};
use crate::agents::{classifier, context, drafter, facts, validator};
use crate::app::App;
use crate::db::{drafts, emails, jobs as jdb};
use crate::error::AppResult;
use crate::knowledge::people::{Person, CONSULTING, FREELANCING, JOB_SEEKING};
use crate::queue;
use crate::telegram::{format, Event};

pub async fn process_email(app: &Arc<App>, email_id: &str) -> AppResult<()> {
    let started = Instant::now();
    let id = email_id.to_string();
    let e = app.db.call(move |c| emails::get(c, &id)).await?;
    if e.status == "done" {
        return Ok(()); // already finished (retry after a crash between steps)
    }
    let id = e.id.clone();
    app.db
        .call(move |c| emails::set_status(c, &id, "processing"))
        .await?;

    // 0. Route: which person owns the receiving mailbox, and what is it for?
    let people = app.knowledge.people();
    let route = people.route(e.account_email.as_deref(), &e.to, &e.cc);

    // 1. Classification. Stale mail (e.g. a bulk pass over old unread
    //    messages) is classified by rules only so it doesn't monopolise the
    //    local model.
    let stale = is_stale(e.received_at.as_deref(), app.cfg.max_email_age_hours);
    let cls = classifier::classify(&app.llm, &e, stale).await;
    let purpose = choose_purpose(&route.purposes, &cls, &e);
    let cls_json = serde_json::to_string(&cls)?;
    let (id, cat, is_job) = (
        e.id.clone(),
        cls.category.as_str().to_string(),
        cls.is_job_opportunity,
    );
    let (pid, purp) = (route.person.as_ref().map(|p| p.id.clone()), purpose.clone());
    app.db
        .call(move |c| {
            emails::set_classification(c, &id, &cat, &cls_json, is_job)?;
            emails::set_route(c, &id, pid.as_deref(), purp.as_deref())
        })
        .await?;
    app.audit(
        "classifier",
        "email_classified",
        "email",
        &e.id,
        json!({ "category": cls.category.as_str(), "confidence": cls.confidence, "method": cls.method,
                "is_job": cls.is_job_opportunity, "needs_reply": cls.needs_reply,
                "injection_flags": cls.injection_flags,
                "person": route.person.as_ref().map(|p| p.id.clone()),
                "mailbox": route.mailbox, "purpose": purpose }),
    )
    .await;

    // Mailboxes nobody owns are classified only: no drafts, no job actions.
    let Some(person) = route.person.clone() else {
        finish(app, &e, &cls, "unassigned_mailbox", started).await?;
        return Ok(());
    };

    // 2. Hand-offs to the job and recruiter agents (separate durable jobs),
    //    only for people who use the job agent and mail about job seeking.
    let job_purpose = purpose.as_deref().map(|p| p == JOB_SEEKING).unwrap_or(true);
    if cls.is_job_opportunity {
        if let Some(sig) = &cls.job_signal {
            app.metrics
                .jobs_detected_total
                .with_label_values(&[sig.source.as_str()])
                .inc();
        }
        // Old mail is only counted: acting on years-old job alerts would
        // browse stale postings.
        if app.cfg.jobs.enabled && person.job_agent && job_purpose && !stale {
            app.queue
                .enqueue(
                    queue::KIND_JOB_EMAIL,
                    format!("job_email:{}:v{}", e.id, app.cfg.workflow_version),
                    Some(e.id.clone()),
                    None,
                    app.cfg.jobs.max_attempts,
                )
                .await?;
        }
        if app.cfg.research.recruiter_enabled && job_purpose && !stale {
            app.queue
                .enqueue(
                    queue::KIND_RECRUITER,
                    format!("recruiter:{}:v{}", e.id, app.cfg.workflow_version),
                    Some(e.id.clone()),
                    None,
                    2,
                )
                .await?;
        }
        if !stale {
            crate::jobs::track_recruiting_email(app, &e, &cls, &person, route.mailbox.as_deref())
                .await;
        }
    }

    if !cls.needs_reply {
        let outcome = if stale {
            "skipped_old"
        } else {
            "no_reply_needed"
        };
        finish(app, &e, &cls, outcome, started).await?;
        return Ok(());
    }

    // 3. Context retrieval, scoped to this person and purpose.
    let ctx = match context::retrieve(app, &e, &person, purpose.as_deref()).await {
        Ok(c) => c,
        Err(err) => {
            app.metrics
                .agent_failures_total
                .with_label_values(&["context"])
                .inc();
            return Err(err);
        }
    };

    // 4. Facts the email asks for (CTC, notice period, DOB, …).
    let asked = facts::asked(&e.subject, &e.body_text);
    let pid = person.id.clone();
    let stored = app.db.call(move |c| jdb::person_facts(c, &pid)).await?;
    let resume = resume_for_email(&person, &e, purpose.as_deref());
    let mut plan = facts::plan(&person, &stored, &asked, resume.as_ref().map(|(f, _)| f.as_str()));
    let resume_path = resume.as_ref().and_then(|(_, p)| p.clone());
    let attachments: Vec<String> = match (&plan.attach_resume, &resume_path) {
        (Some(_), Some(p)) => vec![p.clone()],
        _ => Vec::new(),
    };
    plan.resume_attached = !attachments.is_empty();
    if purpose.as_deref() == Some(crate::knowledge::people::JOB_SEEKING) || !plan.answers.is_empty() {
        let mut skills = person.profile.all_skills();
        skills.dedup();
        skills.truncate(80);
        plan.skills = skills;
    }

    // 5. Draft generation, written as the receiving person.
    // Recruiter questionnaires get an explicit checklist taken from the full email.
    let items = crate::agents::questions::extract(&e.body_text);
    let facts_text = format!("{}{}", plan.prompt_section(), crate::agents::questions::prompt_section(&items));
    let mut draft = match drafter::generate(&app.llm, &e, &cls, &ctx, &facts_text).await {
        Ok(d) => d,
        Err(err) => {
            app.metrics
                .agent_failures_total
                .with_label_values(&["drafter"])
                .inc();
            return Err(err);
        }
    };
    let mut skipped_items = Vec::new();
    if !items.is_empty() {
        let missed: Vec<String> = crate::agents::questions::missing(&items, &draft.body)
            .into_iter()
            .cloned()
            .collect();
        // One more try on a fast cloud model; a local CPU model would take minutes.
        if !missed.is_empty() && app.llm.provider_name() != "ollama" {
            let retry = format!(
                "{facts_text}\nYOUR PREVIOUS DRAFT MISSED THESE ITEMS: {}. Write the whole reply again and \
answer EVERY questionnaire item.\n",
                missed.join("; ")
            );
            if let Ok(d) = drafter::generate(&app.llm, &e, &cls, &ctx, &retry).await {
                if crate::agents::questions::missing(&items, &d.body).len() < missed.len() {
                    draft = d;
                }
            }
        }
        let still = crate::agents::questions::missing(&items, &draft.body);
        if !still.is_empty() {
            draft.body = crate::agents::questions::append_missing(&draft.body, &still);
            skipped_items = still.iter().map(|s| s.to_string()).collect();
        }
    }

    // 6. Validation runs on the draft with private placeholders masked, so the
    //    validator (and any LLM reviewer) never sees private values.
    let (draft_body, added) = plan.ensure_answered(&draft.body);
    let grounding = format!("FACTS (trusted):\n{}\n\n{}", plan.grounding(), ctx.grounding);
    let masked = plan.mask_tokens(&draft_body);
    let v = validator::validate(
        &app.llm,
        &validator::ValidationInput {
            draft: &masked,
            grounding: &grounding,
            classification_confidence: cls.confidence,
            injection_flags: &cls.injection_flags,
        },
        app.cfg.llm_review_enabled(),
    )
    .await;
    let (body, private_used) = plan.fill_tokens(&draft_body);
    let mut verdict = v.verdict;
    let mut notes = v.notes.clone();
    if !added.is_empty() {
        notes.push(format!("Added answers the model left out: {}", added.join(", ")));
    }
    if !items.is_empty() {
        notes.push(format!("Questionnaire: {} items", items.len()));
    }
    if !skipped_items.is_empty() {
        notes.push(format!("Marked 'Will confirm': {}", skipped_items.join("; ")));
    }
    if !private_used.is_empty() {
        verdict = validator::Verdict::ReviewRequired;
        notes.push(format!(
            "Contains private details ({}); check before sending",
            private_used.join(", ")
        ));
    }
    if let Some(file) = &plan.attach_resume {
        notes.push(if plan.resume_attached {
            format!("Resume {file} attached")
        } else {
            format!("Attach resume {file} before sending")
        });
    }
    if verdict == validator::Verdict::ReviewRequired {
        app.metrics.validation_failures_total.inc();
    }
    let unknown = plan.unknown();
    if !unknown.is_empty() {
        let refs = ask_person_for_facts(app, &person, route.mailbox.as_deref(), &e, &unknown).await;
        notes.push(format!(
            "Asked {} for: {}{}",
            person.name,
            unknown
                .iter()
                .map(|f| f.label)
                .collect::<Vec<_>>()
                .join(", "),
            if refs.is_empty() {
                String::new()
            } else {
                format!(" ({})", refs.join(", "))
            }
        ));
    }
    notes.push(format!(
        "Written as {}{}{}",
        person.name,
        route
            .mailbox
            .as_deref()
            .map(|m| format!(" from {m}"))
            .unwrap_or_default(),
        purpose
            .as_deref()
            .map(|p| format!(" ({p})"))
            .unwrap_or_default()
    ));
    if !ctx.sources.is_empty() {
        notes.push(format!("Grounded in {}", ctx.sources.join(", ")));
    }

    // 7. Store draft for Thunderbird (the extension saves it; nothing is sent).
    let new = drafts::NewDraft {
        email_id: Some(e.id.clone()),
        kind: "reply".into(),
        account_id: e.account_id.clone(),
        tb_message_id: Some(e.tb_message_id),
        message_id: Some(e.message_id.clone()),
        to: vec![e.from_email.clone()],
        subject: draft.subject.clone(),
        body,
        validation: verdict.as_str().into(),
        notes: notes.clone(),
        confidence: Some(v.confidence),
        llm_provider: Some(draft.provider.clone()),
        llm_model: Some(draft.model.clone()),
        from_address: route.mailbox.clone(),
        person_id: Some(person.id.clone()),
        attachments,
    };
    let draft_id = app.db.call(move |c| drafts::insert(c, &new)).await?;
    app.drafts_ready.notify_waiters();
    crate::eval::live::spawn(
        app,
        draft_id.clone(),
        format!("{}\n{}", e.subject, e.body_text.chars().take(2000).collect::<String>()),
        grounding.clone(),
        masked.clone(),
    );
    app.metrics
        .drafts_created_total
        .with_label_values(&[verdict.as_str()])
        .inc();
    app.audit(
        "drafter",
        "draft_created",
        "draft",
        &draft_id,
        json!({ "email_id": e.id, "validation": verdict.as_str(), "notes": notes,
                "person": person.id, "mailbox": route.mailbox, "purpose": purpose,
                "facts_asked": asked.iter().map(|f| f.key).collect::<Vec<_>>(),
                "provider": draft.provider, "model": draft.model, "sources": ctx.sources }),
    )
    .await;

    // 8. Notify (tagged with whose mailbox it is).
    let tag = format::person_tag(&person.name, route.mailbox.as_deref());
    app.notifier
        .notify_person(
            person.telegram_chat_id,
            Event::DraftCreated,
            &format!(
                "{tag}\n{}",
                format::draft_created(
                    &e.from,
                    &e.subject,
                    e.account_name
                        .as_deref()
                        .or(route.mailbox.as_deref())
                        .unwrap_or(&e.account_id),
                    &draft_id,
                    verdict.as_str()
                )
            ),
        )
        .await;
    if verdict == validator::Verdict::ReviewRequired {
        app.notifier
            .notify_person(
                person.telegram_chat_id,
                Event::ValidationFailed,
                &format!(
                    "{tag}\n{}",
                    format::validation_failed(&e.from, &e.subject, &notes)
                ),
            )
            .await;
    }

    finish(app, &e, &cls, "draft_created", started).await
}

/// Pick the purpose an email is about, among the receiving mailbox's purposes.
pub fn choose_purpose(
    purposes: &[String],
    cls: &Classification,
    e: &emails::EmailRecord,
) -> Option<String> {
    match purposes.len() {
        0 => return None,
        1 => return Some(purposes[0].clone()),
        _ => {}
    }
    let has = |p: &str| purposes.iter().any(|x| x == p);
    if (cls.is_job_opportunity || cls.category == Category::Recruiting) && has(JOB_SEEKING) {
        return Some(JOB_SEEKING.into());
    }
    let text = format!("{}\n{}", e.subject, e.body_text).to_lowercase();
    let count = |words: &[&str]| words.iter().filter(|w| text.contains(*w)).count();
    let freelance = count(&[
        "freelance",
        "freelancer",
        "upwork",
        "fiverr",
        "toptal",
        "gig",
        "hourly rate",
        "per hour",
        "fixed price",
        "fixed-price",
        "project basis",
        "short-term project",
        "milestone",
    ]);
    let consulting = count(&[
        "consulting",
        "consultant",
        "consultancy",
        "advisory",
        "engagement",
        "statement of work",
        "sow",
        "retainer",
        "consultation",
        "architecture review",
        "assessment",
    ]);
    if freelance > consulting && has(FREELANCING) {
        return Some(FREELANCING.into());
    }
    if consulting > 0 && has(CONSULTING) {
        return Some(CONSULTING.into());
    }
    if freelance > 0 && has(FREELANCING) {
        return Some(FREELANCING.into());
    }
    if matches!(cls.category, Category::Sales | Category::Partnership) {
        if has(CONSULTING) {
            return Some(CONSULTING.into());
        }
        if has(FREELANCING) {
            return Some(FREELANCING.into());
        }
    }
    Some(purposes[0].clone())
}

/// The resume to attach when an email asks for one, chosen by the tech stack
/// the email mentions.
/// Resume for a reply: (file name, path to attach when the file is known).
fn resume_for_email(
    person: &Person,
    e: &emails::EmailRecord,
    purpose: Option<&str>,
) -> Option<(String, Option<String>)> {
    let mut d = crate::db::jobs::JobDetails::default();
    crate::jobs::extract::enrich_from_text(
        &mut d,
        &format!("{}\n{}", e.subject, e.body_text),
        &person.profile.all_skills(),
    );
    person
        .select_resume(&d.skills, purpose)
        .map(|(r, _)| (r.file_name.clone(), Some(r.path.to_string_lossy().to_string())))
        .or_else(|| {
            let f = person.profile.resume.resume_file.trim();
            (!f.is_empty()).then(|| (f.to_string(), None))
        })
}

/// Ask the person (Telegram + dashboard) for facts the email needed that we
/// don't have yet. Returns the question reference codes.
async fn ask_person_for_facts(
    app: &Arc<App>,
    person: &Person,
    mailbox: Option<&str>,
    e: &emails::EmailRecord,
    unknown: &[&'static facts::FactSpec],
) -> Vec<String> {
    let mut refs = Vec::new();
    for f in unknown {
        let question = format!("What is {}'s {}?", person.name, f.label.to_lowercase());
        let reason = format!(
            "asked in \"{}\" from {}",
            e.subject.chars().take(80).collect::<String>(),
            e.from_email
        );
        let (pid, key, q, r) = (person.id.clone(), f.key, question.clone(), reason.clone());
        let created = app
            .db
            .call(move |c| jdb::create_fact_question(c, &pid, key, &q, &r))
            .await;
        let (hq, is_new) = match created {
            Ok(x) => x,
            Err(err) => {
                tracing::warn!(error = %err, "could not create fact question");
                continue;
            }
        };
        refs.push(hq.ref_code.clone());
        if is_new {
            let text = format!(
                "{}\n❓ Information needed\n{question}\nWhy: {reason}\nRef: {}\n\nReply to this message with the answer. It's saved for future emails.",
                format::person_tag(&person.name, mailbox),
                hq.ref_code
            );
            if let Some(mid) = app
                .notifier
                .notify_person(person.telegram_chat_id, Event::ApprovalRequired, &text)
                .await
            {
                let qid = hq.id.clone();
                let _ = app
                    .db
                    .call(move |c| jdb::set_question_telegram_id(c, &qid, mid))
                    .await;
            }
        }
    }
    refs
}

async fn finish(
    app: &Arc<App>,
    e: &emails::EmailRecord,
    cls: &classifier::Classification,
    outcome: &'static str,
    started: Instant,
) -> AppResult<()> {
    let id = e.id.clone();
    app.db
        .call(move |c| emails::set_status(c, &id, "done"))
        .await?;
    app.metrics
        .emails_processed_total
        .with_label_values(&[&e.account_type, cls.category.as_str()])
        .inc();
    let elapsed = started.elapsed();
    app.metrics
        .processing_duration_seconds
        .with_label_values(&["email"])
        .observe(elapsed.as_secs_f64());
    tracing::info!(
        email_id = %e.id,
        account_id = %e.account_id,
        category = cls.category.as_str(),
        outcome,
        duration_ms = elapsed.as_millis() as u64,
        "email processed"
    );
    app.audit(
        "pipeline",
        "email_processed",
        "email",
        &e.id,
        json!({ "outcome": outcome, "duration_ms": elapsed.as_millis() as u64 }),
    )
    .await;
    Ok(())
}

/// True when `received_at` (RFC 3339) is older than `max_age_hours` (0 = never).
pub fn is_stale(received_at: Option<&str>, max_age_hours: u64) -> bool {
    if max_age_hours == 0 {
        return false;
    }
    let Some(ts) = received_at.and_then(|r| chrono::DateTime::parse_from_rfc3339(r).ok()) else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(ts.with_timezone(&chrono::Utc));
    age > chrono::Duration::hours(max_age_hours as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staleness() {
        let old = (chrono::Utc::now() - chrono::Duration::hours(100)).to_rfc3339();
        let fresh = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        assert!(is_stale(Some(&old), 72));
        assert!(!is_stale(Some(&fresh), 72));
        assert!(!is_stale(Some(&old), 0), "0 disables the limit");
        assert!(!is_stale(None, 72), "unknown date is processed");
        assert!(!is_stale(Some("garbage"), 72));
    }

    fn cls(cat: Category, job: bool) -> Classification {
        Classification {
            category: cat,
            confidence: 0.9,
            is_job_opportunity: job,
            needs_reply: true,
            urgency: "normal".into(),
            summary: String::new(),
            method: "test".into(),
            job_signal: None,
            injection_flags: vec![],
        }
    }

    fn email(subject: &str, body: &str) -> emails::EmailRecord {
        emails::EmailRecord {
            id: "e".into(),
            account_id: "a".into(),
            account_name: None,
            account_email: None,
            account_type: "gmail".into(),
            message_id: "<m>".into(),
            tb_message_id: 1,
            folder: None,
            in_reply_to: None,
            references: vec![],
            from: "x@y.z".into(),
            from_email: "x@y.z".into(),
            to: vec![],
            cc: vec![],
            subject: subject.into(),
            received_at: None,
            body_text: body.into(),
            body_html: None,
            attachments: vec![],
            category: None,
            is_job: false,
            status: "received".into(),
            created_at: "now".into(),
        }
    }

    #[test]
    fn purpose_selection() {
        let all: Vec<String> = vec![JOB_SEEKING.into(), CONSULTING.into(), FREELANCING.into()];
        assert_eq!(
            choose_purpose(&[], &cls(Category::Support, false), &email("a", "b")),
            None
        );
        assert_eq!(
            choose_purpose(
                &[CONSULTING.into()],
                &cls(Category::Recruiting, true),
                &email("a", "b")
            )
            .as_deref(),
            Some(CONSULTING),
            "a single-purpose mailbox is always that purpose"
        );
        assert_eq!(
            choose_purpose(
                &all,
                &cls(Category::Recruiting, true),
                &email("Role", "We are hiring")
            )
            .as_deref(),
            Some(JOB_SEEKING)
        );
        assert_eq!(
            choose_purpose(
                &all,
                &cls(Category::Sales, false),
                &email("Upwork project", "Fixed price project, hourly rate?")
            )
            .as_deref(),
            Some(FREELANCING)
        );
        assert_eq!(
            choose_purpose(
                &all,
                &cls(Category::Sales, false),
                &email(
                    "Advisory",
                    "We need a consultant for an architecture review engagement"
                )
            )
            .as_deref(),
            Some(CONSULTING)
        );
        assert_eq!(
            choose_purpose(
                &all,
                &cls(Category::Partnership, false),
                &email("Hello", "Let's work together")
            )
            .as_deref(),
            Some(CONSULTING)
        );
    }
}
