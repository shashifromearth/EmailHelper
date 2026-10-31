//! Job postings on LinkedIn, Glassdoor, Naukri, Wellfound, Indeed, Instahyre,
//! Hirist, foundit and iimjobs, found through web-search APIs (Google
//! Programmable Search JSON API, Brave Search API) with queries like
//! `site:linkedin.com/jobs/view "architect" python`. Those sites and Google's
//! result pages are never fetched directly: only the search APIs are called,
//! and a hit becomes a job with the platform link, applied to later through
//! the browser extension (never by the agent).

use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use regex::Regex;
use serde_json::Value;

use crate::config::ApiSourcesConfig;
use crate::db::jobs::JobDetails;
use crate::jobs::extract::{self, html_to_text};
use crate::research::job_apis::{each, todays, Pacer};
use crate::research::remote_boards::parse_date;
use crate::research::sources::RawJob;

/// Site names accepted in RESEARCH_SEARCH_SITES.
pub const SITES: &[&str] = &[
    "linkedin", "glassdoor", "naukri", "wellfound", "indeed", "instahyre", "hirist", "foundit", "iimjobs",
    // remote / freelance / contract portals
    "upwork", "freelancer", "arc", "dice", "remoteco",
];

/// Search providers whose hits are platform links (applied through the extension).
pub const PROVIDERS: &[&str] = &["google_cse", "brave"];

struct Site {
    name: &'static str,
    /// The `site:` operator used in queries.
    query: &'static str,
    domains: &'static [&'static str],
    /// A posting's URL path contains one of these.
    paths: &'static [&'static str],
}

const SITE_TABLE: &[Site] = &[
    Site { name: "linkedin", query: "site:linkedin.com/jobs/view", domains: &["linkedin.com"], paths: &["/jobs/view/"] },
    Site { name: "glassdoor", query: "site:glassdoor.com/job-listing", domains: &["glassdoor.com", "glassdoor.co.in"], paths: &["/job-listing/"] },
    Site { name: "naukri", query: "site:naukri.com/job-listings", domains: &["naukri.com"], paths: &["/job-listings"] },
    Site { name: "wellfound", query: "site:wellfound.com/jobs", domains: &["wellfound.com"], paths: &["/jobs/"] },
    Site { name: "indeed", query: "site:indeed.com/viewjob", domains: &["indeed.com", "indeed.co.in"], paths: &["/viewjob", "/rc/clk"] },
    Site { name: "instahyre", query: "site:instahyre.com/job-", domains: &["instahyre.com"], paths: &["/job-"] },
    Site { name: "hirist", query: "site:hirist.tech/j", domains: &["hirist.tech", "hirist.com"], paths: &["/j/"] },
    Site { name: "foundit", query: "site:foundit.in/job", domains: &["foundit.in"], paths: &["/job/"] },
    Site { name: "iimjobs", query: "site:iimjobs.com/j", domains: &["iimjobs.com"], paths: &["/j/"] },
    Site { name: "upwork", query: "site:upwork.com/freelance-jobs/apply", domains: &["upwork.com"], paths: &["/freelance-jobs/apply/"] },
    Site { name: "freelancer", query: "site:freelancer.com/projects", domains: &["freelancer.com", "freelancer.in"], paths: &["/projects/"] },
    Site { name: "arc", query: "site:arc.dev/remote-jobs", domains: &["arc.dev"], paths: &["/remote-jobs/"] },
    Site { name: "dice", query: "site:dice.com/job-detail", domains: &["dice.com"], paths: &["/job-detail/"] },
    Site { name: "remoteco", query: "site:remote.co/job", domains: &["remote.co"], paths: &["/job/"] },
];

/// Which job platform a URL is a posting on, if any.
pub fn platform(url: &str) -> Option<&'static str> {
    let u = url::Url::parse(url).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    let path = u.path().to_ascii_lowercase();
    SITE_TABLE
        .iter()
        .find(|s| {
            s.domains.iter().any(|d| host == *d || host.ends_with(&format!(".{d}")))
                && s.paths.iter().any(|p| path.contains(p))
        })
        .map(|s| s.name)
}

/// Every query for the configured roles, stacks and sites. Role and stack
/// pairs are the outer loop so each day's window spans several sites.
pub fn queries(cfg: &ApiSourcesConfig) -> Vec<String> {
    let quote = |t: &str| if t.contains(' ') { format!("\"{t}\"") } else { t.to_string() };
    let sites: Vec<&Site> = cfg
        .search_sites
        .iter()
        .filter_map(|n| SITE_TABLE.iter().find(|s| s.name.eq_ignore_ascii_case(n.trim())))
        .collect();
    let mut out = Vec::new();
    for role in &cfg.search_roles {
        for stack in &cfg.search_stacks {
            for s in &sites {
                out.push(format!("{} \"{}\" {}", s.query, role.trim(), quote(stack.trim())));
            }
        }
    }
    out
}

/// Strip " | LinkedIn", " - Naukri.com" and similar site names from a title.
fn strip_site_suffix(t: &str) -> String {
    const NAMES: &[&str] = &[
        "linkedin", "naukri", "glassdoor", "indeed", "wellfound", "instahyre", "hirist", "foundit", "iimjobs", "monster",
        "upwork", "freelancer.com", "arc.dev", "dice", "remote.co",
    ];
    let mut t = t.trim().trim_end_matches(['.', '…']).trim().to_string();
    loop {
        let cut = [" | ", " - ", " – ", " — "]
            .iter()
            .filter_map(|sep| t.rfind(sep).map(|i| (i, sep.len())))
            .max_by_key(|(i, _)| *i);
        match cut {
            Some((i, n)) if NAMES.iter().any(|w| t[i + n..].to_lowercase().contains(w)) => t.truncate(i),
            _ => return t.trim().to_string(),
        }
    }
}

/// (title, company, location) from a search hit's title, e.g.
/// "Acme hiring Senior Python Developer in Bengaluru, Karnataka, India | LinkedIn",
/// "Senior Python Developer - Acme - Bengaluru | Naukri.com",
/// "Python Architect at Acme | Wellfound".
pub fn parse_title(raw: &str) -> (String, Option<String>, Option<String>) {
    let t = strip_site_suffix(&html_to_text(raw));
    let lower = t.to_lowercase();
    // "X job in Pune"; a bare "X in Y" only when Y looks like a place
    // ("Engineer in Machine Learning" is not one), except after "hiring".
    let split_place = |rest: &str, any_in: bool| -> (String, Option<String>) {
        let rl = rest.to_lowercase();
        for marker in [" jobs in ", " job in ", " in "] {
            if let Some(i) = rl.rfind(marker) {
                let place = rest[i + marker.len()..].trim();
                let placeish = any_in
                    || marker != " in "
                    || place.contains(',')
                    || place.to_lowercase().contains("remote")
                    || !crate::knowledge::profile::countries_in(place).is_empty();
                if !place.is_empty() && i > 0 && placeish {
                    return (rest[..i].trim().to_string(), Some(place.to_string()));
                }
            }
        }
        (rest.trim().to_string(), None)
    };
    if let Some(i) = lower.find(" hiring ") {
        let (title, place) = split_place(&t[i + 8..], true);
        return (title, Some(t[..i].trim().to_string()).filter(|c| !c.is_empty()), place);
    }
    if lower.contains(" at ") {
        let (title, company) = extract::split_title(&t);
        let (title, place) = split_place(&title, false);
        return (title, company, place);
    }
    let norm = t.replace(" | ", " - ").replace(" – ", " - ");
    let parts: Vec<&str> = norm.split(" - ").map(str::trim).filter(|p| !p.is_empty()).collect();
    let years = |p: &str| {
        let p = p.to_lowercase();
        p.contains("years") || p.contains("yrs") || p.contains("experience")
    };
    match parts.as_slice() {
        [title] => {
            let (title, place) = split_place(title, false);
            (title, None, place)
        }
        [title, company] => (title.to_string(), Some(company.to_string()), None),
        [title, company, rest @ ..] => (
            title.to_string(),
            Some(company.to_string()),
            rest.iter().rev().find(|p| !years(p)).map(|p| p.to_string()),
        ),
        [] => (t.clone(), None, None),
    }
}

/// The posting's link without tracking parameters. LinkedIn's search-result
/// form `/jobs/view/<slug>-<id>` becomes `/jobs/view/<id>/`.
pub fn posting_url(site: &str, url: &str) -> String {
    if site == "linkedin" {
        static R: OnceLock<Regex> = OnceLock::new();
        let re = R.get_or_init(|| Regex::new(r"/jobs/view/(?:[^/?#]*-)?(\d{6,})").expect("valid regex"));
        if let Some(c) = re.captures(url) {
            return format!("https://www.linkedin.com/jobs/view/{}/", &c[1]);
        }
    }
    crate::jobs::detect::classify_url(url)
        .map(|l| l.url)
        .unwrap_or_else(|| url.to_string())
}

/// "3 days ago", "5 hours ago", "yesterday" anywhere in `text`.
pub fn relative_age(text: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| {
        Regex::new(r"(?i)\b(\d{1,3})\+?\s*(minute|min|hour|hr|day|week|month)s?\s+ago\b").expect("valid regex")
    });
    if let Some(c) = re.captures(text) {
        let n: i64 = c[1].parse().ok()?;
        let unit = c[2].to_lowercase();
        let d = match unit.as_str() {
            "minute" | "min" => chrono::Duration::minutes(n),
            "hour" | "hr" => chrono::Duration::hours(n),
            "day" => chrono::Duration::days(n),
            "week" => chrono::Duration::weeks(n),
            _ => chrono::Duration::days(30 * n),
        };
        return Some(now - d);
    }
    let l = text.to_lowercase();
    if l.contains("just now") || l.contains("today") {
        return Some(now);
    }
    l.contains("yesterday").then(|| now - chrono::Duration::days(1))
}

/// A search hit on a job platform, as a job. The posting date is the page
/// date the API reports, an age in the snippet ("2 days ago"), or else the
/// fetch time: the query asked the API only for pages from the freshness window.
pub fn job_from_hit(
    provider: &str,
    title: &str,
    url: &str,
    snippet: &str,
    page_date: Option<&Value>,
    now: DateTime<Utc>,
) -> Option<RawJob> {
    let site = platform(url)?;
    let link = posting_url(site, url);
    let (title, company, place) = parse_title(title);
    if title.is_empty() {
        return None;
    }
    let desc = html_to_text(snippet);
    let mut d = JobDetails {
        title,
        company,
        url: Some(link.clone()),
        location: place.clone(),
        description: Some(desc.clone()),
        ..Default::default()
    };
    let text = format!("{} {desc}", d.title);
    extract::enrich_from_text(&mut d, &text, &[]);
    let posted_at = page_date
        .and_then(parse_date)
        .or_else(|| page_date.and_then(Value::as_str).and_then(|a| relative_age(a, now)))
        .or_else(|| relative_age(&desc, now))
        .or(Some(now));
    Some(RawJob {
        source: format!("{provider}:{site}"),
        source_ref: Some(link),
        details: d,
        posted_at,
        region: place,
        employment_type: None,
    })
}

/// Google: `items[]` with title, link, snippet.
pub fn parse_google(v: &Value, now: DateTime<Utc>) -> Vec<RawJob> {
    v["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| {
                    job_from_hit(
                        "google_cse",
                        i["title"].as_str()?,
                        i["link"].as_str()?,
                        i["snippet"].as_str().unwrap_or(""),
                        None,
                        now,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Brave: `web.results[]` with title, url, description, page_age / age.
pub fn parse_brave(v: &Value, now: DateTime<Utc>) -> Vec<RawJob> {
    v["web"]["results"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    let date = if r["page_age"].is_string() { &r["page_age"] } else { &r["age"] };
                    job_from_hit(
                        "brave",
                        r["title"].as_str()?,
                        r["url"].as_str()?,
                        r["description"].as_str().unwrap_or(""),
                        Some(date),
                        now,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

pub async fn google(
    http: &reqwest::Client,
    cfg: &ApiSourcesConfig,
    days: Option<u32>,
    day: u32,
    pacer: &mut Pacer,
    errors: &mut Vec<String>,
    now: DateTime<Utc>,
) -> Vec<RawJob> {
    let qs = todays(&queries(cfg), pacer.budget(), day, 0);
    let (key, cx) = (cfg.google_cse_key.clone().unwrap_or_default(), cfg.google_cse_cx.clone().unwrap_or_default());
    each("google_cse", &qs, pacer, errors, |q| {
        let mut p = vec![("key", key.clone()), ("cx", cx.clone()), ("q", q.clone()), ("num", "10".to_string())];
        if let Some(d) = days {
            p.push(("dateRestrict", format!("d{d}")));
        }
        http.get(&cfg.google_cse_api_base).query(&p)
    }, |_, v| parse_google(v, now))
    .await
}

pub async fn brave(
    http: &reqwest::Client,
    cfg: &ApiSourcesConfig,
    days: Option<u32>,
    day: u32,
    pacer: &mut Pacer,
    errors: &mut Vec<String>,
    now: DateTime<Utc>,
) -> Vec<RawJob> {
    // Google (slot 0) and Brave (slot 1) take different windows of one list.
    let qs = todays(&queries(cfg), pacer.budget(), day, 1);
    let key = cfg.brave_search_key.clone().unwrap_or_default();
    let freshness = days.map(|d| match d {
        0..=1 => "pd",
        2..=7 => "pw",
        8..=31 => "pm",
        _ => "py",
    });
    each("brave", &qs, pacer, errors, |q| {
        let mut p = vec![("q", q.clone()), ("count", "20".to_string())];
        if let Some(f) = freshness {
            p.push(("freshness", f.to_string()));
        }
        http.get(&cfg.brave_search_api_base)
            .header(reqwest::header::ACCEPT, "application/json")
            .header("X-Subscription-Token", key.as_str())
            .query(&p)
    }, |_, v| parse_brave(v, now))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn platforms_are_recognised() {
        assert_eq!(platform("https://in.linkedin.com/jobs/view/senior-python-developer-at-acme-4012345678"), Some("linkedin"));
        assert_eq!(platform("https://www.naukri.com/job-listings-python-architect-acme-bengaluru-8-to-12-years-301024001234"), Some("naukri"));
        assert_eq!(platform("https://www.glassdoor.co.in/job-listing/python-dev-acme-JV_IC123.htm"), Some("glassdoor"));
        assert_eq!(platform("https://in.indeed.com/viewjob?jk=abc123"), Some("indeed"));
        assert_eq!(platform("https://www.iimjobs.com/j/genai-architect-1234567.html"), Some("iimjobs"));
        assert_eq!(platform("https://www.linkedin.com/company/acme"), None, "not a posting");
        assert_eq!(platform("https://example.com/jobs/view/1"), None);
    }

    #[test]
    fn titles_from_each_platform() {
        assert_eq!(
            parse_title("Acme hiring Senior Python Developer in Bengaluru, Karnataka, India | LinkedIn"),
            ("Senior Python Developer".into(), Some("Acme".into()), Some("Bengaluru, Karnataka, India".into()))
        );
        assert_eq!(
            parse_title("GenAI Architect - Initech - 10 to 15 years of experience - Pune - Naukri.com"),
            ("GenAI Architect".into(), Some("Initech".into()), Some("Pune".into()))
        );
        assert_eq!(
            parse_title("Rust Engineer at Globex | Wellfound"),
            ("Rust Engineer".into(), Some("Globex".into()), None)
        );
        assert_eq!(
            parse_title("Java Architect Job in Hyderabad | Glassdoor"),
            ("Java Architect".into(), None, Some("Hyderabad".into()))
        );
        assert_eq!(
            parse_title("Lead Engineer in Machine Learning").2,
            None,
            "not a place"
        );
    }

    #[test]
    fn ages_from_snippets() {
        let now = Utc::now();
        assert_eq!(relative_age("3 days ago ... Python", now), Some(now - chrono::Duration::days(3)));
        assert_eq!(relative_age("Posted 5 hours ago", now), Some(now - chrono::Duration::hours(5)));
        assert_eq!(relative_age("no date here", now), None);
    }

    #[test]
    fn queries_cover_roles_stacks_and_sites() {
        let cfg = crate::config::Config::from_lookup(|k| match k {
            "RESEARCH_SEARCH_ROLES" => Some("architect".into()),
            "RESEARCH_SEARCH_STACKS" => Some("python;generative ai".into()),
            "RESEARCH_SEARCH_SITES" => Some("linkedin;naukri;unknown".into()),
            _ => None,
        })
        .unwrap();
        let q = queries(&cfg.research.apis);
        assert_eq!(q.len(), 4);
        assert_eq!(q[0], "site:linkedin.com/jobs/view \"architect\" python");
        assert_eq!(q[3], "site:naukri.com/job-listings \"architect\" \"generative ai\"");
    }

    #[test]
    fn hits_become_platform_jobs() {
        let now = Utc::now();
        let g = parse_google(&json!({"items": [
            {"title": "Acme hiring Lead Python Engineer in Bengaluru, Karnataka, India | LinkedIn",
             "link": "https://in.linkedin.com/jobs/view/lead-python-engineer-at-acme-4012345678?trk=x",
             "snippet": "2 days ago ... Hybrid. Python, FastAPI, LLMs."},
            {"title": "Python tips", "link": "https://example.com/blog", "snippet": ""}
        ]}), now);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].source, "google_cse:linkedin");
        assert_eq!(g[0].details.url.as_deref(), Some("https://www.linkedin.com/jobs/view/4012345678/"));
        assert_eq!(g[0].details.company.as_deref(), Some("Acme"));
        assert_eq!(g[0].posted_at, Some(now - chrono::Duration::days(2)));
        let b = parse_brave(&json!({"web": {"results": [
            {"title": "Senior .NET Developer - Globex - Remote | Naukri.com",
             "url": "https://www.naukri.com/job-listings-senior-net-developer-globex-301024001234",
             "description": "C#, ASP.NET Core. Remote.", "page_age": "2026-10-30T08:00:00"}
        ]}}), now);
        assert_eq!(b[0].source, "brave:naukri");
        assert_eq!(b[0].details.remote.as_deref(), Some("remote"));
        assert!(b[0].posted_at.unwrap().to_rfc3339().starts_with("2026-10-30T08:00:00"));
    }
}
