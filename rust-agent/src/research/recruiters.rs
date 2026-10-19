//! Recruiter discovery for recruiting / job emails: company research via
//! SearXNG, the sender as a contact, LinkedIn people search through the
//! user's own logged-in session (opt-in, rate limited), ranking and outreach
//! drafts. Outreach is always a draft for review; nothing is ever sent.

use std::sync::Arc;

use serde_json::json;

use crate::app::App;
use crate::db::emails::{self, display_name, EmailRecord};
use crate::db::jobs::{self as jdb, Recruiter};
use crate::db::{drafts, settings};
use crate::error::AppResult;
use crate::jobs::detect;
use crate::llm::{LlmRequest, LlmTask};
use crate::prompts;
use crate::telegram::Event;

/// Rank a contact by how likely they are to own hiring for the role.
pub fn rank(title: &str, company: &str) -> i64 {
    let t = title.to_lowercase();
    let mut s = 0;
    if t.contains("technical recruiter") || t.contains("tech recruiter") {
        s += 50;
    } else if t.contains("talent acquisition") || t.contains("recruiter") || t.contains("sourcer") {
        s += 40;
    } else if t.contains("hiring manager")
        || t.contains("engineering manager")
        || t.contains("head of engineering")
        || t.contains("director of engineering")
        || t.contains("vp engineering")
        || t.contains("cto")
    {
        s += 35;
    } else if t.contains("people")
        || t.contains("hr ")
        || t.ends_with(" hr")
        || t.contains("human resources")
    {
        s += 20;
    }
    if ["ai", "ml", "machine learning", "engineering", "tech"]
        .iter()
        .any(|k| t.contains(k))
    {
        s += 15;
    }
    if !company.is_empty() && t.contains(&company.to_lowercase()) {
        s += 20;
    }
    if t.contains("senior") || t.contains("lead") || t.contains("head") {
        s += 5;
    }
    s.min(100)
}

async fn outreach_draft(
    app: &App,
    p: &crate::knowledge::Profile,
    r: &Recruiter,
    role_hint: &str,
) -> String {
    let req = LlmRequest::new(
        LlmTask::Outreach,
        format!(
            "Write a short, polite outreach note (under 90 words) from the candidate to a recruiter or hiring manager. \
Specific, no flattery, no invented facts, plain text, no subject line. {}",
            prompts::DATA_ONLY_RULE
        ),
        format!(
            "CANDIDATE:\n{}\nRECIPIENT: {} — {} at {}\nROLE OF INTEREST: {}",
            p.summary_text(),
            prompts::sanitize_untrusted(&r.name, 100).text,
            prompts::sanitize_untrusted(r.title.as_deref().unwrap_or(""), 200).text,
            r.company,
            prompts::sanitize_untrusted(role_hint, 200).text,
        ),
    )
    .max_tokens(250);
    match app.llm.complete(&req).await {
        Ok(resp) => {
            let body = crate::agents::drafter::clean_body(&resp.text);
            if body.split_whitespace().count() >= 15 {
                return format!("{body}\n\n{}", p.signature());
            }
            fallback(r, role_hint, &p.signature())
        }
        Err(_) => fallback(r, role_hint, &p.signature()),
    }
}

fn fallback(r: &Recruiter, role_hint: &str, sig: &str) -> String {
    let first = r.name.split_whitespace().next().unwrap_or("there");
    format!(
        "Hi {first},\n\nI'm interested in {} opportunities at {} and believe my background is a strong fit. \
Would you be open to a short conversation about the team's hiring plans?\n\n{sig}",
        if role_hint.is_empty() { "AI engineering" } else { role_hint },
        r.company
    )
}

async fn linkedin_quota_ok(app: &App) -> AppResult<bool> {
    let key = format!(
        "linkedin_searches:{}",
        chrono::Local::now().format("%Y-%m-%d")
    );
    let limit = app.cfg.research.recruiter_daily_limit as i64;
    let used = app
        .db
        .call(move |c| {
            Ok(settings::get(c, &key)?
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0))
        })
        .await?;
    if used >= limit {
        return Ok(false);
    }
    let key = format!(
        "linkedin_searches:{}",
        chrono::Local::now().format("%Y-%m-%d")
    );
    app.db.call(move |c| settings::incr(c, &key)).await?;
    Ok(true)
}

/// Queue handler: discover recruiters for the company behind an email.
pub async fn discover_for_email(app: &Arc<App>, email_id: &str) -> AppResult<()> {
    let id = email_id.to_string();
    let e = app.db.call(move |c| emails::get(c, &id)).await?;
    let signal = detect::detect_job_email(&e);
    let company = signal
        .as_ref()
        .and_then(|s| s.company_hint.clone())
        .or_else(|| detect::company_from_domain(&e.from_email));
    let eid = e.id.clone();
    let pid = app.db.call(move |c| emails::person_of(c, &eid)).await?;
    let Some(person) = crate::jobs::person_by_id(app, pid.as_deref()) else {
        tracing::info!(email_id, "recruiter discovery skipped: unassigned mailbox");
        return Ok(());
    };
    let Some(company) = company else {
        tracing::info!(
            email_id,
            "no company identified; skipping recruiter discovery"
        );
        return Ok(());
    };
    let role_hint = crate::jobs::extract::split_title(&e.subject).0;
    let mut found: Vec<Recruiter> = Vec::new();

    // 1. The sender, if a human at the company.
    if signal.as_ref().is_some_and(|s| !s.is_alert)
        && !crate::agents::classifier::is_automated_sender(&e.from_email)
    {
        let title = signature_title(&e).unwrap_or_else(|| "Recruiter".into());
        found.push(Recruiter {
            company: company.clone(),
            name: display_name(&e.from),
            rank_score: rank(&title, &company).max(40),
            title: Some(title),
            email: Some(e.from_email.clone()),
            source: "email".into(),
            ..Default::default()
        });
    }

    // 2. Company research via SearXNG (stored as findings for context).
    if let Some(base) = &app.cfg.research.searxng_url {
        let q = format!("\"{company}\" company careers engineering");
        match crate::research::sources::searxng_search(&app.http, base, &q, 5).await {
            Ok(hits) => {
                for h in hits {
                    let qq = q.clone();
                    let _ = app
                        .db
                        .call(move |c| {
                            jdb::save_finding(
                                c,
                                &qq,
                                &h.url,
                                &h.title,
                                &h.content,
                                "company_research",
                            )
                        })
                        .await;
                }
            }
            Err(err) => tracing::warn!(error = %err, "company research search failed"),
        }
    }

    // 3. LinkedIn (user-authenticated session; opt-in; daily cap).
    if app.cfg.research.recruiter_linkedin_enabled {
        if let Some(b) = &app.browser {
            if linkedin_quota_ok(app).await? {
                match b
                    .linkedin_people(
                        &company,
                        &[
                            "technical recruiter",
                            "talent acquisition",
                            "hiring manager",
                        ],
                        10,
                    )
                    .await
                {
                    Ok(res) => {
                        if let Some(bl) = res.blocker {
                            app.notifier
                                .notify(Event::Error, &format!("LinkedIn recruiter search blocked ({}): {}. Log in via the browser worker (headed mode) to enable it.", bl.kind, bl.detail))
                                .await;
                        }
                        for p in res.people {
                            found.push(Recruiter {
                                company: company.clone(),
                                name: p.name,
                                rank_score: rank(&p.headline, &company),
                                title: Some(p.headline),
                                linkedin_url: Some(p.profile_url).filter(|u| !u.is_empty()),
                                source: "linkedin".into(),
                                ..Default::default()
                            });
                        }
                    }
                    Err(err) => {
                        app.metrics
                            .playwright_failures_total
                            .with_label_values(&["linkedin_people"])
                            .inc();
                        tracing::warn!(error = %err, "LinkedIn people search failed");
                    }
                }
            } else {
                tracing::info!("LinkedIn daily search limit reached");
            }
        }
    }

    found.sort_by_key(|r| std::cmp::Reverse(r.rank_score));
    found.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    let mut new_contacts = Vec::new();
    for (i, mut r) in found.into_iter().enumerate() {
        if i < 3 {
            r.outreach_draft = Some(outreach_draft(app, &person.profile, &r, &role_hint).await);
        }
        let r2 = r.clone();
        let (rid, is_new) = app.db.call(move |c| jdb::upsert_recruiter(c, &r2)).await?;
        if !is_new {
            continue;
        }
        app.metrics.recruiter_matches_total.inc();
        // Contacts with an email address (other than the sender we already
        // reply to) get a Thunderbird draft; others are delivered via Telegram.
        if let (Some(addr), Some(body)) = (&r.email, &r.outreach_draft) {
            if addr != &e.from_email {
                let d = drafts::NewDraft {
                    email_id: None,
                    kind: "new".into(),
                    account_id: e.account_id.clone(),
                    tb_message_id: None,
                    message_id: None,
                    to: vec![addr.clone()],
                    subject: format!(
                        "{} — {}",
                        if role_hint.is_empty() {
                            "AI engineering roles"
                        } else {
                            &role_hint
                        },
                        r.company
                    ),
                    body: body.clone(),
                    validation: "REVIEW_REQUIRED".into(),
                    notes: vec![format!(
                        "Recruiter outreach draft, written as {}",
                        person.name
                    )],
                    confidence: None,
                    from_address: e.account_email.clone(),
                    person_id: crate::jobs::person_id_of(&person),
                    llm_provider: None,
                    llm_model: None,
                };
                app.db.call(move |c| drafts::insert(c, &d)).await?;
                app.drafts_ready.notify_waiters();
            }
        }
        app.audit("research", "recruiter_discovered", "recruiter", &rid, json!({ "company": r.company, "name": r.name, "source": r.source, "rank": r.rank_score })).await;
        new_contacts.push(r);
    }
    if !new_contacts.is_empty() {
        let mut text = format!("🧑‍💼 Recruiters discovered at {company}\n");
        for r in &new_contacts {
            text.push_str(&format!(
                "\n• {} — {} (rank {})\n  {}",
                r.name,
                r.title.as_deref().unwrap_or("-"),
                r.rank_score,
                r.linkedin_url
                    .as_deref()
                    .or(r.email.as_deref())
                    .unwrap_or("")
            ));
            if r.email.is_none() {
                if let Some(d) = &r.outreach_draft {
                    text.push_str(&format!(
                        "\n  Draft message:\n  {}",
                        d.replace('\n', "\n  ")
                    ));
                }
            }
        }
        text.push_str("\n\nOutreach drafts are for your review; nothing was sent.");
        app.notifier.notify(Event::RecruiterDiscovered, &text).await;
    }
    Ok(())
}

/// Look for a job title in the sender's signature block.
pub fn signature_title(e: &EmailRecord) -> Option<String> {
    let name = display_name(&e.from).to_lowercase();
    let lines: Vec<&str> = e
        .body_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    for (i, l) in lines.iter().enumerate().rev().take(12) {
        if l.to_lowercase().contains(&name) && !name.is_empty() {
            if let Some(next) = lines.get(i + 1) {
                let n = next.to_lowercase();
                if [
                    "recruit", "talent", "hiring", "manager", "hr", "people", "engineer",
                    "director", "head",
                ]
                .iter()
                .any(|k| n.contains(k))
                    && next.len() < 80
                {
                    return Some(next.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranking_prefers_technical_recruiters() {
        let a = rank("Senior Technical Recruiter at Acme", "Acme");
        let b = rank("Talent Acquisition Partner", "Acme");
        let c = rank("Engineering Manager, ML Platform at Acme", "Acme");
        let d = rank("Software Engineer at Acme", "Acme");
        assert!(a > b && b > d, "{a} {b} {d}");
        assert!(c > d);
    }
}
