//! Job APIs (Adzuna, Jooble, Careerjet, Arbeitnow, The Muse) and search APIs
//! (Google Programmable Search, Brave) against wiremock: request shape, keys,
//! parsing, budget and rate-limit handling. No real API is ever called.

use std::collections::HashMap;

use emailhelper::config::Config;
use emailhelper::research::job_apis;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(pairs: &[(&str, String)]) -> Config {
    let mut m: HashMap<String, String> = HashMap::from([
        ("RESEARCH_API_DELAY_MS".to_string(), "0".to_string()),
        ("RESEARCH_API_QUERIES".to_string(), "python developer".to_string()),
    ]);
    m.extend(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())));
    Config::from_lookup(|k| m.get(k).cloned()).unwrap()
}

fn hours_ago(h: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::hours(h)).to_rfc3339()
}

async fn collect(c: &Config) -> (Vec<emailhelper::research::sources::RawJob>, Vec<String>) {
    let mut errors = Vec::new();
    let jobs = job_apis::collect(&reqwest::Client::new(), &c.research.apis, c.research.max_age_hours, &mut errors).await;
    (jobs, errors)
}

#[tokio::test]
async fn adzuna_queries_each_country_and_parses_jobs() {
    let srv = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/adzuna/in/search/1"))
        .and(query_param("app_id", "id1"))
        .and(query_param("app_key", "key1"))
        .and(query_param("what", "python developer"))
        .and(query_param("max_days_old", "2"))
        .and(query_param("sort_by", "date"))
        .and(header("user-agent", job_apis::USER_AGENT))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": [
            {"id": "4711", "title": "Senior <strong>Python</strong> Developer", "company": {"display_name": "Acme"},
             "location": {"display_name": "Bengaluru, Karnataka", "area": ["India", "Karnataka", "Bengaluru"]},
             "created": hours_ago(3), "redirect_url": "https://www.adzuna.in/land/ad/4711",
             "description": "Python, FastAPI, 8+ years.", "contract_type": "contract", "contract_time": "full_time",
             "salary_min": 2500000, "salary_max": 4000000, "salary_is_predicted": "0"},
            {"id": "4712", "title": "Java Lead", "company": {"display_name": "Globex"},
             "location": {"display_name": "Pune"}, "created": hours_ago(5), "redirect_url": "https://www.adzuna.in/land/ad/4712",
             "description": "Java", "salary_min": 100, "salary_max": 200, "salary_is_predicted": "1"}
        ]})))
        .expect(1)
        .mount(&srv)
        .await;
    Mock::given(method("GET"))
        .and(path("/adzuna/gb/search/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
        .expect(1)
        .mount(&srv)
        .await;
    let c = cfg(&[
        ("RESEARCH_SOURCES", "adzuna".into()),
        ("ADZUNA_APP_ID", "id1".into()),
        ("ADZUNA_APP_KEY", "key1".into()),
        ("ADZUNA_COUNTRIES", "in;gb".into()),
        ("ADZUNA_API_BASE", format!("{}/adzuna", srv.uri())),
        ("RESEARCH_MAX_AGE_HOURS", "36".into()),
    ]);
    let (jobs, errors) = collect(&c).await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(jobs.len(), 2);
    let j = &jobs[0];
    assert_eq!(j.source, "adzuna");
    assert_eq!(j.details.title, "Senior Python Developer", "HTML stripped from titles");
    assert_eq!(j.details.company.as_deref(), Some("Acme"));
    assert_eq!(j.region.as_deref(), Some("Bengaluru, Karnataka, India"));
    assert_eq!(j.employment_type.as_deref(), Some("contract, full_time"));
    assert_eq!(j.details.salary_max, Some(4_000_000.0));
    assert_eq!(j.details.salary_currency.as_deref(), Some("INR"));
    assert!(j.posted_at.is_some());
    assert_eq!(jobs[1].details.salary_max, None, "predicted salaries are ignored");
}

#[tokio::test]
async fn jooble_posts_keywords_and_location() {
    let srv = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/jooble/secret-key"))
        .and(body_partial_json(json!({"keywords": "python developer", "location": "Remote"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"totalCount": 1, "jobs": [
            {"id": 99, "title": "Python Engineer (Contract)", "company": "Initech", "location": "Remote",
             "snippet": "&lt;b&gt;Python&lt;/b&gt; and LLMs. Worldwide.", "salary": "$60 - $80 per hour", "type": "Contract",
             "link": "https://jooble.org/desc/99", "updated": "2026-10-30T00:00:00.0000000"}
        ]})))
        .expect(1)
        .mount(&srv)
        .await;
    let c = cfg(&[
        ("RESEARCH_SOURCES", "jooble".into()),
        ("JOOBLE_API_KEY", "secret-key".into()),
        ("JOOBLE_LOCATIONS", "Remote".into()),
        ("JOOBLE_API_BASE", format!("{}/jooble", srv.uri())),
    ]);
    let (jobs, errors) = collect(&c).await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(jobs[0].details.company.as_deref(), Some("Initech"));
    assert_eq!(jobs[0].details.remote.as_deref(), Some("remote"));
    assert_eq!(jobs[0].employment_type.as_deref(), Some("Contract"));
    assert_eq!(jobs[0].details.salary_currency.as_deref(), Some("USD"));
    assert!(jobs[0].posted_at.unwrap().to_rfc3339().starts_with("2026-10-30T00:00:00"), "7-digit fractions parse");
}

#[tokio::test]
async fn careerjet_sends_affid_locale_and_client_details() {
    let srv = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/careerjet"))
        .and(query_param("affid", "aff123"))
        .and(query_param("locale_code", "en_IN"))
        .and(query_param("keywords", "python developer"))
        .and(query_param("user_agent", job_apis::USER_AGENT))
        .and(query_param("user_ip", "203.0.113.5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"type": "JOBS", "hits": 1, "jobs": [
            {"url": "https://www.careerjet.co.in/jobad/in123", "title": "GenAI Architect", "company": "Hooli",
             "date": "Fri, 30 Oct 2026 07:45:21 GMT", "description": "Design LLM and RAG platforms.",
             "locations": "Hyderabad", "salary": "", "salary_min": "", "salary_max": "", "salary_currency_code": "INR"}
        ]})))
        .expect(1)
        .mount(&srv)
        .await;
    let c = cfg(&[
        ("RESEARCH_SOURCES", "careerjet".into()),
        ("CAREERJET_AFFID", "aff123".into()),
        ("CAREERJET_LOCALES", "en_IN".into()),
        ("CAREERJET_USER_IP", "203.0.113.5".into()),
        ("CAREERJET_API_BASE", format!("{}/careerjet", srv.uri())),
    ]);
    let (jobs, errors) = collect(&c).await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(jobs[0].region.as_deref(), Some("Hyderabad, India"), "the locale's country is added");
    assert_eq!(jobs[0].details.salary_max, None, "empty salary fields stay empty");
    assert!(jobs[0].posted_at.is_some());
}

#[tokio::test]
async fn arbeitnow_and_themuse_need_no_key_and_page_politely() {
    let srv = MockServer::start().await;
    for p in ["1", "2"] {
        Mock::given(method("GET"))
            .and(path("/arbeitnow"))
            .and(query_param("page", p))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
                {"slug": format!("rust-dev-{p}"), "company_name": "Initech GmbH", "title": format!("Rust Developer {p}"),
                 "description": "<p>Rust, Tokio. Visa sponsorship available.</p>", "remote": p == "1",
                 "url": format!("https://www.arbeitnow.com/jobs/rust-dev-{p}"), "tags": ["Rust"], "job_types": ["full time"],
                 "location": "Berlin", "created_at": chrono::Utc::now().timestamp() - 3600}
            ]})))
            .expect(1)
            .mount(&srv)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/muse"))
        .and(query_param("page", "1"))
        .and(query_param("location", "Flexible / Remote"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"page": 1, "page_count": 1, "results": [
            {"id": 7, "name": "Engineering Manager, AI Platform", "contents": "<p>Lead Python and LLM teams.</p>",
             "publication_date": hours_ago(2), "locations": [{"name": "Flexible / Remote"}],
             "levels": [{"name": "Management"}], "categories": [{"name": "Software Engineering"}],
             "company": {"name": "Umbrella"}, "refs": {"landing_page": "https://www.themuse.com/jobs/umbrella/em-ai"}}
        ]})))
        .expect(1)
        .mount(&srv)
        .await;
    let c = cfg(&[
        ("RESEARCH_SOURCES", "arbeitnow;themuse".into()),
        ("ARBEITNOW_PAGES", "2".into()),
        ("ARBEITNOW_API_BASE", format!("{}/arbeitnow", srv.uri())),
        ("THEMUSE_PAGES", "1".into()),
        ("THEMUSE_API_BASE", format!("{}/muse", srv.uri())),
    ]);
    let (jobs, errors) = collect(&c).await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(jobs.len(), 3);
    assert_eq!(jobs[0].details.remote.as_deref(), Some("remote"));
    assert!(jobs[0].details.skills.contains(&"Rust".to_string()), "tags are skills");
    assert_eq!(jobs[2].source, "themuse");
    assert_eq!(jobs[2].details.remote.as_deref(), Some("remote"));
    assert!(jobs[2].details.skills.contains(&"Management".to_string()));
}

#[tokio::test]
async fn search_apis_return_platform_postings_only() {
    let srv = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/google"))
        .and(query_param("key", "gkey"))
        .and(query_param("cx", "gcx"))
        .and(query_param("dateRestrict", "d2"))
        .and(query_param("q", "site:linkedin.com/jobs/view \"architect\" python"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            {"title": "Acme hiring Python Architect in Bengaluru, Karnataka, India | LinkedIn",
             "link": "https://in.linkedin.com/jobs/view/python-architect-at-acme-4012345678",
             "snippet": "1 day ago ... Python, Azure, GenAI."},
            {"title": "How to become an architect", "link": "https://example.com/blog", "snippet": ""}
        ]})))
        .expect(1)
        .mount(&srv)
        .await;
    Mock::given(method("GET"))
        .and(path("/brave"))
        .and(header("x-subscription-token", "bkey"))
        .and(query_param("freshness", "pw"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"web": {"results": [
            {"title": "Python Architect - Globex - Remote | Naukri.com",
             "url": "https://www.naukri.com/job-listings-python-architect-globex-remote-301024001234",
             "description": "Python architect, remote, contract.", "age": "3 hours ago"}
        ]}})))
        .mount(&srv)
        .await;
    let c = cfg(&[
        ("RESEARCH_SOURCES", "google_cse;brave".into()),
        ("GOOGLE_CSE_KEY", "gkey".into()),
        ("GOOGLE_CSE_CX", "gcx".into()),
        ("GOOGLE_CSE_API_BASE", format!("{}/google", srv.uri())),
        ("BRAVE_SEARCH_KEY", "bkey".into()),
        ("BRAVE_SEARCH_API_BASE", format!("{}/brave", srv.uri())),
        ("RESEARCH_SEARCH_ROLES", "architect".into()),
        ("RESEARCH_SEARCH_STACKS", "python".into()),
        ("RESEARCH_SEARCH_SITES", "linkedin;naukri".into()),
        ("RESEARCH_API_MAX_REQUESTS_PER_SOURCE", "1".into()),
        ("RESEARCH_MAX_AGE_HOURS", "48".into()),
    ]);
    let (jobs, errors) = collect(&c).await;
    assert!(errors.is_empty(), "{errors:?}");
    let g: Vec<_> = jobs.iter().filter(|j| j.source == "google_cse:linkedin").collect();
    assert_eq!(g.len(), 1, "non-posting hits are dropped");
    assert_eq!(g[0].details.url.as_deref(), Some("https://www.linkedin.com/jobs/view/4012345678/"));
    assert_eq!(g[0].details.title, "Python Architect");
    assert_eq!(g[0].region.as_deref(), Some("Bengaluru, Karnataka, India"));
    // Brave took the other query of the two (one request each, adjacent windows).
    let reqs = srv.received_requests().await.unwrap();
    let brave_q: Vec<String> = reqs
        .iter()
        .filter(|r| r.url.path() == "/brave")
        .filter_map(|r| r.url.query_pairs().find(|(k, _)| k == "q").map(|(_, v)| v.into_owned()))
        .collect();
    assert_eq!(brave_q, vec!["site:naukri.com/job-listings \"architect\" python"]);
    let b: Vec<_> = jobs.iter().filter(|j| j.source == "brave:naukri").collect();
    assert_eq!(b[0].details.company.as_deref(), Some("Globex"));
    assert_eq!(b[0].details.remote.as_deref(), Some("remote"));
}

#[tokio::test]
async fn keyless_sources_are_skipped_and_limits_respected() {
    let srv = MockServer::start().await;
    // 429 stops the source for the day: one request, then nothing more.
    Mock::given(method("GET"))
        .and(path("/adzuna/in/search/1"))
        .respond_with(ResponseTemplate::new(429))
        .expect(1)
        .mount(&srv)
        .await;
    Mock::given(method("GET"))
        .and(path("/arbeitnow"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .expect(2)
        .mount(&srv)
        .await;
    let c = cfg(&[
        ("RESEARCH_SOURCES", "jooble;careerjet;google_cse;brave;adzuna;arbeitnow".into()),
        ("RESEARCH_API_QUERIES", "python developer;java developer;rust developer".into()),
        ("ADZUNA_APP_ID", "id".into()),
        ("ADZUNA_APP_KEY", "key".into()),
        ("ADZUNA_COUNTRIES", "in".into()),
        ("ADZUNA_API_BASE", format!("{}/adzuna", srv.uri())),
        ("ARBEITNOW_PAGES", "5".into()),
        ("ARBEITNOW_API_BASE", format!("{}/arbeitnow", srv.uri())),
        // the total budget: 1 for Adzuna's failed request, 2 left for Arbeitnow
        ("RESEARCH_API_MAX_REQUESTS", "3".into()),
    ]);
    let (jobs, errors) = collect(&c).await;
    assert!(jobs.is_empty());
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].starts_with("adzuna: rate limited"), "{errors:?}");
}

#[test]
fn defaults_list_every_source_and_keys_are_optional() {
    let c = Config::from_lookup(|_| None).unwrap();
    let a = &c.research.apis;
    assert_eq!(a.sources, job_apis::SOURCES);
    assert!(!c.research.require_remote, "on-site jobs are kept by default");
    assert!(c.research.work_types.is_empty());
    assert_eq!(c.research.preferred_work_types, vec!["contract", "freelance", "consultant"]);
    for s in ["adzuna", "jooble", "careerjet", "google_cse", "brave"] {
        assert!(job_apis::missing_key(a, s).is_some(), "{s} needs a key");
    }
    for s in ["arbeitnow", "themuse"] {
        assert!(job_apis::missing_key(a, s).is_none(), "{s} needs no key");
    }
}
