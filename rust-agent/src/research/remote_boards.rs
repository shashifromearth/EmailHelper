//! Remote-job boards with public APIs/feeds: Remotive, RemoteOK, Himalayas,
//! Jobicy and We Work Remotely. Each gives a posting date and the countries
//! applicants may be in, which the research filters need. They are read once
//! a day with an identifying User-Agent, a general list plus worldwide
//! searches for the tech stack (at most `MAX_REQUESTS_PER_BOARD` requests a
//! board); their pages link back to the source.

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use serde_json::Value;

use crate::db::jobs::JobDetails;
use crate::error::{AppError, AppResult};
use crate::jobs::extract::{self, html_to_text};
use crate::research::sources::RawJob;

pub const USER_AGENT: &str = "EmailHelper-research/1.0 (personal job search; a few requests per board per day)";

/// Board names accepted in RESEARCH_REMOTE_BOARDS.
pub const BOARDS: &[&str] = &["remotive", "remoteok", "himalayas", "jobicy", "weworkremotely"];

fn s(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|x| !x.is_empty()).map(str::to_string)
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(s).collect())
        .unwrap_or_default()
}

/// RFC 3339, "2026-10-17T16:04:29[.0000000]" (UTC), RFC 2822, unix
/// seconds, or a bare date ("2026-10-17", "October 17, 2026", "Oct 17, 2026").
pub fn parse_date(v: &Value) -> Option<DateTime<Utc>> {
    if let Some(n) = v.as_i64() {
        let secs = if n > 100_000_000_000 { n / 1000 } else { n };
        return Utc.timestamp_opt(secs, 0).single();
    }
    let t = v.as_str()?.trim();
    DateTime::parse_from_rfc3339(t)
        .map(|d| d.with_timezone(&Utc))
        .or_else(|_| DateTime::parse_from_rfc2822(t).map(|d| d.with_timezone(&Utc)))
        .ok()
        .or_else(|| {
            ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"]
                .iter()
                .find_map(|f| NaiveDateTime::parse_from_str(t, f).ok())
                .map(|n| Utc.from_utc_datetime(&n))
        })
        .or_else(|| {
            ["%Y-%m-%d", "%B %d, %Y", "%b %d, %Y"]
                .iter()
                .find_map(|f| chrono::NaiveDate::parse_from_str(t, f).ok())
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|n| Utc.from_utc_datetime(&n))
        })
}

#[allow(clippy::too_many_arguments)]
fn job(
    source: &str,
    id: Option<String>,
    title: String,
    company: Option<String>,
    url: String,
    region: Option<String>,
    employment_type: Option<String>,
    posted_at: Option<DateTime<Utc>>,
    description_html: &str,
    tags: Vec<String>,
) -> RawJob {
    let description = html_to_text(description_html);
    let mut d = JobDetails {
        title,
        company,
        url: Some(url),
        location: region.clone(),
        remote: Some("remote".into()),
        description: Some(description.chars().take(20_000).collect()),
        skills: tags,
        ..Default::default()
    };
    extract::enrich_from_text(&mut d, &description, &[]);
    d.remote = Some("remote".into());
    RawJob {
        source: source.into(),
        source_ref: id.map(|i| format!("{source}:{i}")),
        details: d,
        posted_at,
        region,
        employment_type,
    }
}

pub fn parse_remotive(v: &Value) -> Vec<RawJob> {
    v["jobs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|j| {
                    Some(job(
                        "remotive",
                        j["id"].as_i64().map(|i| i.to_string()),
                        s(&j["title"])?,
                        s(&j["company_name"]),
                        s(&j["url"])?,
                        s(&j["candidate_required_location"]),
                        s(&j["job_type"]),
                        parse_date(&j["publication_date"]),
                        j["description"].as_str().unwrap_or(""),
                        strings(&j["tags"]),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_remoteok(v: &Value) -> Vec<RawJob> {
    v.as_array()
        .map(|a| {
            a.iter()
                // the first element is the API's legal notice
                .filter(|j| j.get("position").is_some())
                .filter_map(|j| {
                    Some(job(
                        "remoteok",
                        s(&j["id"]).or_else(|| j["id"].as_i64().map(|i| i.to_string())),
                        s(&j["position"])?,
                        s(&j["company"]),
                        s(&j["url"]).or_else(|| s(&j["apply_url"]))?,
                        s(&j["location"]),
                        None,
                        parse_date(&j["date"]).or_else(|| parse_date(&j["epoch"])),
                        j["description"].as_str().unwrap_or(""),
                        strings(&j["tags"]),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_himalayas(v: &Value) -> Vec<RawJob> {
    v["jobs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|j| {
                    let restrictions = strings(&j["locationRestrictions"]);
                    let region = if restrictions.is_empty() {
                        Some("Worldwide".to_string())
                    } else {
                        Some(restrictions.join(", "))
                    };
                    let mut tags = strings(&j["categories"]);
                    tags.extend(strings(&j["parentCategories"]));
                    Some(job(
                        "himalayas",
                        s(&j["guid"]),
                        s(&j["title"])?,
                        s(&j["companyName"]),
                        s(&j["applicationLink"])?,
                        region,
                        s(&j["employmentType"]),
                        parse_date(&j["pubDate"]),
                        j["description"].as_str().or(j["excerpt"].as_str()).unwrap_or(""),
                        tags,
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_jobicy(v: &Value) -> Vec<RawJob> {
    v["jobs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|j| {
                    let types = strings(&j["jobType"]);
                    Some(job(
                        "jobicy",
                        j["id"].as_i64().map(|i| i.to_string()),
                        s(&j["jobTitle"])?,
                        s(&j["companyName"]),
                        s(&j["url"])?,
                        s(&j["jobGeo"]),
                        (!types.is_empty()).then(|| types.join(", ")),
                        parse_date(&j["pubDate"]),
                        j["jobDescription"].as_str().or(j["jobExcerpt"].as_str()).unwrap_or(""),
                        strings(&j["jobIndustry"]),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// We Work Remotely RSS: `<title>Company: Role</title>`, `<region>`, `<pubDate>`.
pub fn parse_wwr(xml: &str) -> AppResult<Vec<RawJob>> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| AppError::other(format!("invalid WWR feed: {e}")))?;
    let mut out = Vec::new();
    for item in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "item") {
        let child = |tag: &str| {
            item.children()
                .find(|c| c.is_element() && c.tag_name().name() == tag)
                .and_then(|c| c.text())
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
        };
        let Some(raw_title) = child("title") else { continue };
        let Some(link) = child("link") else { continue };
        let (company, title) = match raw_title.split_once(": ") {
            Some((c, t)) => (Some(c.trim().to_string()), t.trim().to_string()),
            None => (None, raw_title.clone()),
        };
        let posted = child("pubDate").and_then(|d| parse_date(&Value::String(d)));
        let category = child("category").map(|c| vec![c]).unwrap_or_default();
        out.push(job(
            "weworkremotely",
            child("guid"),
            title,
            company,
            link,
            child("region"),
            child("type"),
            posted,
            &child("description").unwrap_or_default(),
            category,
        ));
    }
    Ok(out)
}

/// Most requests made to one board in a run, its general lists included.
pub const MAX_REQUESTS_PER_BOARD: usize = 9;

/// The default RESEARCH_TECH_STACK.
pub const DEFAULT_TECH_STACK: &[&str] = &["python", "c#", ".net", "java", "rust", "go", "genai", "agentic ai"];

/// Default endpoint (overridable for tests via `RESEARCH_<BOARD>_URL`).
pub fn default_url(board: &str) -> &'static str {
    default_urls(board)[0]
}

/// A board's general recent lists (no search), worldwide.
fn general_urls(board: &str) -> &'static [&'static str] {
    match board {
        "remotive" => &["https://remotive.com/api/remote-jobs?category=software-dev"],
        "remoteok" => &["https://remoteok.com/api"],
        "himalayas" => &[
            "https://himalayas.app/jobs/api?limit=20",
            "https://himalayas.app/jobs/api?limit=20&offset=20",
        ],
        "jobicy" => &["https://jobicy.com/api/v2/remote-jobs?count=100"],
        _ => &[
            "https://weworkremotely.com/categories/remote-programming-jobs.rss",
            "https://weworkremotely.com/categories/remote-back-end-programming-jobs.rss",
            "https://weworkremotely.com/categories/remote-full-stack-programming-jobs.rss",
        ],
    }
}

/// The word a board is searched with for a stack term. C# and .NET share
/// one search, as do GenAI and agentic AI on RemoteOK's tags.
fn board_term(board: &str, term: &str) -> String {
    let t = term.trim().to_lowercase();
    match t.as_str() {
        "c#" | "csharp" | ".net" | "dotnet" | "asp.net" if board == "remoteok" => "dotnet".into(),
        "c#" | "csharp" | ".net" | "dotnet" | "asp.net" => ".net".into(),
        "go" | "golang" => "golang".into(),
        "genai" | "gen ai" | "generative ai" | "llm" if board == "himalayas" => "generative ai".into(),
        "genai" | "gen ai" | "generative ai" | "llm" => "ai".into(),
        "agentic ai" | "agentic" | "ai agents" if board == "remoteok" => "ai".into(),
        "agentic ai" | "agentic" | "ai agents" => "agentic".into(),
        _ => t,
    }
}

/// The requests made to a board in a run: its general recent lists plus one
/// worldwide search per term of the tech stack (RESEARCH_TECH_STACK), at most
/// [`MAX_REQUESTS_PER_BOARD`]. We Work Remotely has no search: its feeds only.
pub fn search_urls(board: &str, stack: &[String]) -> Vec<String> {
    let mut out: Vec<String> = general_urls(board).iter().map(|u| u.to_string()).collect();
    let template = match board {
        "remotive" => "https://remotive.com/api/remote-jobs?search=",
        "remoteok" => "https://remoteok.com/api?tags=",
        "himalayas" => "https://himalayas.app/jobs/api/search?q=",
        "jobicy" => "https://jobicy.com/api/v2/remote-jobs?count=50&tag=",
        _ => return out,
    };
    for term in stack {
        let word = board_term(board, term);
        if word.is_empty() {
            continue;
        }
        let url = format!("{template}{}", url::form_urlencoded::byte_serialize(word.as_bytes()).collect::<String>());
        if !out.contains(&url) {
            out.push(url);
        }
    }
    out.truncate(MAX_REQUESTS_PER_BOARD);
    out
}

/// [`search_urls`] for the default tech stack ([`DEFAULT_TECH_STACK`]).
pub fn default_urls(board: &str) -> &'static [&'static str] {
    match board {
        "remotive" => &[
            "https://remotive.com/api/remote-jobs?category=software-dev",
            "https://remotive.com/api/remote-jobs?search=python",
            "https://remotive.com/api/remote-jobs?search=.net",
            "https://remotive.com/api/remote-jobs?search=java",
            "https://remotive.com/api/remote-jobs?search=rust",
            "https://remotive.com/api/remote-jobs?search=golang",
            "https://remotive.com/api/remote-jobs?search=ai",
            "https://remotive.com/api/remote-jobs?search=agentic",
        ],
        "remoteok" => &[
            "https://remoteok.com/api",
            "https://remoteok.com/api?tags=python",
            "https://remoteok.com/api?tags=dotnet",
            "https://remoteok.com/api?tags=java",
            "https://remoteok.com/api?tags=rust",
            "https://remoteok.com/api?tags=golang",
            "https://remoteok.com/api?tags=ai",
        ],
        "himalayas" => &[
            "https://himalayas.app/jobs/api?limit=20",
            "https://himalayas.app/jobs/api?limit=20&offset=20",
            "https://himalayas.app/jobs/api/search?q=python",
            "https://himalayas.app/jobs/api/search?q=.net",
            "https://himalayas.app/jobs/api/search?q=java",
            "https://himalayas.app/jobs/api/search?q=rust",
            "https://himalayas.app/jobs/api/search?q=golang",
            "https://himalayas.app/jobs/api/search?q=generative+ai",
            "https://himalayas.app/jobs/api/search?q=agentic",
        ],
        "jobicy" => &[
            "https://jobicy.com/api/v2/remote-jobs?count=100",
            "https://jobicy.com/api/v2/remote-jobs?count=50&tag=python",
            "https://jobicy.com/api/v2/remote-jobs?count=50&tag=.net",
            "https://jobicy.com/api/v2/remote-jobs?count=50&tag=java",
            "https://jobicy.com/api/v2/remote-jobs?count=50&tag=rust",
            "https://jobicy.com/api/v2/remote-jobs?count=50&tag=golang",
            "https://jobicy.com/api/v2/remote-jobs?count=50&tag=ai",
            "https://jobicy.com/api/v2/remote-jobs?count=50&tag=agentic",
        ],
        _ => general_urls(board),
    }
}

pub async fn fetch(http: &reqwest::Client, board: &str, url: &str) -> AppResult<Vec<RawJob>> {
    let resp = http
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(std::time::Duration::from_secs(40))
        .send()
        .await?
        .error_for_status()?;
    if board == "weworkremotely" {
        return parse_wwr(&resp.text().await?);
    }
    let v: Value = resp.json().await?;
    Ok(match board {
        "remotive" => parse_remotive(&v),
        "remoteok" => parse_remoteok(&v),
        "himalayas" => parse_himalayas(&v),
        _ => parse_jobicy(&v),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dates_in_every_board_format() {
        let want = Utc.with_ymd_and_hms(2026, 10, 17, 16, 4, 29).unwrap();
        assert_eq!(parse_date(&json!("2026-10-17T16:04:29")), Some(want));
        assert_eq!(parse_date(&json!("2026-10-17T16:04:29+00:00")), Some(want));
        assert_eq!(parse_date(&json!(want.timestamp())), Some(want));
        assert_eq!(parse_date(&json!("Sat, 17 Oct 2026 16:04:29 +0000")), Some(want));
        assert_eq!(parse_date(&json!("yesterday")), None);
    }

    fn stack(terms: &[&str]) -> Vec<String> {
        terms.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn searches_follow_the_stack_worldwide() {
        for b in BOARDS {
            let want: Vec<String> = default_urls(b).iter().map(|u| u.to_string()).collect();
            assert_eq!(search_urls(b, &stack(DEFAULT_TECH_STACK)), want, "default_urls({b}) matches the default stack");
            assert!(want.len() <= MAX_REQUESTS_PER_BOARD, "{b}");
            assert!(want.iter().all(|u| !u.contains("country=") && !u.contains("geo=")), "no regional limits: {b}");
        }
        for want in ["search=rust", "search=golang", "search=.net"] {
            assert!(default_urls("remotive").iter().any(|u| u.ends_with(want)), "{want}");
        }
        let custom = search_urls("himalayas", &stack(&["Kotlin", "C#", ".NET", "machine learning"]));
        assert_eq!(
            custom[2..],
            [
                "https://himalayas.app/jobs/api/search?q=kotlin",
                "https://himalayas.app/jobs/api/search?q=.net",
                "https://himalayas.app/jobs/api/search?q=machine+learning",
            ],
            "C# and .NET share one search"
        );
        assert_eq!(search_urls("weworkremotely", &stack(&["rust"])).len(), 3, "feeds only");
        assert_eq!(search_urls("remotive", &[]), ["https://remotive.com/api/remote-jobs?category=software-dev"]);
        let many: Vec<String> = (0..30).map(|i| format!("term{i}")).collect();
        assert_eq!(search_urls("jobicy", &many).len(), MAX_REQUESTS_PER_BOARD);
    }

    #[test]
    fn boards_parse_region_type_and_date() {
        let r = parse_remotive(&json!({"jobs": [{"id": 1, "title": "Python Developer", "company_name": "Askable",
            "candidate_required_location": "Worldwide", "publication_date": "2026-10-17T16:04:29", "job_type": "contract",
            "url": "https://remotive.com/remote-jobs/x-1", "description": "<p>Python and LLMs</p>", "tags": ["python"]}]}));
        assert_eq!(r[0].region.as_deref(), Some("Worldwide"));
        assert_eq!(r[0].employment_type.as_deref(), Some("contract"));
        assert!(r[0].posted_at.is_some());

        let h = parse_himalayas(&json!({"jobs": [{"guid": "g", "title": "Java Engineer", "companyName": "Acme",
            "locationRestrictions": [], "pubDate": 1792430969, "employmentType": "Contractor",
            "applicationLink": "https://himalayas.app/companies/acme/jobs/java", "description": "Java"}]}));
        assert_eq!(h[0].region.as_deref(), Some("Worldwide"), "no restriction means anywhere");

        let o = parse_remoteok(&json!([{"legal": "notice"}, {"id": "9", "position": "C# Developer", "company": "Rain",
            "location": "", "date": "2026-10-18T18:00:01+00:00", "url": "https://remoteOK.com/remote-jobs/9", "tags": ["c#"]}]));
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].details.title, "C# Developer");

        let w = parse_wwr(r#"<rss><channel><item><title>Stripe: Abuse Research Engineer</title>
            <region>Anywhere in the World</region><pubDate>Sat, 17 Oct 2026 16:04:29 +0000</pubDate>
            <link>https://weworkremotely.com/remote-jobs/stripe-1</link><description>&lt;p&gt;Python&lt;/p&gt;</description></item></channel></rss>"#).unwrap();
        assert_eq!(w[0].details.company.as_deref(), Some("Stripe"));
        assert_eq!(w[0].details.title, "Abuse Research Engineer");
        assert_eq!(w[0].region.as_deref(), Some("Anywhere in the World"));
    }
}
