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
    /// Jobs handed to the job-links service (`JOB_LINKS_ENABLED`).
    pub queued: usize,
    /// Every job kept this run, with what other services need to act on it.
    pub kept: Vec<KeptJob>,
}

/// A kept job as handed to other services (e.g. the job-links queue).
#[derive(Debug, Clone, Serialize)]
pub struct KeptJob {
    pub job_id: String,
    /// Board or API it came from ("adzuna", "brave:linkedin", "greenhouse"…).
    pub source: String,
    pub posted_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The posting's link without tracking parameters.
    pub url: Option<String>,
    pub title: String,
    pub company: Option<String>,
    pub location: Option<String>,
    /// remote | hybrid | onsite | unknown
    pub remote: Option<String>,
    pub employment_type: Option<String>,
    pub salary: Option<String>,
    pub skills: Vec<String>,
    /// The first ~600 characters of the description.
    pub description_excerpt: Option<String>,
    pub score: i64,
    pub decision: String,
    /// Ranking points and what earned them (remote, contract…).
    pub priority_boost: i64,
    pub ranked_for: Vec<String>,
    /// False when an earlier run already stored it.
    pub is_new: bool,
}

/// Group drop reasons ("only for applicants in USA" → "only for applicants elsewhere").
fn drop_kind(reason: &str) -> String {
    if reason.starts_with("posted more than") {
        reason.to_string()
    } else if reason.starts_with("only for applicants") || reason.starts_with("description says") {
        "not open to applicants in your country".into()
    } else if reason.starts_with("on-site in") {
        "on-site abroad without visa sponsorship or relocation".into()
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
            None => crate::research::remote_boards::search_urls(board, &r.tech_stack),
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
    // Job APIs and search APIs (RESEARCH_SOURCES), within their request budget.
    out.extend(crate::research::job_apis::collect(&app.http, &r.apis, r.max_age_hours, &mut report.errors).await);
    report.collected = out.len();
    out
}

/// Jobs found through a search API are postings on LinkedIn, Naukri… that
/// the person applies to with the browser extension, not the job agent.
fn extension_platform(source: &str) -> Option<&str> {
    let (provider, site) = source.split_once(':')?;
    crate::research::search_apis::PROVIDERS
        .contains(&provider)
        .then_some(site)
}

/// `JOB_LINKS_ENABLED`: every kept job worth applying to goes to the shared
/// job database, where the Chrome extension applies from. Returns the
/// service's counts and the URLs sent.
async fn queue_kept(
    app: &Arc<App>,
    person: &crate::knowledge::Person,
    kept: &[KeptJob],
) -> AppResult<(crate::jobs::linkqueue::UpsertSummary, std::collections::HashSet<String>)> {
    use crate::jobs::linkqueue::{LinkQueueClient, LinkSource, QueuedLink};
    let links: Vec<QueuedLink> = kept
        .iter()
        .filter(|k| k.decision != Decision::Skip.as_str())
        .filter_map(|k| {
            Some(QueuedLink {
                url: k.url.clone()?,
                source: LinkSource::Research,
                source_detail: Some(k.source.clone()),
                title: Some(k.title.clone()),
                company: k.company.clone(),
                location: k.location.clone(),
                remote: k.remote.clone(),
                employment_type: k.employment_type.clone(),
                salary: k.salary.clone(),
                skills: k.skills.clone(),
                description: k.description_excerpt.clone(),
                score: Some(k.score.clamp(0, 100) as u8),
                decision: Some(k.decision.clone()),
                person_id: crate::jobs::person_id_of(person),
                resume_hint: person
                    .select_resume(&k.skills, Some(crate::knowledge::people::JOB_SEEKING))
                    .map(|(r, _)| r.file_name.clone()),
                posted_at: k.posted_at,
                ..Default::default()
            })
        })
        .collect();
    let summary = LinkQueueClient::from_app(app).upsert(&links).await?;
    Ok((summary, links.into_iter().map(|l| l.url).collect()))
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
    let mut seen = crate::research::dedupe::Seen::default();
    let mut kept = Vec::new();
    for j in raw {
        // The same job is often on several boards (same URL, or same company and title).
        if !seen.first(&j) {
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
    // (record, decision, rank boost, what earned the boost)
    let mut scored: Vec<(jdb::JobRecord, Decision, i64, Vec<String>)> = Vec::new();
    for mut j in kept {
        report.relevant += 1;
        let (boost, why) = crate::research::filters::priority(&j, &rc.preferred_work_types, rc.priority_boost);
        let src = j.source.split(':').next().unwrap_or("research").to_string();
        j.details.url = j.details.url.as_deref().map(crate::research::dedupe::canonical_link);
        let (posted_at, employment_type) = (j.posted_at, j.employment_type.clone());
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
                let d = &rec.details;
                report.kept.push(KeptJob {
                    job_id: rec.id.clone(),
                    source: j.source.clone(),
                    posted_at,
                    url: d.url.clone(),
                    title: d.title.clone(),
                    company: d.company.clone(),
                    location: d.location.clone(),
                    remote: d.remote.clone(),
                    employment_type,
                    salary: d.salary.clone(),
                    skills: d.skills.clone(),
                    description_excerpt: d.description.as_deref().map(|t| t.chars().take(600).collect()),
                    score: rec.score.unwrap_or(0),
                    decision: decision.as_str().to_string(),
                    priority_boost: boost,
                    ranked_for: why.clone(),
                    is_new,
                });
                if is_new {
                    report.new_jobs += 1;
                    app.metrics
                        .jobs_discovered_total
                        .with_label_values(&[&src])
                        .inc();
                    scored.push((rec, decision, boost, why));
                }
            }
            Err(e) => report.errors.push(format!("store: {e}")),
        }
    }
    // Remote and contract/freelance jobs first: match score plus the boost.
    scored.sort_by_key(|(r, _, boost, _)| std::cmp::Reverse(r.score.unwrap_or(0) + boost));
    report.kept.sort_by_key(|k| std::cmp::Reverse(k.score + k.priority_boost));
    let mut queued_urls = std::collections::HashSet::new();
    if app.cfg.jobs.links_enabled {
        match queue_kept(app, &research_person, &report.kept).await {
            Ok((s, urls)) => {
                queued_urls = urls;
                report.queued = s.created + s.updated;
                tracing::info!(created = s.created, updated = s.updated, failed = s.failed, "research jobs queued in job-links");
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not queue research jobs in job-links");
                report.errors.push(format!("job-links: {e}"));
            }
        }
    }
    let top_n = app.cfg.research.summary_top_n;
    report.top = scored
        .iter()
        .filter(|(_, d, _, _)| *d != Decision::Skip)
        .take(top_n)
        .map(|(r, _, _, _)| {
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
    let mut applying: Vec<(&jdb::JobRecord, &[String], Option<&'static str>)> = Vec::new();
    let mut not_applied: Vec<(&jdb::JobRecord, &[String], String)> = Vec::new();
    for (rec, d, _, ranked_for) in &scored {
        let why: Option<String> = if *d == Decision::Skip {
            Some(if rec.decision_reasons.is_empty() {
                "did not pass the match rules".to_string()
            } else {
                rec.decision_reasons.join("; ")
            })
        } else if rec.details.url.as_ref().is_some_and(|u| queued_urls.contains(u)) {
            Some("queued for the Chrome extension".into())
        } else if let Some(site) = extension_platform(&rec.source) {
            Some(format!("{site} posting: apply with the browser extension"))
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
                    applying.push((rec, ranked_for, note));
                    None
                }
                Err(e) => {
                    report.errors.push(format!("application start: {e}"));
                    Some(format!("could not start the application: {e}"))
                }
            }
        };
        if let Some(w) = why {
            not_applied.push((rec, ranked_for, w));
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
                crate::db::settings::set(c, LAST_RUN_DATE, &today)?;
                crate::db::settings::set(c, LAST_RUN_AT, &at)
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
    if rc.priority_boost > 0 {
        let mut first = vec!["remote".to_string()];
        first.extend(rc.preferred_work_types.iter().cloned());
        text.push_str(&format!("Ranked first: {}\n", first.join("/")));
    }
    if report.queued > 0 {
        text.push_str(&format!("Queued for the Chrome extension: {}\n", report.queued));
    }
    if !report.dropped.is_empty() {
        let parts: Vec<String> = report.dropped.iter().map(|(k, n)| format!("{n} {k}")).collect();
        text.push_str(&format!("Left out: {}\n", parts.join("; ")));
    }
    // Keep the message inside Telegram's limit.
    const MAX_LISTED: usize = 12;
    let line = |i: usize, r: &jdb::JobRecord, ranked_for: &[String]| {
        format!(
            "{}. [{}] {} — {}{}\n   {}\n",
            i + 1,
            r.score.unwrap_or(0),
            r.details.title,
            r.details.company.as_deref().unwrap_or("-"),
            if ranked_for.is_empty() { String::new() } else { format!(" ({})", ranked_for.join(", ")) },
            r.details.url.as_deref().unwrap_or("(no link)")
        )
    };
    if scored.is_empty() {
        text.push_str("\nNo new matching jobs today.");
    }
    if !applying.is_empty() {
        text.push_str(&format!("\n🤖 Applying now ({}):\n", applying.len()));
        for (i, (r, ranked_for, note)) in applying.iter().take(MAX_LISTED).enumerate() {
            text.push_str(&line(i, r, ranked_for));
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
        for (i, (r, ranked_for, why)) in not_applied.iter().take(MAX_LISTED).enumerate() {
            text.push_str(&line(i, r, ranked_for));
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
        .call(|c| crate::db::settings::get(c, LAST_RUN_DATE))
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
