//! Multi-person routing: mailboxes → person + purpose, per-person persona and
//! knowledge, facts answered from the right person's files (private values
//! filled in after generation), fact questions, unassigned mailboxes.

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use emailhelper::llm::{LlmTask, MockProvider};
use serde_json::{json, Value};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const T: Duration = Duration::from_secs(20);

fn write(p: &Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

/// A household with two people and four mailboxes.
fn household() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let k = d.path();
    write(
        &k.join("people.json"),
        r#"{
          "people": [
            {"id": "shashi", "name": "Shashi Bhushan Kumar", "job_agent": false,
             "mailboxes": [
               {"address": "shashi.jobs@family.test", "purposes": ["job_seeking"]},
               {"address": "shashi.consult@family.test", "purposes": ["consulting"]}
             ]},
            {"id": "himanshu", "name": "Himanshu Kumar",
             "mailboxes": [{"address": "himanshu@family.test", "purposes": ["freelancing"]}]}
          ]
        }"#,
    );
    write(
        &k.join("people/shashi/profile.json"),
        r#"{"full_name": "Shashi Bhushan Kumar", "email": "shashi.jobs@family.test", "location": "Bengaluru, India", "current_company": "McKinsey & Company", "current_title": "Lead Backend Engineer"}"#,
    );
    write(
        &k.join("people/shashi/resume.json"),
        r#"{"total_experience_years": 12}"#,
    );
    write(
        &k.join("people/shashi/private.json"),
        r#"{"date_of_birth": "1990-02-03", "pan": "ABCDE1234F", "share": {"date_of_birth": "on_request"}}"#,
    );
    write(
        &k.join("people/shashi/job_seeking/salary.json"),
        r#"{"expected_text": "30 LPA", "notice_period": "60 days"}"#,
    );
    write(
        &k.join("people/shashi/job_seeking/resumes/python_cv.md"),
        "Python FastAPI AWS Kubernetes",
    );
    write(
        &k.join("people/shashi/job_seeking/resumes/dotnet_cv.md"),
        "C# .NET Azure SQL",
    );
    write(
        &k.join("people/shashi/job_seeking/replies/recruiting.md"),
        "# Recruiters\nShashi answers recruiters with his notice period and expected CTC.",
    );
    write(
        &k.join("people/shashi/consulting/rates.md"),
        "# Consulting rates\nShashi's architecture consulting rate is 9000 INR per hour.",
    );
    write(
        &k.join("people/himanshu/profile.json"),
        r#"{"full_name": "Himanshu Kumar", "email": "himanshu@family.test"}"#,
    );
    write(
        &k.join("people/himanshu/freelancing/rates.md"),
        "# Freelance rates\nHimanshu builds React websites at 2500 INR per hour.",
    );
    d
}

/// Mock LLM that records every draft prompt and echoes the DOB placeholder.
fn recording_llm() -> (MockProvider, Arc<Mutex<Vec<String>>>) {
    let prompts = Arc::new(Mutex::new(Vec::<String>::new()));
    let p2 = prompts.clone();
    let llm = MockProvider::with_handler(move |req| {
        if req.task != LlmTask::Draft {
            return None;
        }
        p2.lock()
            .unwrap()
            .push(format!("{}\n{}", req.system, req.prompt));
        let mut body =
            String::from("Thanks for reaching out. Happy to help with the details you asked for.");
        if req.prompt.contains("{{DATE_OF_BIRTH}}") {
            body.push_str(" My date of birth is {{DATE_OF_BIRTH}}.");
        }
        if req.prompt.contains("Notice period: 60 days") {
            body.push_str(" My notice period is 60 days and my expected CTC is 30 LPA.");
        }
        Some(Ok(body))
    });
    (llm, prompts)
}

async fn agent(k: &Path, llm: MockProvider, extra: &[(&str, &str)]) -> TestAgent {
    let mut b = builder()
        .llm(llm)
        .env("EH_KNOWLEDGE_DIR", &k.to_string_lossy());
    for (key, v) in extra {
        b = b.env(key, v);
    }
    b.start().await
}

fn mail(account_email: &str, msg: &str, from: &str, subject: &str, body: &str) -> Value {
    let mut e = email("acct", msg, from, subject, body);
    e["account_id"] = json!(format!("tb-{account_email}"));
    e["account_email"] = json!(account_email);
    e["to"] = json!([account_email]);
    e
}

#[tokio::test]
async fn each_person_gets_their_own_persona_mailbox_and_knowledge() {
    let k = household();
    let (llm, prompts) = recording_llm();
    let a = agent(k.path(), llm, &[]).await;
    a.post(
        "/api/v1/emails",
        &mail(
            "himanshu@family.test",
            "<h1@x>",
            "Client <client@shop.test>",
            "Website project",
            "Hi Himanshu, can you build our React website? What is your hourly rate?",
        ),
    )
    .await;
    let d = a.collect_drafts(1, T).await;
    assert_eq!(d.len(), 1);
    let body = d[0]["body"].as_str().unwrap();
    assert!(
        body.ends_with("Himanshu Kumar"),
        "signed as the receiving person: {body}"
    );
    assert!(!body.contains("Shashi"));
    assert_eq!(
        d[0]["from_address"], "himanshu@family.test",
        "reply goes out from the receiving mailbox"
    );
    assert!(d[0]["notes"]
        .to_string()
        .contains("Written as Himanshu Kumar from himanshu@family.test (freelancing)"));
    let p = prompts.lock().unwrap().join("\n");
    assert!(
        p.contains("2500 INR"),
        "Himanshu's freelancing knowledge used"
    );
    assert!(
        !p.contains("9000 INR") && !p.contains("Shashi"),
        "no other person's knowledge in the prompt"
    );
}

#[tokio::test]
async fn facts_are_answered_and_private_values_never_reach_the_model() {
    let k = household();
    let (llm, prompts) = recording_llm();
    let a = agent(k.path(), llm, &[]).await;
    a.post(
        "/api/v1/emails",
        &mail(
            "shashi.jobs@family.test",
            "<r1@x>",
            "Priya <priya@recruit.test>",
            "Python Lead role - details needed",
            "Hi Shashi, for the Python/FastAPI lead role please share your expected CTC, notice period, DOB, PAN card number and your updated resume.",
        ),
    )
    .await;
    let d = a.collect_drafts(1, T).await;
    let body = d[0]["body"].as_str().unwrap();
    assert!(
        body.contains("1990-02-03"),
        "DOB filled in after generation: {body}"
    );
    assert!(!body.contains("{{"), "no placeholders left");
    assert_eq!(
        d[0]["validation"], "REVIEW_REQUIRED",
        "private details always need review"
    );
    let notes = d[0]["notes"].to_string();
    assert!(
        notes.contains("Contains private details (Date of birth)"),
        "{notes}"
    );
    assert!(
        notes.contains("Attach resume python_cv.md"),
        "stack-matched resume: {notes}"
    );
    let p = prompts.lock().unwrap().join("\n");
    assert!(p.contains("Notice period: 60 days") && p.contains("Expected CTC: 30 LPA"));
    assert!(p.contains("{{DATE_OF_BIRTH}}"));
    assert!(p.contains("PAN: say you will share it over a secure channel"));
    assert!(
        !p.contains("1990-02-03") && !p.contains("ABCDE1234F"),
        "private values never in the prompt"
    );
}

#[tokio::test]
async fn unknown_facts_are_asked_once_and_reused() {
    let k = household();
    let (llm, prompts) = recording_llm();
    let a = agent(k.path(), llm, &[]).await;
    a.post(
        "/api/v1/emails",
        &mail(
            "shashi.jobs@family.test",
            "<c1@x>",
            "hr@acme.test",
            "Current CTC?",
            "Hi, could you share your current CTC?",
        ),
    )
    .await;
    let d = a.collect_drafts(1, T).await;
    assert!(d[0]["notes"]
        .to_string()
        .contains("Asked Shashi Bhushan Kumar for: Current CTC"));
    let open = a
        .app
        .db
        .call(|c| emailhelper::db::jobs::open_questions(c, None))
        .await
        .unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].field_id.as_deref(), Some("fact:shashi:current_ctc"));
    // Answer from the dashboard (same as a Telegram reply).
    let r = a
        .post(
            &format!("/api/v1/questions/{}/answer", open[0].id),
            &json!({"answer": "24 LPA"}),
        )
        .await;
    assert_eq!(r.status(), 200);
    let dash = a.get_json("/api/v1/dashboard").await;
    assert!(dash["questions"].as_array().unwrap().is_empty());
    a.post(
        "/api/v1/emails",
        &mail(
            "shashi.jobs@family.test",
            "<c2@x>",
            "hr@globex.test",
            "CTC details",
            "Please confirm your current CTC.",
        ),
    )
    .await;
    a.collect_drafts(1, T).await;
    let p = prompts.lock().unwrap();
    assert!(
        p.last().unwrap().contains("Current CTC: 24 LPA"),
        "stored answer reused"
    );
}

#[tokio::test]
async fn unassigned_mailboxes_are_classified_only() {
    let k = household();
    let (llm, _) = recording_llm();
    let a = agent(k.path(), llm, &[]).await;
    let r: Value = a
        .post(
            "/api/v1/emails",
            &mail(
                "cousin@family.test",
                "<u1@x>",
                "bob@acme.test",
                "Question",
                "Can you send me the report?",
            ),
        )
        .await
        .json()
        .await
        .unwrap();
    let id = r["email_id"].as_str().unwrap().to_string();
    wait_for("processed", T, || async {
        (email_status(&a.app, &id).await == "done").then_some(())
    })
    .await;
    assert!(a.get_json("/api/v1/drafts/pending?wait=0").await["drafts"]
        .as_array()
        .unwrap()
        .is_empty());
    let dash = a.get_json("/api/v1/dashboard").await;
    assert_eq!(
        dash["unassigned_mailboxes"][0]["mailbox"],
        "cousin@family.test"
    );
}

#[tokio::test]
async fn one_bot_tags_messages_with_the_person() {
    let k = household();
    let tg = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/botTESTTOKEN/sendMessage$"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": true, "result": {"message_id": 5}})),
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
    let (llm, _) = recording_llm();
    let uri = tg.uri();
    let a = agent(
        k.path(),
        llm,
        &[
            ("TELEGRAM_ENABLED", "true"),
            ("TELEGRAM_BOT_TOKEN", "TESTTOKEN"),
            ("TELEGRAM_CHAT_ID", "42"),
            ("TELEGRAM_API_BASE", &uri),
        ],
    )
    .await;
    a.post(
        "/api/v1/emails",
        &mail(
            "himanshu@family.test",
            "<t1@x>",
            "client@shop.test",
            "Landing page",
            "Could you build a landing page for us?",
        ),
    )
    .await;
    a.collect_drafts(1, T).await;
    let texts = wait_for("telegram", T, || async {
        let reqs = tg.received_requests().await.unwrap_or_default();
        let t: Vec<Value> = reqs
            .iter()
            .filter(|r| r.url.path().ends_with("/sendMessage"))
            .filter_map(|r| serde_json::from_slice(&r.body).ok())
            .collect();
        (!t.is_empty()).then_some(t)
    })
    .await;
    let first = &texts[0];
    assert_eq!(first["chat_id"], 42, "single bot, default chat");
    assert!(
        first["text"]
            .as_str()
            .unwrap()
            .starts_with("[Himanshu Kumar · himanshu@family.test]"),
        "{first}"
    );
}
