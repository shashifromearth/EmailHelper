//! Research discovery against mocked SearXNG / Greenhouse / Lever / Ashby /
//! RSS endpoints, recruiter discovery, and Telegram notifications.

mod common;

use std::time::Duration;

use common::*;
use emailhelper::db::jobs as jdb;
use serde_json::{json, Value};
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const T: Duration = Duration::from_secs(20);

async fn telegram_mock() -> MockServer {
    let tg = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/botTESTTOKEN/sendMessage$"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": true, "result": {"message_id": 1}})),
        )
        .mount(&tg)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/botTESTTOKEN/getUpdates$"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": true, "result": []}))
                .set_delay(Duration::from_millis(500)),
        )
        .mount(&tg)
        .await;
    tg
}

async fn sent_texts(tg: &MockServer) -> Vec<String> {
    tg.received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.path().ends_with("/sendMessage"))
        .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
        .filter_map(|v| v["text"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn telegram_notifies_on_draft_created() {
    let tg = telegram_mock().await;
    let a = builder()
        .env("TELEGRAM_ENABLED", "true")
        .env("TELEGRAM_BOT_TOKEN", "TESTTOKEN")
        .env("TELEGRAM_CHAT_ID", "12345")
        .env("TELEGRAM_API_BASE", &tg.uri())
        .start()
        .await;
    a.post(
        "/api/v1/emails",
        &email(
            "work",
            "<tg@x>",
            "Jane Doe <jane@acme.com>",
            "Meeting next week",
            "Can we schedule a call next week to discuss support for our team?",
        ),
    )
    .await;
    let d = a.collect_drafts(1, T).await;
    let draft_id = d[0]["draft_id"].as_str().unwrap().to_string();
    let texts = wait_for("telegram message", T, || async {
        let t = sent_texts(&tg).await;
        t.iter()
            .any(|x| x.contains("New draft generated"))
            .then_some(t)
    })
    .await;
    let msg = texts
        .iter()
        .find(|x| x.contains("New draft generated"))
        .unwrap();
    assert!(msg.contains("Sender: Jane Doe <jane@acme.com>"));
    assert!(msg.contains("Subject: Meeting next week"));
    assert!(msg.contains("Account: work mailbox"));
    assert!(msg.contains(&format!("Draft ID: {draft_id}")));
    let req = tg.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(
        &req.iter()
            .find(|r| r.url.path().ends_with("sendMessage"))
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(body["chat_id"], 12345);
    let h: Value = a
        .http
        .get(a.url("/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(h["checks"]["telegram"], "ok");
}

#[tokio::test]
async fn telegram_outage_never_breaks_processing() {
    let tg = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&tg)
        .await;
    let a = builder()
        .env("TELEGRAM_ENABLED", "true")
        .env("TELEGRAM_BOT_TOKEN", "TESTTOKEN")
        .env("TELEGRAM_CHAT_ID", "1")
        .env("TELEGRAM_API_BASE", &tg.uri())
        .start()
        .await;
    a.post(
        "/api/v1/emails",
        &email(
            "work",
            "<tgdown@x>",
            "jane@acme.com",
            "Help",
            "Can you help with my account setup?",
        ),
    )
    .await;
    assert_eq!(a.collect_drafts(1, T).await.len(), 1);
    assert!(a
        .metrics()
        .await
        .contains("failures_total{component=\"telegram\"}"));
}

#[tokio::test]
async fn daily_discovery_collects_ranks_stores_and_summarises() {
    let src = MockServer::start().await;
    let tg = telegram_mock().await;
    Mock::given(method("GET"))
        .and(path("/gh/acme/jobs"))
        .and(query_param("content", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"jobs": [
            {"id": 1, "title": "Staff AI Engineer", "absolute_url": "https://boards.greenhouse.io/acme/jobs/1",
             "location": {"name": "Remote - India"}, "content": "&lt;p&gt;Rust, Python, LLMs, RAG, Kubernetes. 8+ years.&lt;/p&gt;"},
            {"id": 2, "title": "Office Manager", "absolute_url": "https://boards.greenhouse.io/acme/jobs/2",
             "location": {"name": "Berlin"}, "content": "Admin"}
        ]})))
        .mount(&src)
        .await;
    Mock::given(method("GET"))
        .and(path("/lever/globex"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "a1", "text": "Lead AI Engineer", "hostedUrl": "https://jobs.lever.co/globex/a1",
             "categories": {"location": "Bengaluru"}, "workplaceType": "remote", "descriptionPlain": "Python, PyTorch, LLMs"}
        ])))
        .mount(&src)
        .await;
    Mock::given(method("GET"))
        .and(path("/ashby/initech"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"jobs": [
            {"id": "z", "title": "GenAI Architect", "jobUrl": "https://jobs.ashbyhq.com/initech/z", "isRemote": true,
             "location": "Remote", "descriptionPlain": "Design GenAI platforms with Python and Kubernetes"}
        ]})))
        .mount(&src)
        .await;
    Mock::given(method("GET"))
        .and(path("/searx/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": [
            {"url": "https://jobs.lever.co/umbrella/r1?utm_source=x", "title": "Rust Engineer - Umbrella", "content": "Remote Rust role, Tokio, gRPC", "engine": "ddg"},
            {"url": "https://blog.example.com/ai-trends", "title": "AI trends", "content": "", "engine": "ddg"}
        ]})))
        .mount(&src)
        .await;
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<?xml version="1.0"?><rss><channel><item><title>AI Architect at Hooli</title><link>https://careers.hooli.example/jobs/9</link><description>Hybrid, Bengaluru. LLMs.</description></item></channel></rss>"#,
        ))
        .mount(&src)
        .await;

    let a = builder()
        .env("RESEARCH_ENABLED", "true")
        .env("SEARXNG_URL", &format!("{}/searx", src.uri()))
        .env("GREENHOUSE_API_BASE", &format!("{}/gh", src.uri()))
        .env("LEVER_API_BASE", &format!("{}/lever", src.uri()))
        .env("ASHBY_API_BASE", &format!("{}/ashby", src.uri()))
        .env("RESEARCH_GREENHOUSE_BOARDS", "acme")
        .env("RESEARCH_LEVER_BOARDS", "globex")
        .env("RESEARCH_ASHBY_BOARDS", "initech")
        .env("RESEARCH_RSS_FEEDS", &format!("{}/feed.xml", src.uri()))
        // the classic term-based discovery: no freshness/stack/work-type filters
        .env("RESEARCH_MAX_AGE_HOURS", "0")
        .env("RESEARCH_TECH_STACK", "none")
        .env("RESEARCH_WORK_TYPES", "none")
        .env("RESEARCH_REQUIRE_REMOTE", "false")
        .env("TELEGRAM_ENABLED", "true")
        .env("TELEGRAM_BOT_TOKEN", "TESTTOKEN")
        .env("TELEGRAM_CHAT_ID", "1")
        .env("TELEGRAM_API_BASE", &tg.uri())
        .start()
        .await;

    let r = a.post("/api/v1/research/run", &json!({})).await;
    assert_eq!(r.status(), 202);
    let summary = wait_for("discovery summary", T, || async {
        sent_texts(&tg)
            .await
            .into_iter()
            .find(|t| t.contains("Daily Job Discovery"))
    })
    .await;
    let jobs = a
        .app
        .db
        .call(|c| jdb::list_jobs(c, 50, None))
        .await
        .unwrap();
    let titles: Vec<&str> = jobs.iter().map(|j| j.details.title.as_str()).collect();
    for want in [
        "Staff AI Engineer",
        "Lead AI Engineer",
        "GenAI Architect",
        "Rust Engineer",
        "AI Architect",
    ] {
        assert!(titles.contains(&want), "missing {want}: {titles:?}");
    }
    assert!(
        !titles.contains(&"Office Manager"),
        "irrelevant titles filtered"
    );
    // Ranked: the best match comes first.
    assert_eq!(jobs[0].details.title, "Staff AI Engineer");
    assert!(jobs.windows(2).all(|w| w[0].score >= w[1].score));
    // the job agent is off in this test: every kept job is listed with its link and why
    assert!(summary.contains("Not applied — apply yourself if interested"), "{summary}");
    assert!(summary.contains("Why not: the job agent is off (JOB_AGENT_ENABLED)"), "{summary}");
    assert!(summary.contains("Why not: relocation required"), "{summary}");
    assert!(summary.contains("Staff AI Engineer"));
    let m = a.metrics().await;
    assert!(metric_sum(&m, "jobs_discovered_total") >= 5.0);
    assert!(m.contains("research_runs_total{status=\"ok\"} 1"), "{m}");

    // Second run: nothing new (deduplicated by fingerprint).
    a.post("/api/v1/research/run", &json!({})).await;
    wait_for("second summary", T, || async {
        let n = sent_texts(&tg)
            .await
            .iter()
            .filter(|t| t.contains("Daily Job Discovery"))
            .count();
        (n >= 2).then_some(())
    })
    .await;
    let again = a
        .app
        .db
        .call(|c| jdb::list_jobs(c, 50, None))
        .await
        .unwrap();
    assert_eq!(again.len(), jobs.len());
}

#[tokio::test]
async fn recruiter_email_triggers_recruiter_discovery_and_outreach_drafts() {
    let src = MockServer::start().await;
    let tg = telegram_mock().await;
    Mock::given(method("GET"))
        .and(path("/searx/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": [
            {"url": "https://globex.example/careers", "title": "Globex careers", "content": "Engineering at Globex", "engine": "ddg"}
        ]})))
        .mount(&src)
        .await;
    let worker = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/linkedin/people"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"people": [
            {"name": "Ravi Kumar", "headline": "Senior Technical Recruiter at Globex", "profile_url": "https://www.linkedin.com/in/ravi"},
            {"name": "Meera Iyer", "headline": "Engineering Manager, ML Platform at Globex", "profile_url": "https://www.linkedin.com/in/meera"}
        ], "blocker": null})))
        .expect(1)
        .mount(&worker)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/extract"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"final_url":"https://jobs.lever.co/globex/abc-123","title":"Lead AI Engineer - Globex","text":"Remote. Python, LLMs.","json_ld":[],"links":[]})))
        .mount(&worker)
        .await;

    let a = builder()
        .env("RECRUITER_DISCOVERY_ENABLED", "true")
        .env("RECRUITER_LINKEDIN_ENABLED", "true")
        .env("BROWSER_WORKER_URL", &worker.uri())
        .env("BROWSER_WORKER_TOKEN", "w")
        .env("SEARXNG_URL", &format!("{}/searx", src.uri()))
        .env("TELEGRAM_ENABLED", "true")
        .env("TELEGRAM_BOT_TOKEN", "TESTTOKEN")
        .env("TELEGRAM_CHAT_ID", "1")
        .env("TELEGRAM_API_BASE", &tg.uri())
        .start()
        .await;
    a.post(
        "/api/v1/emails",
        &email(
            "acct1",
            "<rec@globex>",
            "Priya Sharma <priya@globex.io>",
            "Opportunity: Lead AI Engineer",
            "Hi Asha, I came across your profile and we are hiring a Lead AI Engineer. Would you be interested? JD: https://jobs.lever.co/globex/abc-123\n\nThanks,\nPriya Sharma\nTalent Acquisition Lead\nGlobex",
        ),
    )
    .await;
    let recs = wait_for("recruiters", T, || async {
        let r = a
            .app
            .db
            .call(|c| jdb::list_recruiters(c, Some("Globex"), 10))
            .await
            .ok()?;
        (r.len() >= 3).then_some(r)
    })
    .await;
    assert_eq!(
        recs[0].name, "Ravi Kumar",
        "technical recruiter ranked first"
    );
    let priya = recs.iter().find(|r| r.name == "Priya Sharma").unwrap();
    assert_eq!(priya.email.as_deref(), Some("priya@globex.io"));
    assert_eq!(priya.title.as_deref(), Some("Talent Acquisition Lead"));
    assert!(recs.iter().filter(|r| r.outreach_draft.is_some()).count() >= 2);

    // The recruiter email itself gets a reply draft (not an alert).
    let drafts = a.collect_drafts(1, T).await;
    assert!(drafts
        .iter()
        .any(|d| d["kind"] == "reply" && d["to"][0] == "priya@globex.io"));

    let texts = wait_for("recruiter telegram", T, || async {
        sent_texts(&tg)
            .await
            .into_iter()
            .find(|t| t.contains("Recruiters discovered at Globex"))
    })
    .await;
    assert!(texts.contains("nothing was sent"));
    assert!(metric_sum(&a.metrics().await, "recruiter_matches_total") >= 3.0);
    let findings: i64 = a
        .app
        .db
        .call(|c| Ok(c.query_row("SELECT COUNT(*) FROM research_findings", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert!(findings >= 1, "company research stored");
}

#[tokio::test]
async fn remote_boards_keep_only_fresh_india_open_stack_jobs() {
    let src = MockServer::start().await;
    let tg = telegram_mock().await;
    let fresh = (chrono::Utc::now() - chrono::Duration::hours(3)).format("%Y-%m-%dT%H:%M:%S").to_string();
    let stale = (chrono::Utc::now() - chrono::Duration::hours(40)).format("%Y-%m-%dT%H:%M:%S").to_string();
    Mock::given(method("GET"))
        .and(path("/remotive"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"jobs": [
            {"id": 1, "title": "Senior Python Engineer (GenAI)", "company_name": "Askable", "candidate_required_location": "Worldwide",
             "publication_date": fresh, "job_type": "contract", "url": "https://remotive.com/remote-jobs/1", "description": "Python, LLMs, RAG", "tags": ["python"]},
            {"id": 2, "title": "Java Backend Developer", "company_name": "Old Co", "candidate_required_location": "Worldwide",
             "publication_date": stale, "job_type": "full_time", "url": "https://remotive.com/remote-jobs/2", "description": "Java, Spring Boot", "tags": []},
            {"id": 3, "title": ".NET Engineer", "company_name": "US Co", "candidate_required_location": "USA",
             "publication_date": fresh, "job_type": "full_time", "url": "https://remotive.com/remote-jobs/3", "description": "C# and .NET", "tags": []},
            {"id": 4, "title": "Frontend Engineer", "company_name": "UI Co", "candidate_required_location": "Worldwide",
             "publication_date": fresh, "job_type": "full_time", "url": "https://remotive.com/remote-jobs/4", "description": "React, JavaScript", "tags": []},
            {"id": 5, "title": "AI Agent Engineer", "company_name": "Anywhere Inc", "candidate_required_location": "Anywhere",
             "publication_date": fresh, "job_type": "full_time", "url": "https://remotive.com/remote-jobs/5",
             "description": "Agentic AI with LangGraph. You must be located in the US.", "tags": []},
            {"id": 6, "title": "C# .NET Consultant", "company_name": "Desi Tech", "candidate_required_location": "India",
             "publication_date": fresh, "job_type": "contract", "url": "https://remotive.com/remote-jobs/6", "description": "C#, ASP.NET Core, Azure", "tags": []}
        ]})))
        .mount(&src)
        .await;
    let a = builder()
        .env("RESEARCH_ENABLED", "true")
        .env("RESEARCH_REMOTE_BOARDS", "remotive")
        .env("RESEARCH_REMOTIVE_URL", &format!("{}/remotive", src.uri()))
        .env("RESEARCH_APPLICANT_COUNTRY", "India")
        .env("TELEGRAM_ENABLED", "true")
        .env("TELEGRAM_BOT_TOKEN", "TESTTOKEN")
        .env("TELEGRAM_CHAT_ID", "1")
        .env("TELEGRAM_API_BASE", &tg.uri())
        .start()
        .await;
    a.post("/api/v1/research/run", &json!({})).await;
    let summary = wait_for("discovery summary", T, || async {
        sent_texts(&tg).await.into_iter().find(|t| t.contains("Daily Job Discovery"))
    })
    .await;
    let jobs = a.app.db.call(|c| jdb::list_jobs(c, 50, None)).await.unwrap();
    let mut titles: Vec<&str> = jobs.iter().map(|j| j.details.title.as_str()).collect();
    titles.sort();
    assert_eq!(titles, vec!["C# .NET Consultant", "Senior Python Engineer (GenAI)"], "{summary}");
    assert!(summary.contains("posted in the last 25 h"), "{summary}");
    assert!(summary.contains("1 posted more than 25 h ago"), "{summary}");
    assert!(summary.contains("https://remotive.com/remote-jobs/1"), "kept jobs are listed with their link: {summary}");
    assert!(summary.contains("Why not:"), "{summary}");
    // the three full-time jobs (USA-only, frontend, "located in the US") fail the role-type rule first
    assert!(summary.contains("3 not a contract / consultant / freelance role"), "{summary}");
    assert!(summary.contains("remote · contract/freelance/consultant"), "{summary}");
}
