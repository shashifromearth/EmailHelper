//! Job sources for research: SearXNG, RSS/Atom feeds and the public job-board
//! APIs of Greenhouse, Lever and Ashby (no scraping needed for those).

use serde::Deserialize;
use serde_json::Value;

use crate::db::jobs::JobDetails;
use crate::error::{AppError, AppResult};
use crate::jobs::extract::{self, html_to_text};

#[derive(Debug, Clone, PartialEq)]
pub struct RawJob {
    pub source: String,
    pub source_ref: Option<String>,
    pub details: JobDetails,
    /// When the posting was published, if the source says.
    pub posted_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Where applicants may be ("Worldwide", "USA", "India, APAC").
    pub region: Option<String>,
    /// full_time / contract / freelance …, as the source labels it.
    pub employment_type: Option<String>,
}

impl RawJob {
    pub fn new(source: impl Into<String>, source_ref: Option<String>, details: JobDetails) -> Self {
        Self {
            source: source.into(),
            source_ref,
            details,
            posted_at: None,
            region: None,
            employment_type: None,
        }
    }
}

// ---------------------------------------------------------------- SearXNG

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SearchResult {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub engine: String,
}

#[derive(Deserialize)]
struct SearxResponse {
    #[serde(default)]
    results: Vec<SearchResult>,
}

/// `search_web()` against a local SearXNG instance (JSON format must be enabled).
pub async fn searxng_search(
    http: &reqwest::Client,
    base: &str,
    query: &str,
    limit: usize,
) -> AppResult<Vec<SearchResult>> {
    let resp = http
        .get(format!("{}/search", base.trim_end_matches('/')))
        .query(&[("q", query), ("format", "json"), ("safesearch", "1")])
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(AppError::other(format!(
            "searxng returned {} (is the json format enabled in settings.yml?)",
            resp.status()
        )));
    }
    let r: SearxResponse = resp.json().await?;
    Ok(r.results
        .into_iter()
        .filter(|r| !r.url.is_empty())
        .take(limit)
        .collect())
}

/// Turn a search hit into a job candidate when it points at a job posting.
pub fn job_from_search(r: &SearchResult) -> Option<RawJob> {
    let link = crate::jobs::detect::classify_url(&r.url)?;
    let (title, company) = extract::split_title(&r.title);
    let mut d = JobDetails {
        title,
        company,
        url: Some(link.url.clone()),
        description: Some(r.content.clone()),
        ..Default::default()
    };
    extract::enrich_from_text(&mut d, &r.content, &[]);
    Some(RawJob::new(format!("searxng:{}", link.ats), Some(r.url.clone()), d))
}

// ---------------------------------------------------------------- RSS / Atom

#[derive(Debug, Clone, PartialEq)]
pub struct FeedItem {
    pub title: String,
    pub link: String,
    pub description: String,
    /// pubDate / published / updated, as written in the feed.
    pub published: Option<String>,
}

pub fn parse_feed(xml: &str) -> AppResult<Vec<FeedItem>> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| AppError::other(format!("invalid feed XML: {e}")))?;
    let mut items = Vec::new();
    for node in doc.descendants().filter(|n| n.is_element()) {
        let name = node.tag_name().name();
        if name != "item" && name != "entry" {
            continue;
        }
        let child_text = |tag: &str| {
            node.children()
                .find(|c| c.is_element() && c.tag_name().name() == tag)
                .and_then(|c| c.text())
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let link = {
            let l = child_text("link");
            if !l.is_empty() {
                l
            } else {
                node.children()
                    .find(|c| c.is_element() && c.tag_name().name() == "link")
                    .and_then(|c| c.attribute("href"))
                    .unwrap_or("")
                    .to_string()
            }
        };
        let mut description = child_text("description");
        if description.is_empty() {
            description = child_text("summary");
        }
        if description.is_empty() {
            description = child_text("content");
        }
        let published = ["pubDate", "published", "updated"]
            .iter()
            .map(|t| child_text(t))
            .find(|t| !t.is_empty());
        items.push(FeedItem {
            title: child_text("title"),
            link,
            description: html_to_text(&description),
            published,
        });
    }
    Ok(items)
}

pub async fn fetch_feed(http: &reqwest::Client, url: &str) -> AppResult<Vec<RawJob>> {
    let xml = http
        .get(url)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(parse_feed(&xml)?
        .into_iter()
        .filter(|i| !i.link.is_empty() && !i.title.is_empty())
        .map(|i| {
            let (title, company) = extract::split_title(&i.title);
            let mut d = JobDetails {
                title,
                company,
                url: Some(i.link.clone()),
                description: Some(i.description.clone()),
                ..Default::default()
            };
            extract::enrich_from_text(&mut d, &i.description, &[]);
            let mut j = RawJob::new("rss", Some(url.to_string()), d);
            j.posted_at = i
                .published
                .as_deref()
                .and_then(|p| crate::research::remote_boards::parse_date(&Value::String(p.to_string())));
            j
        })
        .collect())
}

// ---------------------------------------------------------------- ATS boards

fn title_case(slug: &str) -> String {
    slug.split(['-', '_'])
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn parse_greenhouse(board: &str, v: &Value) -> Vec<RawJob> {
    v["jobs"]
        .as_array()
        .map(|jobs| {
            jobs.iter()
                .filter_map(|j| {
                    let title = j["title"].as_str()?.to_string();
                    let url = j["absolute_url"].as_str()?.to_string();
                    let desc = j["content"].as_str().map(html_to_text).unwrap_or_default();
                    let mut d = JobDetails {
                        title,
                        company: Some(
                            j["company_name"]
                                .as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| title_case(board)),
                        ),
                        url: Some(url),
                        location: j["location"]["name"].as_str().map(str::to_string),
                        description: Some(desc.chars().take(20_000).collect()),
                        ..Default::default()
                    };
                    let loc_text = format!("{} {}", d.location.clone().unwrap_or_default(), desc);
                    extract::enrich_from_text(&mut d, &loc_text, &[]);
                    let mut r = RawJob::new("greenhouse", j["id"].as_i64().map(|i| format!("{board}:{i}")), d);
                    r.posted_at = crate::research::remote_boards::parse_date(&j["first_published"])
                        .or_else(|| crate::research::remote_boards::parse_date(&j["updated_at"]));
                    r.region = r.details.location.clone();
                    Some(r)
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_lever(company: &str, v: &Value) -> Vec<RawJob> {
    v.as_array()
        .map(|jobs| {
            jobs.iter()
                .filter_map(|j| {
                    let title = j["text"].as_str()?.to_string();
                    let url = j["hostedUrl"].as_str()?.to_string();
                    let desc = j["descriptionPlain"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_default();
                    let mut d = JobDetails {
                        title,
                        company: Some(title_case(company)),
                        url: Some(url),
                        location: j["categories"]["location"].as_str().map(str::to_string),
                        remote: j["workplaceType"].as_str().map(|w| match w {
                            "remote" => "remote".to_string(),
                            "hybrid" => "hybrid".to_string(),
                            "onsite" | "on-site" => "onsite".to_string(),
                            _ => "unknown".to_string(),
                        }),
                        description: Some(desc.clone()),
                        ..Default::default()
                    };
                    if let Some(sr) = j.get("salaryRange").filter(|s| s.is_object()) {
                        d.salary_min = sr["min"].as_f64();
                        d.salary_max = sr["max"].as_f64();
                        d.salary_currency = sr["currency"].as_str().map(str::to_string);
                        if let (Some(a), Some(b)) = (d.salary_min, d.salary_max) {
                            d.salary = Some(format!(
                                "{} {a}–{b}",
                                d.salary_currency.clone().unwrap_or_default()
                            ));
                        }
                    }
                    extract::enrich_from_text(&mut d, &desc, &[]);
                    let mut r = RawJob::new("lever", j["id"].as_str().map(|i| format!("{company}:{i}")), d);
                    r.posted_at = crate::research::remote_boards::parse_date(&j["createdAt"]);
                    r.region = r.details.location.clone();
                    r.employment_type = j["categories"]["commitment"].as_str().map(str::to_string);
                    Some(r)
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_ashby(org: &str, v: &Value) -> Vec<RawJob> {
    v["jobs"]
        .as_array()
        .map(|jobs| {
            jobs.iter()
                .filter(|j| j["isListed"].as_bool().unwrap_or(true))
                .filter_map(|j| {
                    let title = j["title"].as_str()?.to_string();
                    let url = j["jobUrl"]
                        .as_str()
                        .or_else(|| j["applyUrl"].as_str())?
                        .to_string();
                    let desc = j["descriptionPlain"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_default();
                    let mut d = JobDetails {
                        title,
                        company: Some(title_case(org)),
                        url: Some(url),
                        location: j["location"].as_str().map(str::to_string),
                        remote: if j["isRemote"].as_bool() == Some(true) {
                            Some("remote".into())
                        } else {
                            None
                        },
                        salary: j["compensation"]["compensationTierSummary"]
                            .as_str()
                            .map(str::to_string),
                        description: Some(desc.clone()),
                        ..Default::default()
                    };
                    if let Some(sal) = d.salary.clone() {
                        let (lo, hi, cur) = extract::parse_salary_range(&sal);
                        d.salary_min = lo;
                        d.salary_max = hi;
                        d.salary_currency = cur;
                    }
                    extract::enrich_from_text(&mut d, &desc, &[]);
                    let mut r = RawJob::new("ashby", j["id"].as_str().map(|i| format!("{org}:{i}")), d);
                    r.posted_at = crate::research::remote_boards::parse_date(&j["publishedAt"]);
                    r.region = r.details.location.clone();
                    r.employment_type = j["employmentType"].as_str().map(str::to_string);
                    Some(r)
                })
                .collect()
        })
        .unwrap_or_default()
}

pub async fn fetch_json(http: &reqwest::Client, url: &str) -> AppResult<Value> {
    Ok(http
        .get(url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rss_and_atom() {
        let rss = r#"<?xml version="1.0"?><rss><channel><item><title>Rust Engineer at Globex</title>
<link>https://jobs.lever.co/globex/1</link><description>&lt;p&gt;Remote, 5+ years Rust&lt;/p&gt;</description></item></channel></rss>"#;
        let items = parse_feed(rss).unwrap();
        assert_eq!(items[0].title, "Rust Engineer at Globex");
        assert_eq!(items[0].description, "Remote, 5+ years Rust");
        let atom = r#"<feed xmlns="http://www.w3.org/2005/Atom"><entry><title>AI Architect - Initech</title>
<link href="https://initech.com/careers/1"/><summary>Hybrid</summary></entry></feed>"#;
        let items = parse_feed(atom).unwrap();
        assert_eq!(items[0].link, "https://initech.com/careers/1");
    }

    #[test]
    fn greenhouse_lever_ashby() {
        let gh = json!({"jobs":[{"id":1,"title":"Staff AI Engineer","absolute_url":"https://boards.greenhouse.io/acme/jobs/1",
            "location":{"name":"Remote - India"},"content":"&lt;p&gt;Python and Rust, 8+ years&lt;/p&gt;"}]});
        let j = parse_greenhouse("acme", &gh);
        assert_eq!(j[0].details.company.as_deref(), Some("Acme"));
        assert_eq!(j[0].details.remote.as_deref(), Some("remote"));
        assert!(j[0].details.skills.contains(&"Rust".to_string()));

        let lv = json!([{"id":"abc","text":"Lead AI Engineer","hostedUrl":"https://jobs.lever.co/globex/abc",
            "categories":{"location":"Bengaluru"},"workplaceType":"hybrid","descriptionPlain":"LLMs and RAG",
            "salaryRange":{"min":4000000,"max":6000000,"currency":"INR"}}]);
        let j = parse_lever("globex", &lv);
        assert_eq!(j[0].details.remote.as_deref(), Some("hybrid"));
        assert_eq!(j[0].details.salary_max, Some(6_000_000.0));

        let ab = json!({"jobs":[{"id":"x","title":"GenAI Architect","jobUrl":"https://jobs.ashbyhq.com/initech/x",
            "location":"Remote","isRemote":true,"descriptionPlain":"Design GenAI platforms",
            "compensation":{"compensationTierSummary":"$180K – $220K"}}]});
        let j = parse_ashby("initech", &ab);
        assert_eq!(j[0].details.remote.as_deref(), Some("remote"));
        assert_eq!(j[0].details.salary_min, Some(180_000.0));
    }

    #[test]
    fn search_hits_to_jobs() {
        let r = SearchResult {
            url: "https://boards.greenhouse.io/acme/jobs/9?gh_src=x".into(),
            title: "Staff AI Engineer - Acme".into(),
            content: "Remote role building LLM systems".into(),
            engine: "duckduckgo".into(),
        };
        let j = job_from_search(&r).unwrap();
        assert_eq!(j.details.title, "Staff AI Engineer");
        assert_eq!(
            j.details.url.as_deref(),
            Some("https://boards.greenhouse.io/acme/jobs/9")
        );
        let not_job = SearchResult {
            url: "https://example.com/blog/ai".into(),
            title: "AI blog".into(),
            content: String::new(),
            engine: String::new(),
        };
        assert!(job_from_search(&not_job).is_none());
    }
}
