//! Real end-to-end test: Rust agent → real Playwright browser worker → mock
//! Greenhouse / Lever / career-portal / CAPTCHA sites.
//!
//! Skipped unless `EH_E2E_WORKER_URL` and `EH_E2E_MOCK_URL` are set; run via
//! `scripts/e2e-browser.ps1` or `scripts/e2e-browser.sh`, which start the
//! mock sites and the worker first.

mod common;

use std::time::Duration;

use common::*;
use emailhelper::db::jobs as jdb;
use serde_json::{json, Value};

struct E2e {
    worker: String,
    mock: String,
    token: String,
    resume: String,
}

fn e2e_env() -> Option<E2e> {
    let worker = std::env::var("EH_E2E_WORKER_URL").ok()?;
    let mock = std::env::var("EH_E2E_MOCK_URL").ok()?;
    Some(E2e {
        worker,
        mock: mock.trim_end_matches('/').to_string(),
        token: std::env::var("EH_E2E_WORKER_TOKEN").unwrap_or_else(|_| "e2e-worker-token".into()),
        resume: std::env::var("EH_E2E_RESUME").unwrap_or_else(|_| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../tests/mock-sites/fixtures/resume.pdf")
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .to_string()
        }),
    })
}

async fn agent(e: &E2e) -> TestAgent {
    builder()
        .env("JOB_AGENT_ENABLED", "true")
        .env("JOB_SUBMIT_MODE", "auto")
        // Every job counts as "apply" so the browser path is always exercised.
        .env("JOB_AUTO_APPLY_THRESHOLD", "0")
        .env("JOB_REVIEW_THRESHOLD", "0")
        .env("BROWSER_WORKER_URL", &e.worker)
        .env("BROWSER_WORKER_TOKEN", &e.token)
        .env("BROWSER_WORKER_TIMEOUT_SECS", "120")
        .env("JOB_RESUME_PATH", &e.resume)
        .env("JOB_MAX_ATTEMPTS", "1")
        .start()
        .await
}

/// Open the posting through the worker, store it, start the application and
/// drive it to a terminal state, answering any human questions generically.
async fn apply(a: &TestAgent, url: &str) -> (jdb::ApplicationRecord, Vec<String>) {
    let page = emailhelper::jobs::fetch_page(&a.app, url)
        .await
        .expect("extract via worker");
    let mut d =
        emailhelper::jobs::extract::parse_job(&page, &a.app.knowledge.profile().all_skills());
    d.url = Some(url.to_string());
    if d.title.is_empty() {
        d.title = "Staff AI Engineer".into();
    }
    let (job, _, _) = emailhelper::jobs::score_and_store(&a.app, "e2e", None, d)
        .await
        .unwrap();
    let id = emailhelper::jobs::application::start(&a.app, &job, url)
        .await
        .unwrap();
    let mut asked = Vec::new();
    for _round in 0..6 {
        let app_rec = wait_for(
            "terminal application state",
            Duration::from_secs(150),
            || {
                let id = id.clone();
                async move {
                    let r = a
                        .app
                        .db
                        .call(move |c| jdb::get_application(c, &id))
                        .await
                        .ok()?;
                    [
                        "submitted",
                        "awaiting_human",
                        "blocked",
                        "failed",
                        "awaiting_approval",
                        "ready",
                    ]
                    .contains(&r.status.as_str())
                    .then_some(r)
                }
            },
        )
        .await;
        if app_rec.status == "awaiting_approval" {
            // A job the matcher didn't rate "apply" (e.g. a hard rule fired):
            // approve it the way a user would on Telegram.
            asked.push("approval".into());
        } else if app_rec.status != "awaiting_human" {
            return (app_rec, asked);
        }
        let id2 = id.clone();
        let open = a
            .app
            .db
            .call(move |c| jdb::open_questions(c, Some(&id2)))
            .await
            .unwrap();
        for q in open {
            asked.push(q.question.clone());
            let answer = if q.field_id.as_deref() == Some("__approve__") {
                "submit".to_string()
            } else {
                q.options
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "Happy to discuss".into())
            };
            let r = a
                .post(
                    &format!("/api/v1/applications/{id}/answer"),
                    &json!({"question_id": q.id, "answer": answer}),
                )
                .await;
            assert_eq!(r.status(), 200);
        }
    }
    panic!("application did not settle");
}

async fn submissions(e: &E2e) -> Vec<Value> {
    let v: Value = reqwest::get(format!("{}/__submissions", e.mock))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    v["submissions"].as_array().cloned().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_browser_applications_against_mock_ats_sites() {
    let Some(e) = e2e_env() else {
        eprintln!(
            "skipping: set EH_E2E_WORKER_URL and EH_E2E_MOCK_URL (see scripts/e2e-browser.*)"
        );
        return;
    };
    let _ = reqwest::Client::new()
        .post(format!("{}/__reset", e.mock))
        .send()
        .await;
    let a = agent(&e).await;
    let h: Value = a
        .http
        .get(a.url("/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        h["checks"]["browser_worker"], "ok",
        "worker must be reachable: {h}"
    );

    // 1. Greenhouse: JSON-LD extraction, Apply-button flow, profile answers,
    //    LLM free-text answer, resume upload, submit, confirmation.
    let gh_url = format!("{}/greenhouse/job.html", e.mock);
    let (gh, asked) = apply(&a, &gh_url).await;
    println!(
        "greenhouse: status={} step={:?} asked={asked:?} answers={}",
        gh.status,
        gh.current_step,
        Value::Object(gh.answers.clone())
    );
    assert_eq!(
        gh.status, "submitted",
        "greenhouse blocker={:?} {:?}",
        gh.blocker_kind, gh.blocker_detail
    );
    assert!(gh.resume_uploaded);
    assert!(
        gh.confirmation_text
            .as_deref()
            .unwrap_or("")
            .to_lowercase()
            .contains("thank"),
        "{:?}",
        gh.confirmation_text
    );
    assert!(gh.screenshot_path.is_some());

    // 2. Lever-style form.
    let lv_url = format!("{}/lever/apply.html", e.mock);
    let (lv, asked) = apply(&a, &lv_url).await;
    println!("lever: status={} asked={asked:?}", lv.status);
    assert_eq!(
        lv.status, "submitted",
        "lever blocker={:?} {:?}",
        lv.blocker_kind, lv.blocker_detail
    );

    // 3. Generic career portal with "expected CTC" and notice period.
    let cp_url = format!("{}/career/apply.html", e.mock);
    let (cp, asked) = apply(&a, &cp_url).await;
    println!(
        "career: status={} asked={asked:?} answers={}",
        cp.status,
        Value::Object(cp.answers.clone())
    );
    assert_eq!(
        cp.status, "submitted",
        "career blocker={:?} {:?}",
        cp.blocker_kind, cp.blocker_detail
    );
    let ctc = cp
        .answers
        .iter()
        .find(|(_, v)| v.as_str() == Some("60 LPA"));
    assert!(
        ctc.is_some(),
        "expected CTC answered from salary_preferences.json"
    );

    // 4. CAPTCHA page: must block, never submit.
    let before = submissions(&e).await.len();
    let cap_url = format!("{}/blocked/captcha.html", e.mock);
    let (cap, _) = apply(&a, &cap_url).await;
    println!(
        "captcha: status={} blocker={:?}",
        cap.status, cap.blocker_kind
    );
    assert_eq!(cap.status, "blocked");
    assert_eq!(cap.blocker_kind.as_deref(), Some("captcha"));
    assert_eq!(
        submissions(&e).await.len(),
        before,
        "nothing submitted behind a CAPTCHA"
    );

    // The mock ATS recorded exactly the three real submissions with the resume.
    let subs = submissions(&e).await;
    println!(
        "submissions: {}",
        serde_json::to_string_pretty(&subs).unwrap()
    );
    assert_eq!(subs.len(), 3);
    let gh_sub = subs
        .iter()
        .find(|s| s["ats"] == "greenhouse")
        .expect("greenhouse submission");
    let blob = gh_sub.to_string();
    assert!(blob.contains("resume.pdf"), "resume uploaded: {blob}");
    assert!(blob.contains("Asha"), "first name submitted: {blob}");
    assert!(blob.contains("asha.rao@example.com"));

    let m = a.metrics().await;
    assert_eq!(metric_sum(&m, "applications_completed_total"), 3.0);
    assert!(metric_sum(&m, "applications_blocked_total") >= 1.0);
}
