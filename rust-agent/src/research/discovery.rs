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
        for term in &r.search_terms {
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
                    Err(e) => report.errors.push(format!("searxng: {e}")),
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
                        });
                    }
                }
            }
            Err(e) => report.errors.push(format!("career page {page_url}: {e}")),
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
    let terms = &app.cfg.research.search_terms;
    let mut scored: Vec<(jdb::JobRecord, Decision)> = Vec::new();
    for j in raw
        .into_iter()
        .filter(|j| matches_terms(&j.details.title, terms))
    {
        report.relevant += 1;
        let src = j.source.split(':').next().unwrap_or("research").to_string();
        match crate::jobs::score_and_store(app, &j.source, j.source_ref, j.details).await {
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

    // Auto-start applications for strong matches when the job agent is on.
    if app.cfg.jobs.enabled {
        for (rec, _) in scored.iter().filter(|(_, d)| *d != Decision::Skip) {
            if let Some(u) = rec.details.url.clone() {
                if let Err(e) = crate::jobs::application::start(app, rec, &u).await {
                    report.errors.push(format!("application start: {e}"));
                }
            }
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
    app.metrics
        .processing_duration_seconds
        .with_label_values(&["research"])
        .observe(started.elapsed().as_secs_f64());
    app.audit("research", "discovery_run", "research", "daily", json!({ "collected": report.collected, "relevant": report.relevant, "new": report.new_jobs, "errors": report.errors })).await;

    let mut text = format!(
        "🔎 Daily Job Discovery\nCollected: {} · Relevant: {} · New: {}\n",
        report.collected, report.relevant, report.new_jobs
    );
    if report.top.is_empty() {
        text.push_str("\nNo new matching jobs today.");
    } else {
        text.push_str("\nTop matches:\n");
        for (i, (title, company, score, url)) in report.top.iter().enumerate() {
            text.push_str(&format!(
                "{}. [{}] {} — {}\n   {}\n",
                i + 1,
                score,
                title,
                company,
                url
            ));
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
/// Queuing (instead of running inline) makes the run durable and retryable.
pub fn spawn_scheduler(app: Arc<App>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let wait = until_next(app.cfg.research.daily_time, Local::now());
            tracing::info!(
                in_secs = wait.as_secs(),
                "next research discovery scheduled"
            );
            tokio::time::sleep(wait).await;
            let key = format!("research:{}", Local::now().format("%Y-%m-%d"));
            if let Err(e) = app
                .queue
                .enqueue(crate::queue::KIND_RESEARCH, key, None, None, 2)
                .await
            {
                tracing::error!(error = %e, "failed to queue research run");
            }
            // Avoid double-firing within the same minute.
            tokio::time::sleep(std::time::Duration::from_secs(61)).await;
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
