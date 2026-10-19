//! No-submit benchmark of the job agent on real data.
//!
//! 1. Replays every LinkedIn/Naukri alert job already stored: how many are
//!    screened out, found on the employer's own Greenhouse/Lever/Ashby board,
//!    or platform-only.
//! 2. Runs the real application flow on the best-matching employer-board
//!    postings, filling every field but never submitting and never uploading
//!    the resume (some boards send a file to the employer as soon as it is
//!    chosen). Nothing is written to the application tables and no Telegram
//!    questions are sent.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::app::App;
use crate::db::jobs::JobDetails;
use crate::error::{AppError, AppResult};
use crate::jobs::answers::{self, Answer};
use crate::jobs::matcher::{self, Decision};
use crate::jobs::{alerts, cover_letter, employer};
use crate::knowledge::Person;

#[derive(Debug, Default, Serialize)]
pub struct AlertReplay {
    pub alert_jobs: usize,
    pub screened_out: usize,
    pub anonymous_company: usize,
    pub on_employer_board: usize,
    pub platform_only: usize,
    pub found: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct JobRun {
    pub company: String,
    pub title: String,
    pub url: String,
    pub score: u8,
    pub decision: String,
    /// autonomous | needs_human | blocked | error
    pub outcome: String,
    pub reasons: Vec<String>,
    pub fields: usize,
    pub auto_answered: usize,
    pub resume_field: bool,
    pub cover_letter: bool,
    pub screenshot: Option<String>,
    pub seconds: f64,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub replay: AlertReplay,
    pub runs: Vec<JobRun>,
    pub autonomous: usize,
    pub needs_human: usize,
    pub blocked: usize,
    pub errors: usize,
}

fn is_board_url(u: &str) -> bool {
    ["greenhouse.io", "jobs.lever.co", "jobs.ashbyhq.com"]
        .iter()
        .any(|h| u.contains(h))
}

type StoredAlertJob = (String, String, Option<String>, Option<String>, Option<String>);

/// Step 1: what the new alert flow would do with every stored alert job.
async fn replay_alerts(app: &Arc<App>, person: &Person) -> AppResult<(AlertReplay, Vec<(JobDetails, String)>)> {
    let rows: Vec<StoredAlertJob> = app
        .db
        .call(|c| {
            let mut st = c.prepare(
                "SELECT source, title, company, location, url FROM jobs WHERE source IN ('linkedin','naukri')",
            )?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(v)
        })
        .await?;
    // Alert emails stored before this flow existed: parse them too.
    let bodies: Vec<(String, String)> = app
        .db
        .call(|c| {
            let mut st = c.prepare(
                "SELECT json_extract(classification,'$.job_signal.source'), body_text FROM emails
                 WHERE json_extract(classification,'$.job_signal.source') IN ('linkedin','naukri')",
            )?;
            let v = st
                .query_map([], |r| Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(v)
        })
        .await?;
    let mut listed: Vec<alerts::AlertJob> = Vec::new();
    for (src, body) in &bodies {
        listed.extend(alerts::parse(src, body));
    }
    for (_, title, company, location, url) in rows {
        listed.push(alerts::AlertJob { title, company, location, url, ..Default::default() });
    }
    let mut seen = std::collections::HashSet::new();
    listed.retain(|a| {
        seen.insert(format!("{}|{}", a.title.to_lowercase(), a.company.as_deref().unwrap_or("").to_lowercase()))
    });

    let mut r = AlertReplay { alert_jobs: listed.len(), ..Default::default() };
    let mut postings = Vec::new();
    for a in &listed {
        let m = matcher::score(
            &crate::jobs::details_from_alert(a),
            &person.profile,
            app.cfg.jobs.auto_apply_threshold,
            app.cfg.jobs.review_threshold,
        );
        if m.decision == Decision::Skip {
            r.screened_out += 1;
            continue;
        }
        if a.company.as_deref().is_none_or(alerts::is_anonymous) {
            r.anonymous_company += 1;
            continue;
        }
        match employer::find_posting(app, a).await? {
            Some(raw) => {
                r.on_employer_board += 1;
                r.found.push(format!(
                    "{} — {} → {}",
                    a.title,
                    a.company.as_deref().unwrap_or(""),
                    raw.details.url.as_deref().unwrap_or("")
                ));
                postings.push((raw.details, raw.source));
            }
            None => r.platform_only += 1,
        }
    }
    Ok((r, postings))
}

/// Step 2: fill one posting without submitting.
async fn dry_run(app: &Arc<App>, person: &Person, d: &JobDetails, score: u8, decision: Decision) -> JobRun {
    let started = Instant::now();
    let url = d.url.clone().unwrap_or_default();
    let mut run = JobRun {
        company: d.company.clone().unwrap_or_default(),
        title: d.title.clone(),
        url: url.clone(),
        score,
        decision: decision.as_str().into(),
        outcome: "error".into(),
        reasons: Vec::new(),
        fields: 0,
        auto_answered: 0,
        resume_field: false,
        cover_letter: false,
        screenshot: None,
        seconds: 0.0,
    };
    let res: AppResult<()> = async {
        let browser = app
            .browser
            .clone()
            .ok_or_else(|| AppError::other("browser worker not configured"))?;
        let key = format!("bench-{}", crate::db::new_id());
        let insp = browser.inspect(&key, &url).await?;
        if let Some(b) = insp.blocker {
            run.outcome = "blocked".into();
            run.reasons.push(format!("{}: {}", b.kind, b.detail));
            return Ok(());
        }
        if insp.fields.is_empty() {
            run.outcome = "blocked".into();
            run.reasons.push("unexpected_form: no fillable fields".into());
            return Ok(());
        }
        run.fields = insp.fields.len();
        let scope = crate::knowledge::Scope::for_person(person, Some(crate::knowledge::people::JOB_SEEKING));
        let memory_ns = if person.rel_dir.is_empty() { String::new() } else { format!("{}:", person.id) };
        let job_ctx = format!(
            "{} at {}\nLocation: {}\n{}",
            d.title,
            d.company.as_deref().unwrap_or(""),
            d.location.as_deref().unwrap_or(""),
            d.description.as_deref().unwrap_or("")
        );
        let mut answers_map: Map<String, Value> = Map::new();
        let mut file_fields: Vec<String> = Vec::new();
        let mut asked: Vec<String> = Vec::new();
        let mut cover: Option<String> = None;
        for f in &insp.fields {
            match answers::answer_field(f, &person.profile, &app.db, &app.knowledge, &app.llm, &job_ctx, &scope, &memory_ns)
                .await?
            {
                Answer::Value(v) => {
                    answers_map.insert(f.id.clone(), v);
                    run.auto_answered += 1;
                }
                Answer::ResumeFile => {
                    run.resume_field = true;
                    run.auto_answered += 1;
                    file_fields.push(f.id.clone());
                }
                Answer::CoverLetter => {
                    if cover.is_none() {
                        cover = Some(cover_letter::generate(&app.llm, &app.knowledge, d, &person.profile, &scope).await);
                    }
                    run.cover_letter = true;
                    run.auto_answered += 1;
                    answers_map.insert(f.id.clone(), Value::String(cover.clone().unwrap_or_default()));
                }
                Answer::Skip => {}
                Answer::Ask { reason } => asked.push(format!("{} ({reason})", f.label)),
            }
        }
        // Fill (never submit, no files) to catch anything the form still wants.
        let fill = browser.fill(&key, &url, &answers_map, &BTreeMap::new(), false).await?;
        run.screenshot = fill.screenshot_path.clone();
        if let Some(b) = fill.blocker {
            if b.kind != "upload_failure" {
                run.outcome = "blocked".into();
                run.reasons.push(format!("{}: {}", b.kind, b.detail));
                return Ok(());
            }
        }
        let still: Vec<String> = fill
            .missing_required
            .iter()
            .filter(|id| !file_fields.contains(id))
            .filter_map(|id| insp.fields.iter().find(|f| &f.id == id).map(|f| f.label.clone()).or(Some(id.clone())))
            .filter(|label| !asked.iter().any(|a| a.starts_with(label.as_str())))
            .map(|l| format!("{l} (still empty after filling)"))
            .collect();
        asked.extend(still);
        // Only a form that was really found and filled counts: the person's
        // name/email went in and at least three answers were given.
        let identity_filled = insp.fields.iter().any(|f| {
            let l = f.label.to_lowercase();
            (l.contains("email") || l.contains("name")) && answers_map.contains_key(&f.id)
        });
        if !identity_filled || run.auto_answered < 3 {
            run.outcome = "blocked".into();
            run.reasons.push(format!(
                "application form not reached ({} field(s) found, {} answered)",
                run.fields, run.auto_answered
            ));
        } else if asked.is_empty() {
            run.outcome = "autonomous".into();
        } else {
            run.outcome = "needs_human".into();
            run.reasons = asked;
        }
        Ok(())
    }
    .await;
    if let Err(e) = res {
        run.outcome = "error".into();
        run.reasons.push(e.to_string());
    }
    run.seconds = started.elapsed().as_secs_f64();
    run
}

/// Run the benchmark on up to `limit` employer-board postings.
pub async fn run(app: &Arc<App>, limit: usize) -> AppResult<Report> {
    let people = app.knowledge.people();
    let person = people.research_person();
    let (replay, found) = replay_alerts(app, &person).await?;

    // Candidates: postings found via alerts plus board jobs already discovered.
    let stored: Vec<(String, String)> = app
        .db
        .call(|c| {
            let mut st = c.prepare(
                "SELECT id, source FROM jobs WHERE source IN ('greenhouse','lever','ashby') AND url IS NOT NULL",
            )?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(v)
        })
        .await?;
    let mut candidates: Vec<JobDetails> = found.into_iter().map(|(d, _)| d).collect();
    for (id, _) in stored {
        let rec = app.db.call(move |c| crate::db::jobs::get_job(c, &id)).await?;
        candidates.push(rec.details);
    }
    let mut scored: Vec<(u8, Decision, JobDetails)> = candidates
        .into_iter()
        .filter(|d| d.url.as_deref().is_some_and(is_board_url))
        .map(|d| {
            let m = matcher::score(&d, &person.profile, app.cfg.jobs.auto_apply_threshold, app.cfg.jobs.review_threshold);
            (m.score, m.decision, d)
        })
        .filter(|(_, dec, _)| *dec != Decision::Skip)
        .collect();
    scored.sort_by_key(|x| std::cmp::Reverse(x.0));
    // Spread over companies: at most 4 postings from one employer.
    let mut per_company: BTreeMap<String, usize> = BTreeMap::new();
    let mut seen_urls = std::collections::HashSet::new();
    let mut picked = Vec::new();
    for (s, dec, d) in scored {
        let c = d.company.clone().unwrap_or_default().to_lowercase();
        let n = per_company.entry(c).or_default();
        if *n >= 4 || !seen_urls.insert(d.url.clone().unwrap_or_default()) {
            continue;
        }
        *n += 1;
        picked.push((s, dec, d));
        if picked.len() >= limit {
            break;
        }
    }
    let mut runs = Vec::new();
    for (i, (s, dec, d)) in picked.iter().enumerate() {
        println!("[{}/{}] {} — {}", i + 1, picked.len(), d.title, d.company.as_deref().unwrap_or(""));
        let r = dry_run(app, &person, d, *s, *dec).await;
        println!("        {} {}", r.outcome, r.reasons.join("; "));
        runs.push(r);
    }
    let count = |o: &str| runs.iter().filter(|r| r.outcome == o).count();
    Ok(Report {
        autonomous: count("autonomous"),
        needs_human: count("needs_human"),
        blocked: count("blocked"),
        errors: count("error"),
        replay,
        runs,
    })
}
