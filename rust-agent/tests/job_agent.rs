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
        chat: Chat { id: 777 },
        text: Some("Functional".into()),
        reply_to_message: Some(Box::new(Message {
            message_id: 4242,
            chat: Chat { id: 777 },
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
        chat: Chat { id: 777 },
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
        chat: Chat { id: 777 },
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
    // The worker returns an accounting job for this URL.
    env.worker.reset().await;
    Mock::given(method("POST"))
        .and(path("/v1/extract"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "final_url": "https://www.naukri.com/job-listings-senior-accountant-initech-123",
            "title": "Senior Accountant - Initech", "text": "Onsite in Berlin. SAP, Excel, IFRS. 5 years.",
            "json_ld": [], "links": []
        })))
        .mount(&env.worker)
        .await;
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
}
