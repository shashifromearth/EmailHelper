//! Daily job discovery: collect from all configured sources, filter by the
//! search terms, score against the resume, store and send a Telegram summary.

use std::sync::Arc;
use std::time::Instant;

use chrono::{Local, NaiveTime, TimeZone};
use serde::Serialize;
use serde_json::json;

use crate::app::App;
use crate::db::jobs as jdb;
use crate::error::AppResult;
use crate::jobs::matcher::Decision;
use crate::research::sources::{self, RawJob};
use crate::telegram::Event;

#[derive(Debug, Default, Clone, Serialize)]
pub struct DiscoveryReport {
    pub collected: usize,
    pub relevant: usize,
    pub new_jobs: usize,
    pub errors: Vec<String>,
    pub top: Vec<(String, String, i64, String)>,
    /// Why collected jobs were left out, with counts.
    pub dropped: std::collections::BTreeMap<String, usize>,
    /// Applications started for new matches.
    pub applying: usize,
}

/// Group drop reasons ("only for applicants in USA" → "only for applicants elsewhere").
fn drop_kind(reason: &str) -> String {
    if reason.starts_with("posted more than") {
        reason.to_string()
    } else if reason.starts_with("only for applicants") || reason.starts_with("description says") {
        "not open to applicants in your country".into()
    } else {
        reason.to_string()
    }
}

/// Does a job title match any search term? Generic words ("remote", "jobs",
/// "india") are ignored so "Remote AI Jobs" means "AI".
pub fn matches_terms(title: &str, terms: &[String]) -> bool {
    const GENERIC: &[&str] = &["remote", "jobs", "job", "india", "hiring", "role", "roles"];
    let t = title.to_lowercase();
    terms.iter().any(|term| {
        let words: Vec<String> = term
            .to_lowercase()
            .split_whitespace()
            .filter(|w| !GENERIC.contains(w))
            .map(str::to_string)
            .collect();
        !words.is_empty()
            && words
                .iter()
                .all(|w| crate::jobs::extract::contains_word(&t, w))
    })
}

async fn collect(app: &App, report: &mut DiscoveryReport) -> Vec<RawJob> {
    let r = &app.cfg.research;
    let mut out: Vec<RawJob> = Vec::new();

    if let Some(base) = &r.searxng_url {
        'searx: for term in &r.search_terms {
            for q in [
                format!("{term} site:boards.greenhouse.io OR site:jobs.lever.co OR site:jobs.ashbyhq.com"),
                format!("{term} job opening apply"),
            ] {
                match sources::searxng_search(&app.http, base, &q, r.max_results_per_query).await {
                    Ok(hits) => {
                        for h in &hits {
                            let (u, t, c) = (h.url.clone(), h.title.clone(), h.content.clone());
                            let qq = q.clone();
                            let _ = app
                                .db
                                .call(move |db| jdb::save_finding(db, &qq, &u, &t, &c, "searxng"))
                                .await;
                            if let Some(j) = sources::job_from_search(h) {
                                out.push(j);
                            }
                        }
                    }
                    Err(e) => {
                        let unreachable = matches!(&e, crate::error::AppError::Http(re) if re.is_connect() || re.is_timeout());
                        report.errors.push(format!("searxng: {e}"));
                        if unreachable {
                            // One error is enough; don't hammer a server that isn't there.
                            break 'searx;
                        }
                    }
                }
            }
        }
    }
    for feed in &r.rss_feeds {
        match sources::fetch_feed(&app.http, feed).await {
            Ok(mut j) => out.append(&mut j),
            Err(e) => report.errors.push(format!("rss {feed}: {e}")),
        }
    }
    for b in &r.greenhouse_boards {
        let url = format!(
            "{}/{}/jobs?content=true",
            r.greenhouse_api_base.trim_end_matches('/'),
            b
        );
        match sources::fetch_json(&app.http, &url).await {
            Ok(v) => out.extend(sources::parse_greenhouse(b, &v)),
            Err(e) => report.errors.push(format!("greenhouse {b}: {e}")),
        }
    }
    for b in &r.lever_boards {
        let url = format!("{}/{}?mode=json", r.lever_api_base.trim_end_matches('/'), b);
        match sources::fetch_json(&app.http, &url).await {
            Ok(v) => out.extend(sources::parse_lever(b, &v)),
            Err(e) => report.errors.push(format!("lever {b}: {e}")),
        }
    }
    for b in &r.ashby_boards {
        let url = format!(
            "{}/{}?includeCompensation=true",
            r.ashby_api_base.trim_end_matches('/'),
            b
        );
        match sources::fetch_json(&app.http, &url).await {
            Ok(v) => out.extend(sources::parse_ashby(b, &v)),
            Err(e) => report.errors.push(format!("ashby {b}: {e}")),
        }
    }
    // Company career pages: open via the browser worker (JS-heavy sites).
    for page_url in &r.career_pages {
        match crate::jobs::fetch_page(app, page_url).await {
            Ok(p) => {
                for l in p
                    .links
                    .iter()
                    .filter(|l| matches_terms(&l.text, &r.search_terms))
                {
                    if let Some(link) = crate::jobs::detect::classify_url(&l.href) {
                        let (title, company) = crate::jobs::extract::split_title(&l.text);
                        out.push(RawJob {
                            source: "career_page".into(),
                            source_ref: Some(page_url.clone()),
                            details: jdb::JobDetails {
                                title,
                                company: company.or_else(|| {
                                    crate::jobs::detect::company_from_domain(&format!(
                                        "x@{}",
                                        url::Url::parse(page_url)
                                            .ok()
                                            .and_then(|u| u.host_str().map(str::to_string))
                                            .unwrap_or_default()
                                    ))
                                }),
                                url: Some(link.url),
                                ..Default::default()
                            },
                            // a career page gives no date or region
                            posted_at: None,
                            region: None,
                            employment_type: None,
                        });
                    }
                }
            }
            Err(e) => report.errors.push(format!("career page {page_url}: {e}")),
        }
    }
    // Aggregators last: when the same job is on the employer's own board,
    // that version (its real location and date) is the one judged.
    for board in &r.remote_boards {
        let urls: Vec<String> = match r.remote_board_urls.iter().find(|(b, _)| b == board) {
            Some((_, u)) => vec![u.clone()],
            None => crate::research::remote_boards::default_urls(board)
                .iter()
                .map(|u| u.to_string())
                .collect(),
        };
        let mut seen_urls = std::collections::HashSet::new();
        for url in urls {
            match crate::research::remote_boards::fetch(&app.http, board, &url).await {
                Ok(jobs) => out.extend(
                    jobs.into_iter()
                        .filter(|j| seen_urls.insert(j.details.url.clone().unwrap_or_default())),
                ),
                Err(e) => report.errors.push(format!("{board} {url}: {e}")),
            }
        }
    }
    report.collected = out.len();
    out
}

/// Run one discovery pass.
pub async fn run(app: &Arc<App>) -> AppResult<DiscoveryReport> {
    let started = Instant::now();
    let mut report = DiscoveryReport::default();
    let raw = collect(app, &mut report).await;
    // Discovery scores jobs for one person (people.json `research_person`).
    let research_person = app.knowledge.people().research_person();
    let terms = &app.cfg.research.search_terms;
    let rc = &app.cfg.research;
    let criteria = crate::research::filters::Criteria {
        max_age_hours: rc.max_age_hours,
        country: rc
            .applicant_country
            .clone()
            .or_else(|| research_person.profile.authorized_countries().first().map(|c| c.to_string()))
            .unwrap_or_else(|| "India".into()),
        stack: rc.tech_stack.clone(),
        work_types: rc.work_types.clone(),
        require_remote: rc.require_remote,
    };
    let now = chrono::Utc::now();
    let mut seen = std::collections::HashSet::new();
    let mut kept = Vec::new();
    for j in raw {
        // The same job is often on several boards.
        let key = format!(
            "{}|{}",
            j.details.title.to_lowercase(),
            j.details.company.as_deref().unwrap_or("").to_lowercase()
        );
        if !seen.insert(key) {
            *report.dropped.entry("duplicate on another board".into()).or_default() += 1;
            continue;
        }
        if criteria.stack.is_empty() && !matches_terms(&j.details.title, terms) {
            *report.dropped.entry("does not match the search terms".into()).or_default() += 1;
            continue;
        }
        match crate::research::filters::check(&j, &criteria, now) {
            Ok(_) => kept.push(j),
            Err(reason) => *report.dropped.entry(drop_kind(&reason)).or_default() += 1,
        }
    }
    let mut scored: Vec<(jdb::JobRecord, Decision)> = Vec::new();
    for j in kept {
        report.relevant += 1;
        let src = j.source.split(':').next().unwrap_or("research").to_string();
        match crate::jobs::score_and_store(
            app,
            &research_person,
            &j.source,
            j.source_ref,
            j.details,
        )
        .await
        {
            Ok((rec, decision, is_new)) => {
                if is_new {
                    report.new_jobs += 1;
                    app.metrics
                        .jobs_discovered_total
                        .with_label_values(&[&src])
                        .inc();
                    scored.push((rec, decision));
                }
            }
            Err(e) => report.errors.push(format!("store: {e}")),
        }
    }
    scored.sort_by_key(|(r, _)| std::cmp::Reverse(r.score));
    let top_n = app.cfg.research.summary_top_n;
    report.top = scored
        .iter()
        .filter(|(_, d)| *d != Decision::Skip)
        .take(top_n)
        .map(|(r, _)| {
            (
                r.details.title.clone(),
                r.details.company.clone().unwrap_or_default(),
                r.score.unwrap_or(0),
                r.details.url.clone().unwrap_or_default(),
            )
        })
        .collect();

    // Discovered jobs are only applied to when explicitly enabled: they come
    // from the open web rather than from mail addressed to the user. Every
    // kept job ends up either applying or in the summary with the reason.
    let mut applying: Vec<(&jdb::JobRecord, Option<&'static str>)> = Vec::new();
    let mut not_applied: Vec<(&jdb::JobRecord, String)> = Vec::new();
    for (rec, d) in &scored {
        let why: Option<String> = if *d == Decision::Skip {
            Some(if rec.decision_reasons.is_empty() {
                "did not pass the match rules".to_string()
            } else {
                rec.decision_reasons.join("; ")
            })
        } else if !app.cfg.jobs.enabled {
            Some("the job agent is off (JOB_AGENT_ENABLED)".into())
        } else if !app.cfg.research.auto_apply {
            Some("automatic applying is off (RESEARCH_AUTO_APPLY)".into())
        } else if rec.details.url.is_none() {
            Some("no application link".into())
        } else {
            let u = rec.details.url.clone().unwrap_or_default();
            match crate::jobs::application::start(app, rec, &u).await {
                Ok(_) => {
                    report.applying += 1;
                    let note = (*d == Decision::Review).then_some("needs your OK on Telegram first");
                    applying.push((rec, note));
                    None
                }
                Err(e) => {
                    report.errors.push(format!("application start: {e}"));
                    Some(format!("could not start the application: {e}"))
                }
            }
        };
        if let Some(w) = why {
            not_applied.push((rec, w));
        }
    }

    let status = if report.errors.is_empty() {
        "ok"
    } else if report.new_jobs > 0 || report.collected > 0 {
        "partial"
    } else {
        "error"
    };
    app.metrics
        .research_runs_total
        .with_label_values(&[status])
        .inc();
    if status != "error" {
        // Today's discovery is done; the scheduler won't run it again today.
        let today = Local::now().format("%Y-%m-%d").to_string();
        let at = crate::db::now();
        if let Err(e) = app
            .db
            .call(move |c| {
                crate::db::set_setting(c, LAST_RUN_DATE, &today)?;
                crate::db::set_setting(c, LAST_RUN_AT, &at)
            })
            .await
        {
            tracing::warn!(error = %e, "could not record research run");
        }
    }
    app.metrics
        .processing_duration_seconds
        .with_label_values(&["research"])
        .observe(started.elapsed().as_secs_f64());
    app.audit("research", "discovery_run", "research", "daily", json!({ "collected": report.collected, "relevant": report.relevant, "new": report.new_jobs, "errors": report.errors })).await;

    let mut text = format!(
        "🔎 Daily Job Discovery\nCollected: {} · Kept: {} · New: {}\n",
        report.collected, report.relevant, report.new_jobs
    );
    if rc.max_age_hours.is_some() || !rc.work_types.is_empty() || rc.require_remote {
        text.push_str(&format!(
            "Filters: posted in the last {} h · open to applicants in {} · {}{} · {}\n",
            rc.max_age_hours.map(|h| h.to_string()).unwrap_or_else(|| "∞".into()),
            criteria.country,
            if rc.require_remote { "remote · " } else { "" },
            if rc.work_types.is_empty() { "any role type".to_string() } else { rc.work_types.join("/") },
            if rc.tech_stack.is_empty() { "search terms".to_string() } else { rc.tech_stack.join(", ") },
        ));
    }
    if !report.dropped.is_empty() {
        let parts: Vec<String> = report.dropped.iter().map(|(k, n)| format!("{n} {k}")).collect();
        text.push_str(&format!("Left out: {}\n", parts.join("; ")));
    }
    // Keep the message inside Telegram's limit.
    const MAX_LISTED: usize = 12;
    let line = |i: usize, r: &jdb::JobRecord| {
        format!(
            "{}. [{}] {} — {}\n   {}\n",
            i + 1,
            r.score.unwrap_or(0),
            r.details.title,
            r.details.company.as_deref().unwrap_or("-"),
            r.details.url.as_deref().unwrap_or("(no link)")
        )
    };
    if scored.is_empty() {
        text.push_str("\nNo new matching jobs today.");
    }
    if !applying.is_empty() {
        text.push_str(&format!("\n🤖 Applying now ({}):\n", applying.len()));
        for (i, (r, note)) in applying.iter().take(MAX_LISTED).enumerate() {
            text.push_str(&line(i, r));
            if let Some(n) = note {
                text.push_str(&format!("   ({n})\n"));
            }
        }
        if applying.len() > MAX_LISTED {
            text.push_str(&format!("…and {} more (see the dashboard)\n", applying.len() - MAX_LISTED));
        }
        text.push_str("If an application gets stuck you'll get a message with the link and the reason.\n");
    }
    if !not_applied.is_empty() {
        text.push_str(&format!(
            "\n📝 Not applied — apply yourself if interested ({}):\n",
            not_applied.len()
        ));
        for (i, (r, why)) in not_applied.iter().take(MAX_LISTED).enumerate() {
            text.push_str(&line(i, r));
            text.push_str(&format!("   Why not: {why}\n"));
        }
        if not_applied.len() > MAX_LISTED {
            text.push_str(&format!("…and {} more (see the dashboard)\n", not_applied.len() - MAX_LISTED));
        }
    }
    if !report.errors.is_empty() {
        text.push_str(&format!(
            "\n⚠️ {} source error(s); see logs.",
            report.errors.len()
        ));
    }
    app.notifier.notify(Event::JobsDiscovered, &text).await;
    tracing::info!(
        collected = report.collected,
        relevant = report.relevant,
        new = report.new_jobs,
        errors = report.errors.len(),
        "research discovery finished"
    );
    Ok(report)
}

/// Duration until the next local `at` time.
pub fn until_next(at: NaiveTime, now: chrono::DateTime<Local>) -> std::time::Duration {
    let today = now.date_naive().and_time(at);
    let target = Local
        .from_local_datetime(&today)
        .earliest()
        .filter(|t| *t > now)
        .unwrap_or_else(|| {
            Local
                .from_local_datetime(&(today + chrono::Duration::days(1)))
                .earliest()
                .unwrap_or(now + chrono::Duration::days(1))
        });
    (target - now).to_std().unwrap_or_default()
}

/// Daily scheduler: sleeps until the configured time, then queues a run.
/// Settings keys: local date ("2026-10-20") and time of the last successful run.
pub const LAST_RUN_DATE: &str = "research.last_run_date";
pub const LAST_RUN_AT: &str = "research.last_run_at";

/// (ran today?, last run date) for the status API and the scheduler.
pub async fn ran_today(app: &App) -> (bool, Option<String>) {
    let last = app
        .db
        .call(|c| crate::db::get_setting(c, LAST_RUN_DATE))
        .await
        .ok()
        .flatten();
    let today = Local::now().format("%Y-%m-%d").to_string();
    (last.as_deref() == Some(today.as_str()), last)
}

/// Check now and then every `RESEARCH_CHECK_INTERVAL_HOURS`: if today's
/// discovery hasn't run yet, queue it (queued work is durable and retried;
/// the per-day key stops a second run while one is waiting).
pub fn spawn_scheduler(app: Arc<App>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let every = std::time::Duration::from_secs(app.cfg.research.check_interval_hours * 3600);
        loop {
            let (done, last) = ran_today(&app).await;
            if done {
                tracing::info!(last_run = ?last, "research discovery already ran today");
            } else {
                let key = format!("research:{}", Local::now().format("%Y-%m-%d"));
                tracing::info!(last_run = ?last, "research discovery has not run today; running now");
                if let Err(e) = app
                    .queue
                    .enqueue(crate::queue::KIND_RESEARCH, key, None, None, 2)
                    .await
                {
                    tracing::error!(error = %e, "failed to queue research run");
                }
            }
            tokio::time::sleep(every).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn term_matching() {
        let terms: Vec<String> = crate::config::DEFAULT_SEARCH_TERMS
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(matches_terms("Staff AI Engineer, Platform", &terms));
        assert!(matches_terms("Senior Rust Engineer", &terms));
        assert!(matches_terms("Principal GenAI Architect", &terms));
        assert!(
            matches_terms("AI Research Scientist", &terms),
            "'Remote AI Jobs' reduces to 'AI'"
        );
        assert!(!matches_terms("Senior Accountant", &terms));
    }

    #[test]
    fn next_run_is_in_future() {
        let now = Local::now();
        let d = until_next(NaiveTime::from_hms_opt(7, 30, 0).unwrap(), now);
        assert!(d.as_secs() <= 24 * 3600 + 3600);
    }
}
