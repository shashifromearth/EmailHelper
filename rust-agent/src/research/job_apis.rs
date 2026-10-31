//! Official job-search APIs: Adzuna, Jooble, Careerjet, Arbeitnow and The
//! Muse, plus the web-search APIs in `search_apis`. Each source is read once
//! a day with an identifying User-Agent, a pause between requests and a
//! request budget (a few hundred requests a day in total). A keyed source is
//! skipped, with one log line, until its key is set.

use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, Utc};
use serde_json::{json, Value};

use crate::config::ApiSourcesConfig;
use crate::db::jobs::JobDetails;
use crate::jobs::extract::{self, html_to_text};
use crate::research::remote_boards::parse_date;
use crate::research::search_apis;
use crate::research::sources::RawJob;

/// Source names accepted in RESEARCH_SOURCES.
pub const SOURCES: &[&str] = &["adzuna", "jooble", "careerjet", "arbeitnow", "themuse", "google_cse", "brave"];

pub const USER_AGENT: &str = "EmailHelper-research/1.0 (personal job search; a few dozen requests per API per day)";

/// Request budget and pacing for one source within a run.
pub struct Pacer {
    delay: Duration,
    budget: usize,
    used: usize,
    last: Option<Instant>,
}

impl Pacer {
    pub fn new(delay: Duration, budget: usize) -> Self {
        Self { delay, budget, used: 0, last: None }
    }

    /// Wait for this source's next request; false when its budget is spent.
    pub async fn turn(&mut self) -> bool {
        if self.used >= self.budget {
            return false;
        }
        if let Some(t) = self.last {
            let since = t.elapsed();
            if since < self.delay {
                tokio::time::sleep(self.delay - since).await;
            }
        }
        self.used += 1;
        self.last = Some(Instant::now());
        true
    }

    pub fn used(&self) -> usize {
        self.used
    }

    pub fn budget(&self) -> usize {
        self.budget
    }
}

/// Why a request failed: `Stop` ends the source for this run (rate limit,
/// rejected key, unreachable), `Skip` moves on to its next request.
pub(crate) enum Fail {
    Stop(String),
    Skip(String),
}

pub(crate) async fn send(req: reqwest::RequestBuilder) -> Result<Value, Fail> {
    let resp = req
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| Fail::Stop(e.to_string()))?;
    let st = resp.status();
    if st.as_u16() == 429 {
        return Err(Fail::Stop("rate limited (429); stopped for today".into()));
    }
    if st.as_u16() == 401 || st.as_u16() == 403 {
        return Err(Fail::Stop(format!("key rejected or quota used up ({st})")));
    }
    if !st.is_success() {
        return Err(Fail::Skip(format!("HTTP {st}")));
    }
    resp.json().await.map_err(|e| Fail::Skip(format!("unreadable response: {e}")))
}

/// Run `build` for each item in turn (within the pacer's budget) and parse
/// each response.
pub(crate) async fn each<T>(
    name: &str,
    items: &[T],
    pacer: &mut Pacer,
    errors: &mut Vec<String>,
    build: impl Fn(&T) -> reqwest::RequestBuilder,
    parse: impl Fn(&T, &Value) -> Vec<RawJob>,
) -> Vec<RawJob> {
    let mut out = Vec::new();
    for it in items {
        if !pacer.turn().await {
            break;
        }
        match send(build(it)).await {
            Ok(v) => out.extend(parse(it, &v)),
            Err(Fail::Stop(e)) => {
                tracing::warn!(source = name, error = %e, "research source stopped for this run");
                errors.push(format!("{name}: {e}"));
                break;
            }
            Err(Fail::Skip(e)) => {
                tracing::warn!(source = name, error = %e, "research source request failed");
                errors.push(format!("{name}: {e}"));
            }
        }
    }
    out
}

/// The `n` items used today out of `all`: a window that moves each day, so a
/// long list of queries is covered over a few days within the budget.
/// `slot` gives sources sharing one list (Google, Brave) adjacent windows.
pub fn todays<T: Clone>(all: &[T], n: usize, day: u32, slot: usize) -> Vec<T> {
    if n == 0 {
        return Vec::new();
    }
    if all.len() <= n {
        return all.to_vec();
    }
    let start = ((day as usize * 2 + slot) * n) % all.len();
    (0..n).map(|i| all[(start + i) % all.len()].clone()).collect()
}

/// The setting a source still needs, if any.
pub fn missing_key(cfg: &ApiSourcesConfig, source: &str) -> Option<&'static str> {
    match source {
        "adzuna" if cfg.adzuna_app_id.is_none() || cfg.adzuna_app_key.is_none() => Some("ADZUNA_APP_ID / ADZUNA_APP_KEY"),
        "jooble" if cfg.jooble_api_key.is_none() => Some("JOOBLE_API_KEY"),
        "careerjet" if cfg.careerjet_affid.is_none() => Some("CAREERJET_AFFID"),
        "google_cse" if cfg.google_cse_key.is_none() || cfg.google_cse_cx.is_none() => Some("GOOGLE_CSE_KEY / GOOGLE_CSE_CX"),
        "brave" if cfg.brave_search_key.is_none() => Some("BRAVE_SEARCH_KEY"),
        _ => None,
    }
}

/// Each API's own pause between requests (its published rate limit, with room).
fn default_delay_ms(source: &str) -> u64 {
    match source {
        "adzuna" => 2_500, // 25 requests a minute
        "brave" => 1_100,  // 1 request a second on the free plan
        "themuse" | "google_cse" => 500,
        _ => 1_000,
    }
}

/// Read every configured API source, within the request budget.
pub async fn collect(
    http: &reqwest::Client,
    cfg: &ApiSourcesConfig,
    max_age_hours: Option<u32>,
    errors: &mut Vec<String>,
) -> Vec<RawJob> {
    let mut left = cfg.max_requests;
    let mut out = Vec::new();
    let now = Utc::now();
    let day = now.ordinal();
    let days = max_age_hours.map(|h| h.div_ceil(24).max(1));
    for src in &cfg.sources {
        let name = src.trim().to_lowercase();
        if !SOURCES.contains(&name.as_str()) {
            tracing::warn!(source = %name, "unknown research source in RESEARCH_SOURCES; ignored");
            continue;
        }
        if let Some(key) = missing_key(cfg, &name) {
            tracing::info!(source = %name, "research source skipped: {key} not set");
            continue;
        }
        if left == 0 {
            tracing::warn!(source = %name, "research API request budget used up (RESEARCH_API_MAX_REQUESTS)");
            break;
        }
        let delay = Duration::from_millis(cfg.delay_ms.unwrap_or_else(|| default_delay_ms(&name)));
        let mut pacer = Pacer::new(delay, cfg.max_requests_per_source.min(left));
        let jobs = match name.as_str() {
            "adzuna" => adzuna(http, cfg, days, day, &mut pacer, errors).await,
            "jooble" => jooble(http, cfg, days, day, &mut pacer, errors).await,
            "careerjet" => careerjet(http, cfg, day, &mut pacer, errors).await,
            "arbeitnow" => {
                let pages: Vec<usize> = (1..=cfg.arbeitnow_pages).collect();
                each("arbeitnow", &pages, &mut pacer, errors, |p| {
                    http.get(&cfg.arbeitnow_api_base).query(&[("page", p.to_string())])
                }, |_, v| parse_arbeitnow(v))
                .await
            }
            "themuse" => themuse(http, cfg, &mut pacer, errors).await,
            "google_cse" => search_apis::google(http, cfg, days, day, &mut pacer, errors, now).await,
            _ => search_apis::brave(http, cfg, days, day, &mut pacer, errors, now).await,
        };
        left = left.saturating_sub(pacer.used());
        tracing::info!(source = %name, jobs = jobs.len(), requests = pacer.used(), "research source read");
        out.extend(jobs);
    }
    out
}

// ---------------------------------------------------------------- helpers

fn s(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|x| !x.is_empty()).map(str::to_string)
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|t| t.trim().replace(',', "").parse().ok()))
        .filter(|n| *n > 0.0)
}

/// Country name for an ISO code, so filters can tell where a job is.
pub fn country_name(code: &str) -> Option<&'static str> {
    Some(match code.to_ascii_lowercase().as_str() {
        "in" => "India",
        "us" => "United States",
        "gb" | "uk" => "United Kingdom",
        "de" => "Germany",
        "sg" => "Singapore",
        "nl" => "Netherlands",
        "ae" => "United Arab Emirates",
        "au" => "Australia",
        "ca" => "Canada",
        "fr" => "France",
        "ie" => "Ireland",
        "nz" => "New Zealand",
        "ch" => "Switzerland",
        "at" => "Austria",
        "be" => "Belgium",
        "es" => "Spain",
        "it" => "Italy",
        "pl" => "Poland",
        "za" => "South Africa",
        "br" => "Brazil",
        "mx" => "Mexico",
        _ => return None,
    })
}

fn currency(country: &str) -> Option<&'static str> {
    Some(match country {
        "in" => "INR",
        "us" => "USD",
        "gb" => "GBP",
        "de" | "nl" | "fr" | "at" | "be" | "es" | "it" | "ie" => "EUR",
        "sg" => "SGD",
        "au" => "AUD",
        "ca" => "CAD",
        "ch" => "CHF",
        "nz" => "NZD",
        "pl" => "PLN",
        "za" => "ZAR",
        "br" => "BRL",
        "mx" => "MXN",
        _ => return None,
    })
}

/// "Bengaluru" + "India" → "Bengaluru, India" (unless already named).
fn with_country(place: Option<String>, country: Option<&str>) -> Option<String> {
    match (place, country) {
        (Some(p), Some(c)) if !p.to_lowercase().contains(&c.to_lowercase()) => Some(format!("{p}, {c}")),
        (Some(p), _) => Some(p),
        (None, c) => c.map(str::to_string),
    }
}

#[allow(clippy::too_many_arguments)]
fn job(
    source: &str,
    id: Option<String>,
    title: &str,
    company: Option<String>,
    url: String,
    location: Option<String>,
    employment_type: Option<String>,
    posted_at: Option<DateTime<Utc>>,
    description_html: &str,
    tags: Vec<String>,
) -> RawJob {
    let description = html_to_text(description_html);
    let mut d = JobDetails {
        title: html_to_text(title),
        company,
        url: Some(url),
        location: location.clone(),
        description: Some(description.chars().take(20_000).collect()),
        skills: tags,
        ..Default::default()
    };
    let text = format!("{} {description}", d.title);
    extract::enrich_from_text(&mut d, &text, &[]);
    RawJob {
        source: source.into(),
        source_ref: id.map(|i| format!("{source}:{i}")),
        details: d,
        posted_at,
        region: location,
        employment_type,
    }
}

fn set_salary(d: &mut JobDetails, lo: Option<f64>, hi: Option<f64>, cur: Option<&str>) {
    if lo.is_none() && hi.is_none() {
        return;
    }
    d.salary_min = lo;
    d.salary_max = hi.or(lo);
    d.salary_currency = cur.map(str::to_string);
    d.salary = Some(format!(
        "{} {}–{}",
        cur.unwrap_or(""),
        lo.map(|v| v.to_string()).unwrap_or_default(),
        hi.map(|v| v.to_string()).unwrap_or_default()
    ));
}

// ---------------------------------------------------------------- Adzuna

/// GET /v1/api/jobs/{country}/search/1 — `results[]` with title, company,
/// location.area, created, contract_type/time, salary_min/max, redirect_url.
pub fn parse_adzuna(country: &str, v: &Value) -> Vec<RawJob> {
    let cc = country.to_ascii_lowercase();
    v["results"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|j| {
                    let area: Vec<String> = j["location"]["area"]
                        .as_array()
                        .map(|a| a.iter().rev().filter_map(s).collect())
                        .unwrap_or_default();
                    let place = s(&j["location"]["display_name"]).or_else(|| (!area.is_empty()).then(|| area.join(", ")));
                    let kind: Vec<String> = [s(&j["contract_type"]), s(&j["contract_time"])].into_iter().flatten().collect();
                    let mut r = job(
                        "adzuna",
                        s(&j["id"]).or_else(|| j["id"].as_i64().map(|i| i.to_string())).map(|i| format!("{cc}:{i}")),
                        &s(&j["title"])?,
                        s(&j["company"]["display_name"]),
                        s(&j["redirect_url"])?,
                        with_country(place, country_name(&cc)),
                        (!kind.is_empty()).then(|| kind.join(", ")),
                        parse_date(&j["created"]),
                        j["description"].as_str().unwrap_or(""),
                        s(&j["category"]["label"]).into_iter().collect(),
                    );
                    // Adzuna estimates salaries it isn't given; only real ones count.
                    if j["salary_is_predicted"].as_str() != Some("1") {
                        set_salary(&mut r.details, num(&j["salary_min"]), num(&j["salary_max"]), currency(&cc));
                    }
                    Some(r)
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn adzuna(
    http: &reqwest::Client,
    cfg: &ApiSourcesConfig,
    days: Option<u32>,
    day: u32,
    pacer: &mut Pacer,
    errors: &mut Vec<String>,
) -> Vec<RawJob> {
    let (id, key) = (cfg.adzuna_app_id.clone().unwrap_or_default(), cfg.adzuna_app_key.clone().unwrap_or_default());
    let mut combos = Vec::new();
    for q in &cfg.queries {
        for c in &cfg.adzuna_countries {
            combos.push((c.to_ascii_lowercase(), q.clone()));
        }
    }
    let combos = todays(&combos, pacer.budget(), day, 0);
    each("adzuna", &combos, pacer, errors, |(c, q)| {
        let mut params = vec![
            ("app_id", id.clone()),
            ("app_key", key.clone()),
            ("results_per_page", "50".to_string()),
            ("what", q.clone()),
            ("sort_by", "date".to_string()),
            ("content-type", "application/json".to_string()),
        ];
        if let Some(d) = days {
            params.push(("max_days_old", d.to_string()));
        }
        http.get(format!("{}/{c}/search/1", cfg.adzuna_api_base.trim_end_matches('/'))).query(&params)
    }, |(c, _), v| parse_adzuna(c, v))
    .await
}

// ---------------------------------------------------------------- Jooble

/// POST /api/{key} — `jobs[]` with title, location, snippet, salary, type,
/// link, company, updated.
pub fn parse_jooble(v: &Value) -> Vec<RawJob> {
    v["jobs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|j| {
                    let mut r = job(
                        "jooble",
                        s(&j["id"]).or_else(|| j["id"].as_i64().map(|i| i.to_string())),
                        &s(&j["title"])?,
                        s(&j["company"]),
                        s(&j["link"])?,
                        s(&j["location"]),
                        s(&j["type"]),
                        parse_date(&j["updated"]),
                        j["snippet"].as_str().unwrap_or(""),
                        Vec::new(),
                    );
                    if let Some(sal) = s(&j["salary"]) {
                        let (lo, hi, cur) = extract::parse_salary_range(&sal);
                        r.details.salary_min = lo;
                        r.details.salary_max = hi;
                        r.details.salary_currency = cur;
                        r.details.salary = Some(sal);
                    }
                    Some(r)
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn jooble(
    http: &reqwest::Client,
    cfg: &ApiSourcesConfig,
    days: Option<u32>,
    day: u32,
    pacer: &mut Pacer,
    errors: &mut Vec<String>,
) -> Vec<RawJob> {
    let key = cfg.jooble_api_key.clone().unwrap_or_default();
    let since = days.map(|d| (Utc::now() - chrono::Duration::days(d as i64)).format("%Y-%m-%d").to_string());
    let mut combos = Vec::new();
    for q in &cfg.queries {
        for l in &cfg.jooble_locations {
            combos.push((q.clone(), l.clone()));
        }
    }
    let combos = todays(&combos, pacer.budget(), day, 0);
    each("jooble", &combos, pacer, errors, |(q, l)| {
        let mut body = json!({"keywords": q, "location": l, "page": "1", "ResultOnPage": "50"});
        if let Some(d) = &since {
            body["datecreatedfrom"] = json!(d);
        }
        http.post(format!("{}/{key}", cfg.jooble_api_base.trim_end_matches('/'))).json(&body)
    }, |_, v| parse_jooble(v))
    .await
}

// ---------------------------------------------------------------- Careerjet

/// GET public.api.careerjet.net/search — `jobs[]` with url, title, company,
/// date (RFC 2822), description, locations, salary_min/max/currency_code.
pub fn parse_careerjet(locale: &str, v: &Value) -> Vec<RawJob> {
    let country = locale.rsplit('_').next().and_then(country_name);
    v["jobs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|j| {
                    let mut r = job(
                        "careerjet",
                        None,
                        &s(&j["title"])?,
                        s(&j["company"]),
                        s(&j["url"])?,
                        with_country(s(&j["locations"]), country),
                        s(&j["contracttype"]).or_else(|| s(&j["contractperiod"])),
                        parse_date(&j["date"]),
                        j["description"].as_str().unwrap_or(""),
                        Vec::new(),
                    );
                    r.source_ref = r.details.url.clone().map(|u| format!("careerjet:{u}"));
                    let cur = s(&j["salary_currency_code"]);
                    set_salary(&mut r.details, num(&j["salary_min"]), num(&j["salary_max"]), cur.as_deref());
                    if r.details.salary.is_none() {
                        r.details.salary = s(&j["salary"]);
                    }
                    Some(r)
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn careerjet(
    http: &reqwest::Client,
    cfg: &ApiSourcesConfig,
    day: u32,
    pacer: &mut Pacer,
    errors: &mut Vec<String>,
) -> Vec<RawJob> {
    let affid = cfg.careerjet_affid.clone().unwrap_or_default();
    let mut combos = Vec::new();
    for q in &cfg.queries {
        for l in &cfg.careerjet_locales {
            combos.push((l.clone(), q.clone()));
        }
    }
    let combos = todays(&combos, pacer.budget(), day, 0);
    each("careerjet", &combos, pacer, errors, |(l, q)| {
        http.get(&cfg.careerjet_api_base).query(&[
            ("affid", affid.as_str()),
            ("keywords", q.as_str()),
            ("locale_code", l.as_str()),
            ("sort", "date"),
            ("pagesize", "50"),
            ("page", "1"),
            ("user_ip", cfg.careerjet_user_ip.as_str()),
            ("user_agent", USER_AGENT),
        ])
    }, |(l, _), v| parse_careerjet(l, v))
    .await
}

// ---------------------------------------------------------------- Arbeitnow

/// GET /api/job-board-api?page=N — `data[]` with slug, company_name, title,
/// description (HTML), remote, url, tags, job_types, location, created_at.
pub fn parse_arbeitnow(v: &Value) -> Vec<RawJob> {
    v["data"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|j| {
                    let types: Vec<String> = j["job_types"].as_array().map(|t| t.iter().filter_map(s).collect()).unwrap_or_default();
                    let mut r = job(
                        "arbeitnow",
                        s(&j["slug"]),
                        &s(&j["title"])?,
                        s(&j["company_name"]),
                        s(&j["url"])?,
                        s(&j["location"]),
                        (!types.is_empty()).then(|| types.join(", ")),
                        parse_date(&j["created_at"]),
                        j["description"].as_str().unwrap_or(""),
                        j["tags"].as_array().map(|t| t.iter().filter_map(s).collect()).unwrap_or_default(),
                    );
                    if j["remote"].as_bool() == Some(true) {
                        r.details.remote = Some("remote".into());
                    }
                    Some(r)
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------- The Muse

/// GET /api/public/jobs?page=N — `results[]` with name, contents (HTML),
/// publication_date, locations[].name, levels, categories, company.name,
/// refs.landing_page.
pub fn parse_themuse(v: &Value) -> Vec<RawJob> {
    let names = |v: &Value| -> Vec<String> {
        v.as_array().map(|a| a.iter().filter_map(|x| s(&x["name"])).collect()).unwrap_or_default()
    };
    v["results"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|j| {
                    let places = names(&j["locations"]);
                    let mut tags = names(&j["categories"]);
                    tags.extend(names(&j["levels"]));
                    let mut r = job(
                        "themuse",
                        j["id"].as_i64().map(|i| i.to_string()),
                        &s(&j["name"])?,
                        s(&j["company"]["name"]),
                        s(&j["refs"]["landing_page"])?,
                        (!places.is_empty()).then(|| places.join("; ")),
                        s(&j["type"]).filter(|t| t != "external"),
                        parse_date(&j["publication_date"]),
                        j["contents"].as_str().unwrap_or(""),
                        tags,
                    );
                    if places.iter().any(|p| p.to_lowercase().contains("remote")) {
                        r.details.remote = Some("remote".into());
                    }
                    Some(r)
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn themuse(http: &reqwest::Client, cfg: &ApiSourcesConfig, pacer: &mut Pacer, errors: &mut Vec<String>) -> Vec<RawJob> {
    let pages: Vec<usize> = (1..=cfg.themuse_pages).collect();
    each("themuse", &pages, pacer, errors, |p| {
        let mut q: Vec<(&str, String)> = vec![
            ("page", p.to_string()),
            ("descending", "true".into()),
            ("category", "Software Engineering".into()),
            ("category", "Data Science".into()),
            ("level", "Senior Level".into()),
            ("level", "Management".into()),
            ("location", "Flexible / Remote".into()),
        ];
        if let Some(k) = &cfg.themuse_api_key {
            q.push(("api_key", k.clone()));
        }
        http.get(&cfg.themuse_api_base).query(&q)
    }, |_, v| parse_themuse(v))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todays_window_moves_and_stays_in_budget() {
        let all: Vec<u32> = (0..10).collect();
        assert_eq!(todays(&all, 20, 3, 0).len(), 10, "everything fits");
        let a = todays(&all, 4, 1, 0);
        let b = todays(&all, 4, 1, 1);
        assert_eq!(a.len(), 4);
        assert!(a.iter().all(|x| !b.contains(x)), "two sources sharing a list get different windows");
        assert_ne!(todays(&all, 4, 2, 0), a, "the window moves each day");
        assert!(todays(&all, 0, 1, 0).is_empty());
    }

    #[tokio::test]
    async fn pacer_enforces_budget() {
        let mut p = Pacer::new(Duration::ZERO, 2);
        assert!(p.turn().await);
        assert!(p.turn().await);
        assert!(!p.turn().await);
        assert_eq!(p.used(), 2);
    }

    #[test]
    fn country_names_and_currencies() {
        assert_eq!(country_name("IN"), Some("India"));
        assert_eq!(currency("gb"), Some("GBP"));
        assert_eq!(with_country(Some("Bengaluru".into()), Some("India")).as_deref(), Some("Bengaluru, India"));
        assert_eq!(with_country(Some("Pune, India".into()), Some("India")).as_deref(), Some("Pune, India"));
    }
}
