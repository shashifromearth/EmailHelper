//! Find a job from a platform alert on the employer's own public job board
//! (Greenhouse, Lever or Ashby). Those boards publish open JSON APIs and
//! their application forms need no account, so the job agent can apply there
//! without touching LinkedIn or Naukri.

use rusqlite::{params, Connection, OptionalExtension};

use crate::app::App;
use crate::db::jobs::JobDetails;
use crate::error::AppResult;
use crate::jobs::alerts::AlertJob;
use crate::research::sources::{self, RawJob};

/// How long a lookup result (board found or not) is trusted.
const CACHE_DAYS: i64 = 7;
/// Minimum title similarity for "the same job".
pub const TITLE_MATCH: f64 = 0.75;

const SUFFIXES: &[&str] = &[
    "pvt", "private", "ltd", "limited", "inc", "llc", "llp", "corp", "corporation", "co", "company", "plc", "gmbh",
    "technologies", "technology", "services", "solutions", "india", "global", "group", "software", "systems",
    "consulting", "labs", "hq",
];

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .replace('&', " and ")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Company name without legal/generic suffixes ("Accenture services Pvt Ltd" → ["accenture"]).
fn core_words(company: &str) -> Vec<String> {
    let mut w = words(company);
    while w.len() > 1 && w.last().is_some_and(|x| SUFFIXES.contains(&x.as_str())) {
        w.pop();
    }
    w
}

/// Cache key for a company.
pub fn company_key(company: &str) -> String {
    core_words(company).join(" ")
}

/// Board names to try, most likely first ("Netrolynx AI" → netrolynxai, netrolynx-ai).
pub fn slug_candidates(company: &str) -> Vec<String> {
    let core = core_words(company);
    let full = words(company);
    let mut out: Vec<String> = vec![core.join(""), core.join("-"), full.join("")];
    out.retain(|s| s.len() >= 2);
    let mut seen = std::collections::HashSet::new();
    out.retain(|s| seen.insert(s.clone()));
    out
}

fn title_tokens(t: &str) -> std::collections::HashSet<String> {
    const STOP: &[&str] = &["and", "of", "the", "for", "with", "in", "at", "to", "a", "an", "i", "ii", "iii"];
    words(t)
        .into_iter()
        .map(|w| match w.as_str() {
            "sr" => "senior".to_string(),
            "jr" => "junior".to_string(),
            "eng" | "engg" | "engineering" => "engineer".to_string(),
            "mgr" => "manager".to_string(),
            "dev" | "developers" => "developer".to_string(),
            "engineers" => "engineer".to_string(),
            "architects" => "architect".to_string(),
            "genai" => "generative".to_string(),
            _ => w,
        })
        .filter(|w| !STOP.contains(&w.as_str()))
        .collect()
}

/// Dice similarity of two job titles (0..1).
pub fn title_similarity(a: &str, b: &str) -> f64 {
    let (x, y) = (title_tokens(a), title_tokens(b));
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    let common = x.intersection(&y).count() as f64;
    2.0 * common / (x.len() + y.len()) as f64
}

/// Locations are compatible unless both name countries and none overlap.
pub fn location_compatible(alert: Option<&str>, job: &JobDetails) -> bool {
    let (Some(a), Some(j)) = (alert, job.location.as_deref()) else {
        return true;
    };
    let ca = crate::knowledge::profile::countries_in(a);
    let cj = crate::knowledge::profile::countries_in(j);
    if ca.is_empty() || cj.is_empty() {
        return true;
    }
    ca.iter().any(|c| cj.contains(c))
}

/// Legal-form and country words that don't identify a company.
const LEGAL: &[&str] = &[
    "pvt", "private", "ltd", "limited", "inc", "llc", "llp", "corp", "corporation", "co", "company", "plc", "gmbh",
    "india",
];
/// First words too common to identify a company on their own.
const GENERIC: &[&str] = &[
    "applied", "digital", "global", "data", "cloud", "smart", "tech", "first", "united", "general", "national",
    "prime", "alpha", "next", "open", "future", "modern", "quantum", "infinite", "bright", "blue", "green",
];

/// Does a posting belong to `company`? Its text must name the company: the
/// full name, or a distinctive first word ("Valtech" for "Valtech India Systems").
pub fn belongs_to(company: &str, d: &JobDetails) -> bool {
    let text = format!(
        "{} {}",
        d.company.as_deref().unwrap_or(""),
        d.description.as_deref().unwrap_or("")
    )
    .to_lowercase();
    if d.description.as_deref().unwrap_or("").trim().is_empty() {
        return false;
    }
    let w: Vec<String> = words(company).into_iter().filter(|x| !LEGAL.contains(&x.as_str())).collect();
    if w.is_empty() {
        return false;
    }
    let phrase = w.join(" ");
    if crate::jobs::extract::contains_word(&text, &phrase) {
        return true;
    }
    let first = &w[0];
    first.len() >= 5 && !GENERIC.contains(&first.as_str()) && crate::jobs::extract::contains_word(&text, first)
}

/// Pick the posting on a board that is the alert's job.
pub fn best_match(alert: &AlertJob, jobs: Vec<RawJob>) -> Option<RawJob> {
    jobs.into_iter()
        .filter(|j| location_compatible(alert.location.as_deref(), &j.details))
        .filter(|j| alert.company.as_deref().is_none_or(|c| belongs_to(c, &j.details)))
        .map(|j| (title_similarity(&alert.title, &j.details.title), j))
        .filter(|(s, _)| *s >= TITLE_MATCH)
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, j)| j)
}

// ---------------------------------------------------------------- cache

pub fn cached(c: &Connection, key: &str) -> AppResult<Option<(Option<String>, Option<String>)>> {
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(CACHE_DAYS))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    Ok(c.query_row(
        "SELECT ats, slug FROM employer_boards WHERE company_key=?1 AND checked_at > ?2",
        params![key, cutoff],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

pub fn remember(c: &Connection, key: &str, ats: Option<&str>, slug: Option<&str>) -> AppResult<()> {
    c.execute(
        "INSERT INTO employer_boards(company_key, ats, slug, checked_at) VALUES (?1,?2,?3,?4)
         ON CONFLICT(company_key) DO UPDATE SET ats=excluded.ats, slug=excluded.slug, checked_at=excluded.checked_at",
        params![key, ats, slug, crate::db::now()],
    )?;
    Ok(())
}

// ---------------------------------------------------------------- lookup

async fn board_jobs(app: &App, ats: &str, slug: &str) -> Option<Vec<RawJob>> {
    let r = &app.cfg.research;
    let url = match ats {
        "greenhouse" => format!("{}/{slug}/jobs?content=true", r.greenhouse_api_base.trim_end_matches('/')),
        "lever" => format!("{}/{slug}?mode=json", r.lever_api_base.trim_end_matches('/')),
        "ashby" => format!("{}/{slug}?includeCompensation=true", r.ashby_api_base.trim_end_matches('/')),
        _ => return None,
    };
    let v = sources::fetch_json(&app.http, &url).await.ok()?;
    let jobs = match ats {
        "greenhouse" => sources::parse_greenhouse(slug, &v),
        "lever" => sources::parse_lever(slug, &v),
        _ => sources::parse_ashby(slug, &v),
    };
    (!jobs.is_empty()).then_some(jobs)
}

/// The employer's board for a company, from the cache or by probing the three
/// ATS APIs. `None` when the company has no public Greenhouse/Lever/Ashby board.
pub async fn find_board(app: &App, company: &str) -> AppResult<Option<(String, String, Vec<RawJob>)>> {
    let key = company_key(company);
    if key.is_empty() {
        return Ok(None);
    }
    let k2 = key.clone();
    if let Some((ats, slug)) = app.db.call(move |c| cached(c, &k2)).await? {
        return Ok(match (ats, slug) {
            (Some(a), Some(s)) => board_jobs(app, &a, &s).await.map(|j| (a, s, j)),
            _ => None,
        });
    }
    for ats in ["greenhouse", "lever", "ashby"] {
        for slug in slug_candidates(company) {
            if let Some(jobs) = board_jobs(app, ats, &slug).await {
                let (k2, a2, s2) = (key.clone(), ats.to_string(), slug.clone());
                app.db
                    .call(move |c| remember(c, &k2, Some(&a2), Some(&s2)))
                    .await?;
                tracing::info!(company, ats, slug, jobs = jobs.len(), "employer job board found");
                return Ok(Some((ats.to_string(), slug, jobs)));
            }
        }
    }
    let k2 = key.clone();
    app.db.call(move |c| remember(c, &k2, None, None)).await?;
    Ok(None)
}

/// The alert's job on the employer's own board, if it is there.
pub async fn find_posting(app: &App, alert: &AlertJob) -> AppResult<Option<RawJob>> {
    let Some(company) = alert.company.as_deref() else {
        return Ok(None);
    };
    Ok(find_board(app, company)
        .await?
        .and_then(|(_, _, jobs)| best_match(alert, jobs)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_keys() {
        assert_eq!(slug_candidates("Netrolynx AI"), vec!["netrolynxai", "netrolynx-ai"]);
        assert_eq!(slug_candidates("Accenture services Pvt Ltd")[0], "accenture");
        assert_eq!(company_key("GitLab Inc."), "gitlab");
        assert_eq!(slug_candidates("ID Logistics US")[0], "idlogisticsus");
    }

    #[test]
    fn titles_match_loosely_but_not_wrongly() {
        assert!(title_similarity("Lead AI Engineer", "Lead AI Engineer") > 0.99);
        assert!(title_similarity("Sr. Backend Engineer", "Senior Backend Engineer") > 0.99);
        assert!(title_similarity("Staff Software Engineer, GTM AI Engineering", "Staff Software Engineer - GTM AI") >= TITLE_MATCH);
        assert!(title_similarity("Lead AI Engineer", "AI Support Engineer") < TITLE_MATCH);
        assert!(title_similarity("Technology Architect", "Lead Salesforce Engineer") < TITLE_MATCH);
    }

    fn raw(title: &str, loc: &str) -> RawJob {
        RawJob::new(
            "greenhouse",
            None,
            JobDetails {
                title: title.into(),
                location: Some(loc.into()),
                description: Some("Join Acme to build AI systems.".into()),
                ..Default::default()
            },
        )
    }

    #[test]
    fn board_must_name_the_company() {
        let d = |desc: &str| JobDetails {
            title: "Engineer".into(),
            description: Some(desc.into()),
            ..Default::default()
        };
        assert!(!belongs_to("Applied Systems", &d("Applied Intuition builds autonomy software.")));
        assert!(belongs_to("Applied Systems", &d("At Applied Systems we build insurance software.")));
        assert!(belongs_to("Valtech India Systems", &d("Valtech is a global agency.")));
        assert!(belongs_to("Beacon Holdings", &d("Beacon is hiring.")));
        assert!(!belongs_to("Beacon Holdings", &JobDetails { title: "x".into(), ..Default::default() }), "no text, no proof");
    }

    #[test]
    fn best_match_respects_country() {
        let alert = AlertJob {
            title: "Lead AI Engineer".into(),
            company: Some("Acme".into()),
            location: Some("Bengaluru, India".into()),
            ..Default::default()
        };
        let jobs = vec![raw("Lead AI Engineer", "New York, United States"), raw("Lead AI Engineer", "Bengaluru")];
        let m = best_match(&alert, jobs).unwrap();
        assert_eq!(m.details.location.as_deref(), Some("Bengaluru"));
        assert!(best_match(&alert, vec![raw("Recruiter", "Bengaluru")]).is_none());
    }

    #[tokio::test]
    async fn cache_round_trip() {
        let db = crate::db::Db::open_in_memory().unwrap();
        db.call(|c| {
            remember(c, "acme", Some("lever"), Some("acme"))?;
            assert_eq!(cached(c, "acme")?, Some((Some("lever".into()), Some("acme".into()))));
            remember(c, "nobody", None, None)?;
            assert_eq!(cached(c, "nobody")?, Some((None, None)));
            assert_eq!(cached(c, "unknown")?, None);
            Ok(())
        })
        .await
        .unwrap();
    }
}
