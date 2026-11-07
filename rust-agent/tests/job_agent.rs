//! Job Application Agent end-to-end tests with a mocked browser worker and a
//! mocked Telegram Bot API.

mod common;

use std::time::Duration;

use common::*;
use emailhelper::db::jobs as jdb;
use emailhelper::telegram::client::{Chat, Message};
use serde_json::{json, Value};
use wiremock::matchers::{body_partial_json, header, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const T: Duration = Duration::from_secs(20);
const JOB_URL: &str = "https://boards.greenhouse.io/acme/jobs/4012345";

fn job_page() -> Value {
    json!({
        "final_url": JOB_URL,
        "title": "Job Application for Staff AI Engineer at Acme",
        "text": "Staff AI Engineer. Remote (India). 8+ years. Rust, Python, LLMs, RAG, Kubernetes.",
        "json_ld": [{
            "@type": "JobPosting", "title": "Staff AI Engineer",
            "hiringOrganization": {"name": "Acme"}, "jobLocationType": "TELECOMMUTE",
            "baseSalary": {"currency": "INR", "value": {"minValue": 5000000, "maxValue": 7000000, "unitText": "YEAR"}},
            "description": "<p>8+ years building ML systems with Rust, Python, LLMs, RAG and Kubernetes.</p>"
        }],
        "links": [],
        "blocker": null
    })
}

fn fields(extra: Value) -> Value {
    let mut f = vec![
        json!({"id":"first_name","label":"First Name","type":"text","required":true,"options":[]}),
        json!({"id":"last_name","label":"Last Name","type":"text","required":true,"options":[]}),
        json!({"id":"email","label":"Email","type":"email","required":true,"options":[]}),
        json!({"id":"phone","label":"Phone","type":"tel","required":true,"options":[]}),
        json!({"id":"resume","label":"Resume/CV","type":"file","required":true,"options":[],"accept":".pdf"}),
        json!({"id":"q_auth","label":"Are you legally authorized to work in India?","type":"select","required":true,"options":["Yes","No"]}),
        json!({"id":"cover_letter","label":"Cover Letter","type":"textarea","required":false,"options":[]}),
    ];
    if let Value::Array(e) = extra {
        f.extend(e);
    }
    Value::Array(f)
}

struct Env {
    worker: MockServer,
    tg: MockServer,
    agent: TestAgent,
}

async fn setup(mode: &str, extra_fields: Value, telegram: bool) -> Env {
    let worker = MockServer::start().await;
    let tg = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/extract"))
        .and(header("X-Worker-Token", "worker-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(job_page()))
        .mount(&worker)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/apply/inspect"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "final_url": format!("{JOB_URL}#app"), "ats": "greenhouse", "fields": fields(extra_fields), "blocker": null
        })))
        .mount(&worker)
        .await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status":"ok"})))
        .mount(&worker)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/botTESTTOKEN/sendMessage$"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": true, "result": {"message_id": 4242}})),
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

    let resume_dir = tempfile::tempdir().unwrap();
    let resume = resume_dir.path().join("resume.pdf");
    std::fs::write(&resume, b"%PDF-1.4\n%test\n").unwrap();
    let resume_path = resume.to_string_lossy().to_string();
    std::mem::forget(resume_dir);

    let mut b = builder()
        .env("JOB_AGENT_ENABLED", "true")
        .env("JOB_SUBMIT_MODE", mode)
        .env("BROWSER_WORKER_URL", &worker.uri())
        .env("BROWSER_WORKER_TOKEN", "worker-secret")
        .env("JOB_RESUME_PATH", &resume_path)
        .env("JOB_MAX_ATTEMPTS", "1");
    if telegram {
        b = b
            .env("TELEGRAM_ENABLED", "true")
            .env("TELEGRAM_BOT_TOKEN", "TESTTOKEN")
            .env("TELEGRAM_CHAT_ID", "777")
            .env("TELEGRAM_API_BASE", &tg.uri())
            .env("TELEGRAM_POLL_TIMEOUT_SECS", "1");
    }
    let agent = b.start().await;
    Env { worker, tg, agent }
}

fn alert_email() -> Value {
    email(
        "acct1",
        "<alert-1@linkedin.com>",
        "LinkedIn Job Alerts <jobalerts-noreply@linkedin.com>",
        "Staff AI Engineer at Acme",
        &format!("Your job alert for Staff AI Engineer\nApply now: {JOB_URL}?gh_src=linkedin"),
    )
}

async fn application(env: &Env) -> jdb::ApplicationRecord {
    wait_for("application", T, || async {
        env.agent
            .app
            .db
            .call(|c| jdb::list_applications(c, None, 10))
            .await
            .ok()?
            .into_iter()
            .next()
    })
    .await
}

async fn wait_status(env: &Env, want: &str) -> jdb::ApplicationRecord {
    let want = want.to_string();
    wait_for(&format!("application status {want}"), T, || {
        let w = want.clone();
        async move {
            let apps = env
                .agent
                .app
                .db
                .call(|c| jdb::list_applications(c, None, 10))
                .await
                .ok()?;
            apps.into_iter().find(|a| a.status == w)
        }
    })
    .await
}

async fn telegram_texts(tg: &MockServer) -> Vec<String> {
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
async fn auto_mode_detects_scores_fills_uploads_and_submits() {
    let env = setup("auto", json!([]), true).await;
    Mock::given(method("POST"))
        .and(path("/v1/apply/fill"))
        .and(body_partial_json(json!({"submit": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "submitted", "final_url": "https://boards.greenhouse.io/acme/jobs/4012345/confirmation",
            "missing_required": [], "blocker": null, "confirmation_text": "Thank you for applying!",
            "screenshot_path": "data/screenshots/x.png"
        })))
        .expect(1)
        .mount(&env.worker)
        .await;

    assert_eq!(
        env.agent
            .post("/api/v1/emails", &alert_email())
            .await
            .status(),
        202
    );
    let a = wait_status(&env, "submitted").await;
    assert_eq!(a.company.as_deref(), Some("Acme"));
    assert_eq!(a.role.as_deref(), Some("Staff AI Engineer"));
    assert!(a.resume_uploaded);
    assert!(a.applied_at.is_some());
    assert_eq!(a.answers["first_name"], "Asha");
    assert_eq!(a.answers["q_auth"], "Yes");
    assert!(
        a.answers["cover_letter"].as_str().unwrap().len() > 200,
        "cover letter generated"
    );

    // The worker received the resume path and the submit flag.
    let fill = env
        .worker
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.url.path() == "/v1/apply/fill")
        .unwrap();
    let body: Value = serde_json::from_slice(&fill.body).unwrap();
    assert!(body["files"]["resume"]
        .as_str()
        .unwrap()
        .ends_with("resume.pdf"));
    assert_eq!(body["session_key"], a.id);

    let job = {
        let jid = a.job_id.clone();
        env.agent
            .app
            .db
            .call(move |c| jdb::get_job(c, &jid))
            .await
            .unwrap()
    };
    assert!(job.score.unwrap() >= 80, "score {:?}", job.score);
    assert_eq!(job.status, "applied");
    assert_eq!(
        job.details.url.as_deref(),
        Some(JOB_URL),
        "tracking params stripped"
    );

    let texts = telegram_texts(&env.tg).await;
    assert!(texts.iter().any(|t| t.contains("Job Found")), "{texts:?}");
    assert!(
        texts
            .iter()
            .any(|t| t.contains("Application Submitted") && t.contains("Acme")),
        "{texts:?}"
    );

    let m = env.agent.metrics().await;
    assert_eq!(metric_sum(&m, "applications_started_total"), 1.0);
    assert_eq!(metric_sum(&m, "applications_completed_total"), 1.0);
    assert!(metric_sum(&m, "telegram_notifications_total") >= 2.0);

    let audit = env
        .agent
        .get_json(&format!("/api/v1/audit?entity_id={}", a.id))
        .await;
    assert!(
        audit.to_string().contains("application_submitted"),
        "submission logged"
    );
}

#[tokio::test]
async fn unknown_question_escalates_and_resumes_after_telegram_reply() {
    let env = setup(
        "auto",
        json!([{"id":"q_color","label":"What is your favourite programming paradigm?","type":"text","required":true,"options":[]}]),
        true,
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/v1/apply/fill"))
        .and(body_partial_json(
            json!({"submit": true, "answers": {"q_color": "Functional"}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "submitted", "final_url": "x", "missing_required": [], "blocker": null,
            "confirmation_text": "Application received"
        })))
        .mount(&env.worker)
        .await;

    env.agent.post("/api/v1/emails", &alert_email()).await;
    let a = wait_status(&env, "awaiting_human").await;
    assert_eq!(a.blocker_kind.as_deref(), Some("unknown_question"));
    let q = {
        let id = a.id.clone();
        env.agent
            .app
            .db
            .call(move |c| jdb::open_questions(c, Some(&id)))
            .await
            .unwrap()
    };
    assert_eq!(q.len(), 1);
    assert_eq!(
        q[0].telegram_message_id,
        Some(4242),
        "question message id stored"
    );
    let texts = telegram_texts(&env.tg).await;
    let blocked = texts
        .iter()
        .find(|t| t.contains("Application Blocked"))
        .expect("blocked message");
    assert!(blocked.contains("Company: Acme") && blocked.contains("Role: Staff AI Engineer"));
    assert!(blocked.contains("Question: What is your favourite programming paradigm?"));
    assert!(blocked.contains("Reason: unknown_question"));

    // Simulate the user replying to the bot's message.
    let reply = Message {
        message_id: 99,
        chat: Chat {
            id: 777,
            ..Default::default()
        },
        text: Some("Functional".into()),
        reply_to_message: Some(Box::new(Message {
            message_id: 4242,
            chat: Chat {
                id: 777,
                ..Default::default()
            },
            text: None,
            reply_to_message: None,
        })),
    };
    let out = emailhelper::telegram::bot::handle_message(&env.agent.app, &reply)
        .await
        .unwrap()
        .unwrap();
    assert!(out.contains("resuming"), "{out}");

    let a = wait_status(&env, "submitted").await;
    assert_eq!(a.answers["q_color"], "Functional");
    // The answer is remembered for next time.
    let mem = env
        .agent
        .app
        .db
        .call(|c| jdb::recall_answer(c, "What is your favourite programming paradigm?"))
        .await
        .unwrap();
    assert_eq!(mem.as_deref(), Some("Functional"));
}

#[tokio::test]
async fn captcha_blocks_and_never_submits_until_human_clears_it() {
    let env = setup("auto", json!([]), false).await;
    Mock::given(method("POST"))
        .and(path("/v1/apply/fill"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "blocked", "final_url": "x", "missing_required": [],
            "blocker": {"kind": "captcha", "detail": "recaptcha iframe"}
        })))
        .up_to_n_times(1)
        .mount(&env.worker)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/apply/fill"))
        .and(body_partial_json(json!({"submit": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "submitted", "final_url": "x", "missing_required": [], "blocker": null
        })))
        .mount(&env.worker)
        .await;

    env.agent.post("/api/v1/emails", &alert_email()).await;
    let a = wait_status(&env, "blocked").await;
    assert_eq!(a.blocker_kind.as_deref(), Some("captcha"));
    assert!(a.applied_at.is_none());
    assert!(metric_sum(&env.agent.metrics().await, "applications_blocked_total") >= 1.0);

    // Answer via the REST API (same path as Telegram).
    let q = {
        let id = a.id.clone();
        env.agent
            .app
            .db
            .call(move |c| jdb::open_questions(c, Some(&id)))
            .await
            .unwrap()
    };
    let r = env
        .agent
        .post(
            &format!("/api/v1/applications/{}/answer", a.id),
            &json!({"question_id": q[0].id, "answer": "done"}),
        )
        .await;
    assert_eq!(r.status(), 200);
    wait_status(&env, "submitted").await;
}

#[tokio::test]
async fn review_mode_fills_then_waits_for_approval() {
    let env = setup("review", json!([]), false).await;
    Mock::given(method("POST"))
        .and(path("/v1/apply/fill"))
        .and(body_partial_json(json!({"submit": false})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "filled", "final_url": "x", "missing_required": [], "blocker": null
        })))
        .mount(&env.worker)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/apply/fill"))
        .and(body_partial_json(json!({"submit": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "submitted", "final_url": "x", "missing_required": [], "blocker": null
        })))
        .mount(&env.worker)
        .await;

    env.agent.post("/api/v1/emails", &alert_email()).await;
    let a = wait_status(&env, "awaiting_approval").await;
    assert!(
        a.applied_at.is_none(),
        "review mode never submits without approval"
    );
    let q = {
        let id = a.id.clone();
        env.agent
            .app
            .db
            .call(move |c| jdb::open_questions(c, Some(&id)))
            .await
            .unwrap()
    };
    assert_eq!(q[0].field_id.as_deref(), Some("__approve__"));

    // An unclear reply is not consent.
    let msg = |text: &str| Message {
        message_id: 1,
        chat: Chat {
            id: 777,
            ..Default::default()
        },
        text: Some(text.into()),
        reply_to_message: None,
    };
    let out = emailhelper::telegram::bot::handle_message(
        &env.agent.app,
        &msg(&format!("{} maybe later", q[0].ref_code)),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(out.contains("submit"), "{out}");
    assert_eq!(application(&env).await.status, "awaiting_approval");

    let open = env
        .agent
        .app
        .db
        .call(|c| jdb::open_questions(c, None))
        .await
        .unwrap();
    let out = emailhelper::telegram::bot::handle_message(
        &env.agent.app,
        &msg(&format!("{} submit", open[0].ref_code)),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(out.contains("resuming"), "{out}");
    wait_status(&env, "submitted").await;
}

#[tokio::test]
async fn skip_reply_abandons_application() {
    let env = setup("review", json!([]), false).await;
    Mock::given(method("POST"))
        .and(path("/v1/apply/fill"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "filled", "final_url": "x", "missing_required": [], "blocker": null
        })))
        .mount(&env.worker)
        .await;
    env.agent.post("/api/v1/emails", &alert_email()).await;
    wait_status(&env, "awaiting_approval").await;
    let msg = Message {
        message_id: 1,
        chat: Chat {
            id: 777,
            ..Default::default()
        },
        text: Some("skip".into()),
        reply_to_message: None,
    };
    let out = emailhelper::telegram::bot::handle_message(&env.agent.app, &msg)
        .await
        .unwrap()
        .unwrap();
    assert!(out.starts_with("Skipped"), "{out}");
    let a = wait_status(&env, "skipped").await;
    let job = env
        .agent
        .app
        .db
        .call(move |c| jdb::get_job(c, &a.job_id))
        .await
        .unwrap();
    assert_eq!(job.status, "skipped");
}

#[tokio::test]
async fn missing_resume_file_blocks_with_upload_failure() {
    let env = setup("auto", json!([]), false).await;
    // Point the resume at a file that doesn't exist by deleting it.
    let p = env.agent.app.cfg.jobs.resume_path.clone();
    std::fs::remove_file(&p).unwrap();
    env.agent.post("/api/v1/emails", &alert_email()).await;
    let a = wait_status(&env, "blocked").await;
    assert_eq!(a.blocker_kind.as_deref(), Some("upload_failure"));
    let reqs = env.worker.received_requests().await.unwrap();
    assert!(
        !reqs.iter().any(|r| r.url.path() == "/v1/apply/fill"),
        "never fills without the resume"
    );
}

#[tokio::test]
async fn application_state_survives_restart() {
    let env = setup("review", json!([]), false).await;
    Mock::given(method("POST"))
        .and(path("/v1/apply/fill"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "filled", "final_url": "x", "missing_required": [], "blocker": null
        })))
        .mount(&env.worker)
        .await;
    env.agent.post("/api/v1/emails", &alert_email()).await;
    let before = wait_status(&env, "awaiting_approval").await;

    // Restart the agent on the same data directory and worker.
    let worker_uri = env.worker.uri();
    let resume = env
        .agent
        .app
        .cfg
        .jobs
        .resume_path
        .to_string_lossy()
        .to_string();
    let Env {
        worker,
        tg: _tg,
        agent,
    } = env;
    let dir = {
        let mut agent = agent;
        std::mem::replace(&mut agent.dir, tempfile::tempdir().unwrap())
    };
    let agent = builder()
        .dir(dir)
        .env("JOB_AGENT_ENABLED", "true")
        .env("JOB_SUBMIT_MODE", "review")
        .env("BROWSER_WORKER_URL", &worker_uri)
        .env("BROWSER_WORKER_TOKEN", "worker-secret")
        .env("JOB_RESUME_PATH", &resume)
        .start()
        .await;
    let after = {
        let id = before.id.clone();
        agent
            .app
            .db
            .call(move |c| jdb::get_application(c, &id))
            .await
            .unwrap()
    };
    assert_eq!(after.status, "awaiting_approval");
    assert_eq!(after.answers, before.answers, "partial answers persisted");
    assert_eq!(after.cover_letter, before.cover_letter);

    Mock::given(method("POST"))
        .and(path("/v1/apply/fill"))
        .and(body_partial_json(json!({"submit": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "submitted", "final_url": "x", "missing_required": [], "blocker": null
        })))
        .with_priority(1)
        .mount(&worker)
        .await;
    let q = agent
        .app
        .db
        .call(|c| jdb::open_questions(c, None))
        .await
        .unwrap();
    let r = agent
        .post(
            &format!("/api/v1/applications/{}/answer", before.id),
            &json!({"question_id": q[0].id, "answer": "submit"}),
        )
        .await;
    assert_eq!(r.status(), 200);
    wait_for("submitted after restart", T, || async {
        let id = before.id.clone();
        let a = agent
            .app
            .db
            .call(move |c| jdb::get_application(c, &id))
            .await
            .ok()?;
        (a.status == "submitted").then_some(())
    })
    .await;
}

#[tokio::test]
async fn low_score_jobs_are_skipped_without_applying() {
    let env = setup("auto", json!([]), false).await;
    let e = email(
        "acct1",
        "<acct@x>",
        "Jobs <jobs-noreply@naukri.com>",
        "Senior Accountant at Initech",
        "Recommended jobs for you: Senior Accountant https://www.naukri.com/job-listings-senior-accountant-initech-123",
    );
    env.agent.post("/api/v1/emails", &e).await;
    let job = wait_for("job stored", T, || async {
        env.agent
            .app
            .db
            .call(|c| jdb::list_jobs(c, 10, None))
            .await
            .ok()?
            .into_iter()
            .next()
    })
    .await;
    assert_eq!(job.decision.as_deref(), Some("skip"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let apps = env
        .agent
        .app
        .db
        .call(|c| jdb::list_applications(c, None, 10))
        .await
        .unwrap();
    assert!(apps.is_empty(), "no application for skipped job");
    let reqs = env.worker.received_requests().await.unwrap();
    assert!(
        reqs.iter().all(|r| r.url.path() != "/v1/extract"),
        "Naukri pages are never opened by the agent"
    );
}

#[tokio::test]
async fn job_can_be_submitted_by_url_and_applied() {
    let env = setup("auto", json!([]), false).await;
    Mock::given(method("POST"))
        .and(path("/v1/apply/fill"))
        .and(body_partial_json(json!({"submit": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "submitted", "final_url": "x", "missing_required": [], "blocker": null
        })))
        .mount(&env.worker)
        .await;
    let bad = env
        .agent
        .post("/api/v1/jobs", &json!({"url": "not a url"}))
        .await;
    assert_eq!(bad.status(), 422);
    let r = env
        .agent
        .post("/api/v1/jobs", &json!({"url": JOB_URL, "apply": true}))
        .await;
    assert_eq!(r.status(), 201);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["job"]["details"]["title"], "Staff AI Engineer");
    assert_eq!(v["decision"], "apply");
    assert!(v["application_id"].is_string());
    wait_status(&env, "submitted").await;
    // Same URL again: existing job, no duplicate.
    let again = env
        .agent
        .post("/api/v1/jobs", &json!({"url": JOB_URL}))
        .await;
    assert_eq!(again.status(), 200);
}

#[test]
fn sample_profile_is_refused() {
    use emailhelper::jobs::application::profile_problem;
    let sample = emailhelper::knowledge::Profile::load(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../knowledge"),
    );
    if sample.identity.email.ends_with("@example.com") {
        assert!(profile_problem(&sample).is_some());
    }
    let fixture = emailhelper::knowledge::Profile::load(&knowledge_dir());
    assert!(profile_problem(&fixture).is_none());
    assert!(profile_problem(&emailhelper::knowledge::Profile::default()).is_some());
}

#[tokio::test]
async fn old_job_alerts_are_counted_but_never_browsed() {
    let env = setup("auto", json!([]), false).await;
    let mut e = alert_email();
    e["message_id"] = json!("<old-alert@linkedin.com>");
    e["date"] = json!("2021-05-01T08:00:00Z");
    let r: Value = env
        .agent
        .post("/api/v1/emails", &e)
        .await
        .json()
        .await
        .unwrap();
    let id = r["email_id"].as_str().unwrap().to_string();
    wait_for("old alert processed", T, || async {
        (email_status(&env.agent.app, &id).await == "done").then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let reqs = env.worker.received_requests().await.unwrap();
    assert!(
        reqs.iter().all(|r| r.url.path() == "/health"),
        "worker must not be used for stale mail"
    );
    let apps = env
        .agent
        .app
        .db
        .call(|c| jdb::list_applications(c, None, 10))
        .await
        .unwrap();
    assert!(apps.is_empty());
    assert!(env
        .agent
        .metrics()
        .await
        .contains("jobs_detected_total{source=\"linkedin\"} 1"));
}

#[tokio::test]
async fn job_links_mode_queues_every_link_instead_of_applying() {
    let worker = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status":"ok"})))
        .mount(&worker)
        .await;
    let links = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/links"))
        .and(header("x-joblinks-token", "links-secret-0123456789"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "created": 3, "updated": 0, "failed": 0, "results": []
        })))
        .expect(1)
        .mount(&links)
        .await;
    let agent = builder()
        .env("JOB_AGENT_ENABLED", "true")
        .env("JOB_SUBMIT_MODE", "auto")
        .env("BROWSER_WORKER_URL", &worker.uri())
        .env("JOB_MAX_ATTEMPTS", "1")
        .env("JOB_LINKS_ENABLED", "true")
        .env("JOB_LINKS_URL", &links.uri())
        .env("JOB_LINKS_TOKEN", "links-secret-0123456789")
        .start()
        .await;

    let body = "Your job alert for Technical Lead\n\
Lead AI Engineer\nNetrolynx AI\nUnited States\n\
View job: https://www.linkedin.com/comm/jobs/view/4480916162/?trackingId=abc\n\
---------------------------------------------------------\n\
AI Engineering Lead\nBeni Health\nRemote\n\
View job: https://www.linkedin.com/comm/jobs/view/4479947091/?trackingId=def\n";
    let lever = "https://jobs.lever.co/acme/1b2c-3d";
    let html = format!(
        r#"<p><a href="https://eur01.safelinks.protection.outlook.com/?url={}&amp;data=x">Staff Rust Engineer</a></p>"#,
        url::form_urlencoded::byte_serialize(lever.as_bytes()).collect::<String>()
    );
    let mut e = email(
        "acct1",
        "<alert-links@linkedin.com>",
        "LinkedIn Job Alerts <jobalerts-noreply@linkedin.com>",
        "Lead AI Engineer at Netrolynx AI and 1 more job",
        body,
    );
    e["body_html"] = json!(html);
    agent.post("/api/v1/emails", &e).await;

    let sent: Value = wait_for("links posted", T, || async {
        let reqs = links.received_requests().await?;
        reqs.first().and_then(|r| serde_json::from_slice(&r.body).ok())
    })
    .await;
    let queued = sent["links"].as_array().unwrap();
    assert_eq!(queued.len(), 3, "{sent}");
    let by_url = |u: &str| queued.iter().find(|l| l["url"] == u).unwrap_or_else(|| panic!("{u} not in {sent}"));
    let first = by_url("https://www.linkedin.com/jobs/view/4480916162/");
    assert_eq!(first["title"], "Lead AI Engineer");
    assert_eq!(first["company"], "Netrolynx AI");
    assert_eq!(first["platform"], "linkedin");
    assert_eq!(first["source"], "email");
    assert_eq!(first["mailbox"], "acct1@example.com");
    assert!(first["source_detail"].is_string(), "the email id");
    assert!(first["score"].as_u64().is_some_and(|s| s <= 100), "{first}");
    assert!(["apply", "review", "skip"].contains(&first["decision"].as_str().unwrap()));
    assert_eq!(by_url("https://www.linkedin.com/jobs/view/4479947091/")["company"], "Beni Health");
    let ats = by_url(lever);
    assert_eq!(ats["platform"], "lever");
    assert_eq!(ats["title"], "Staff Rust Engineer");

    tokio::time::sleep(Duration::from_millis(300)).await;
    let reqs = worker.received_requests().await.unwrap();
    assert!(
        reqs.iter().all(|r| r.url.path() == "/health"),
        "the browser worker is not used in job-links mode"
    );
    let apps = agent
        .app
        .db
        .call(|c| jdb::list_applications(c, None, 10))
        .await
        .unwrap();
    assert!(apps.is_empty());
}

#[tokio::test]
async fn extension_gets_form_answers_and_applies_nothing() {
    let agent = builder().start().await;
    let field = |id: &str, label: &str, kind: &str, required: bool| {
        json!({"id": id, "label": label, "type": kind, "required": required, "options": []})
    };
    let body = json!({
        "job": {"title": "Staff AI Engineer", "company": "Acme"},
        "fields": [
            field("email", "Email", "email", true),
            field("resume", "Resume/CV", "file", true),
            field("terms", "I agree to the Terms of Service and Privacy Policy", "checkbox", true),
            field("cover", "Cover Letter", "textarea", false),
        ]
    });
    // the Chrome extension's service worker sends its own origin
    let r = agent
        .http
        .post(agent.url("/api/v1/apply/answers"))
        .header("x-emailhelper-token", TOKEN)
        .header("origin", "chrome-extension://abcdefghijklmnop")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    let answer = |id: &str| v["answers"].as_array().unwrap().iter().find(|a| a["id"] == id).cloned().unwrap();
    assert_eq!(answer("email"), json!({"kind": "value", "id": "email", "value": "asha.rao@asharao.test"}));
    assert_eq!(answer("resume")["kind"], "resume");
    assert_eq!(answer("terms")["kind"], "ask", "legal agreements stay with the person");
    assert_eq!(answer("cover")["kind"], "value");
    assert!(answer("cover")["value"].as_str().is_some_and(|s| !s.is_empty()));

    // What the person answers on a form is reused next time, except sensitive questions.
    let r = agent
        .post(
            "/api/v1/apply/remember",
            &json!({"answers": [
                {"label": "Which conference talk inspired you most?", "value": "tokio-console"},
                {"label": "What is your expected salary?", "value": "60 LPA"},
                {"label": "What is your gender?", "value": "Female"}
            ]}),
        )
        .await;
    assert_eq!(r.json::<Value>().await.unwrap()["remembered"], 1);
    let again: Value = agent
        .post(
            "/api/v1/apply/answers",
            &json!({"fields": [field("oss", "Which conference talk inspired you most?", "text", true)]}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(again["answers"][0], json!({"kind": "value", "id": "oss", "value": "tokio-console"}));

    let unknown = agent.post("/api/v1/apply/answers", &json!({"person": "nobody", "fields": []})).await;
    assert_eq!(unknown.status(), 404);
    let foreign = agent
        .http
        .post(agent.url("/api/v1/apply/answers"))
        .header("x-emailhelper-token", TOKEN)
        .header("origin", "https://evil.example")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), 401);
}

#[tokio::test]
async fn extension_failures_are_stored_and_summarised() {
    let agent = builder().start().await;
    let fill = |url: &str| {
        json!({"kind": "fill_failed", "url": url, "platform": "greenhouse", "field": "Phone", "reason": "no input found"})
    };
    let r = agent
        .post(
            "/api/v1/apply/events",
            &json!({"events": [
                fill("https://x.test/1"), fill("https://x.test/2"), fill("https://x.test/3"),
                {"kind": "blocked", "url": "https://y.test/1", "platform": "lever", "reason": "captcha", "detail": "x".repeat(10_000)},
                {"kind": "applied", "url": "https://y.test/2", "platform": "lever", "reason": "submitted"}
            ]}),
        )
        .await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["stored"], 5);

    let s = agent.get_json("/api/v1/apply/events/summary?days=7").await;
    assert_eq!(s["days"], 7);
    assert_eq!(s["by_kind"], json!({"applied": 1, "blocked": 1, "fill_failed": 3}));
    let top = s["top"].as_array().unwrap();
    assert_eq!(top.len(), 2, "applied is not a problem: {top:?}");
    assert_eq!(
        (&top[0]["platform"], &top[0]["kind"], &top[0]["field"], &top[0]["reason"], &top[0]["count"]),
        (&json!("greenhouse"), &json!("fill_failed"), &json!("Phone"), &json!("no input found"), &json!(3))
    );
    assert!(top[0]["last_url"].as_str().is_some_and(|u| u.starts_with("https://x.test/")));
    assert_eq!(top[1]["field"], Value::Null);
    assert_eq!(metric_sum(&agent.metrics().await, "apply_events_total"), 5.0);

    let too_many: Vec<Value> = (0..201).map(|i| fill(&format!("https://x.test/{i}"))).collect();
    let r = agent.post("/api/v1/apply/events", &json!({ "events": too_many })).await;
    assert_eq!(r.status(), 422);
}

#[tokio::test]
async fn health_reports_optional_services_and_degrades_when_one_is_down() {
    let off = builder().start().await;
    let h: Value = off.http.get(off.url("/health")).send().await.unwrap().json().await.unwrap();
    assert_eq!((h["status"].as_str(), h["checks"]["knowledge_service"].as_str(), h["checks"]["job_links"].as_str()), (Some("ok"), Some("off"), Some("off")));

    let links = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/livez"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&links)
        .await;
    let down = builder().env("JOB_LINKS_ENABLED", "true").env("JOB_LINKS_URL", &links.uri()).start().await;
    let h: Value = down.http.get(down.url("/health")).send().await.unwrap().json().await.unwrap();
    assert_eq!((h["status"].as_str(), h["checks"]["job_links"].as_str()), (Some("degraded"), Some("down")), "{h}");
}
