//! HTTP tests: the router driven in-process with `tower::ServiceExt::oneshot`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use job_links::api::{router, AppState, TOKEN_HEADER};
use job_links::store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

const TOKEN: &str = "test-token-0123456789";

fn app_with_snapshot(snapshot: PathBuf) -> (Router, Arc<Store>) {
    let store = Arc::new(Store::new());
    let r = router(AppState {
        store: store.clone(),
        token: TOKEN.into(),
        default_lease: Duration::from_secs(600),
        default_max_age_hours: Some(336),
        snapshot_path: Arc::new(snapshot),
    });
    (r, store)
}

fn app() -> (Router, Arc<Store>) {
    app_with_snapshot(std::env::temp_dir().join("job-links-http-test").join("links.json"))
}

async fn call(app: &Router, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path).header(TOKEN_HEADER, TOKEN);
    let body = match body {
        Some(v) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()))
    };
    (status, v)
}

#[tokio::test]
async fn health_is_open_and_v1_needs_the_token() {
    let (app, _) = app();
    let resp = app
        .clone()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    for token in [None, Some("wrong-token-0123456789")] {
        let mut req = Request::get("/v1/stats");
        if let Some(t) = token {
            req = req.header(TOKEN_HEADER, t);
        }
        let resp = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
    let (status, _) = call(&app, Method::GET, "/v1/stats", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn probes() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _) = app_with_snapshot(dir.path().join("data").join("links.json"));
    for path in ["/livez", "/readyz"] {
        let resp = app.clone().oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
    }
    // A snapshot path under a regular file can't be written.
    let file = dir.path().join("plain-file");
    std::fs::write(&file, b"x").unwrap();
    let (app, _) = app_with_snapshot(file.join("links.json"));
    let resp = app.clone().oneshot(Request::get("/readyz").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let resp = app.oneshot(Request::get("/livez").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn create_single_then_upsert() {
    let (app, _) = app();
    let link = json!({
        "url": "https://www.linkedin.com/comm/jobs/view/3912345678/?trackingId=abc",
        "source": "email",
        "title": "Staff AI Engineer",
        "person_id": "shashi",
        "source_detail": "email-1",
        "mailbox": "me@example.com",
        "remote": "remote",
        "employment_type": "Full-Time",
        "skills": ["Rust", "LLMs"],
        "description": "Build agents.",
        "score": 84,
        "decision": "apply",
        "resume_hint": "python",
        "posted_at": "2026-10-30T09:00:00Z"
    });
    let (status, v) = call(&app, Method::POST, "/v1/links", Some(link.clone())).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(v["created"], true);
    assert_eq!(v["link"]["url"], "https://www.linkedin.com/jobs/view/3912345678/");
    assert_eq!(v["link"]["platform"], "linkedin");
    assert_eq!(v["link"]["status"], "new");
    assert_eq!(v["link"]["source"], "email");
    assert_eq!(v["link"]["source_detail"], "email-1");
    assert_eq!(v["link"]["remote"], "remote");
    assert_eq!(v["link"]["employment_type"], "full_time");
    assert_eq!(v["link"]["skills"], json!(["Rust", "LLMs"]));
    assert_eq!(v["link"]["score"], 84);
    assert_eq!(v["link"]["resume_hint"], "python");
    assert_eq!(v["link"]["posted_at"], "2026-10-30T09:00:00Z");
    assert_eq!(v["link"]["status_history"][0]["status"], "new");
    assert!(v["link"]["discovered_at"].is_string());
    let id = v["link"]["id"].as_str().unwrap().to_string();

    let mut again = link;
    again["company"] = json!("Acme");
    let (status, v) = call(&app, Method::POST, "/v1/links", Some(again)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["created"], false);
    assert_eq!(v["link"]["id"], id.as_str());
    assert_eq!(v["link"]["company"], "Acme");

    let (status, v) = call(&app, Method::GET, &format!("/v1/links/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["original_url"], "https://www.linkedin.com/comm/jobs/view/3912345678/?trackingId=abc");
}

#[tokio::test]
async fn batch_create_reports_per_item() {
    let (app, _) = app();
    let body = json!({ "links": [
        { "url": "https://jobs.lever.co/acme/1" },
        { "url": "https://jobs.lever.co/acme/1?utm_source=x" },
        { "url": "ftp://nope" },
        { "title": "no url" },
        { "url": "https://boards.greenhouse.io/acme/jobs/2", "platform": "lever" }
    ]});
    let (status, v) = call(&app, Method::POST, "/v1/links", Some(body)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["created"], 2);
    assert_eq!(v["updated"], 1);
    assert_eq!(v["failed"], 2);
    assert!(v["results"][2]["error"].as_str().unwrap().contains("http"));
    assert_eq!(v["results"][4]["link"]["platform"], "greenhouse", "the URL wins over a wrong hint");

    // A bare array works too.
    let (status, v) = call(&app, Method::POST, "/v1/links", Some(json!([{ "url": "https://acme.com/careers/rust" }]))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["results"][0]["link"]["platform"], "career_site");

    let (status, _) = call(&app, Method::POST, "/v1/links", Some(json!({ "url": "not a url" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let too_many: Vec<Value> = (0..501).map(|i| json!({ "url": format!("https://jobs.lever.co/a/{i}") })).collect();
    let (status, _) = call(&app, Method::POST, "/v1/links", Some(Value::Array(too_many))).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn list_filters() {
    let (app, _) = app();
    let body = json!([
        { "url": "https://jobs.lever.co/acme/1", "person_id": "p1" },
        { "url": "https://jobs.lever.co/acme/2", "person_id": "p2" },
        { "url": "https://www.naukri.com/job-listings-x-3", "person_id": "p1" }
    ]);
    call(&app, Method::POST, "/v1/links", Some(body)).await;
    let (_, v) = call(&app, Method::GET, "/v1/links?since=2000-01-01T00:00:00Z&source=manual", None).await;
    assert_eq!(v["total"], 3);
    let (_, v) = call(&app, Method::GET, "/v1/links?since=2999-01-01T00:00:00Z", None).await;
    assert_eq!(v["total"], 0);
    let (_, v) = call(&app, Method::GET, "/v1/links?platform=lever&person=p1", None).await;
    assert_eq!(v["total"], 1);
    assert_eq!(v["links"][0]["url"], "https://jobs.lever.co/acme/1");
    let (_, v) = call(&app, Method::GET, "/v1/links?status=new&limit=1&offset=1&order=asc", None).await;
    assert_eq!(v["total"], 3);
    assert_eq!(v["links"].as_array().unwrap().len(), 1);
    assert_eq!(v["links"][0]["url"], "https://jobs.lever.co/acme/2");
    let (status, _) = call(&app, Method::GET, "/v1/links?status=bogus", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn claim_result_lifecycle() {
    let (app, _) = app();
    call(
        &app,
        Method::POST,
        "/v1/links",
        Some(json!([{ "url": "https://jobs.lever.co/acme/1" }, { "url": "https://jobs.lever.co/acme/2" }])),
    )
    .await;

    let (status, v) = call(&app, Method::POST, "/v1/links/claim", Some(json!({ "client": "ext-1", "limit": 1 }))).await;
    assert_eq!(status, StatusCode::OK);
    let links = v["links"].as_array().unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["status"], "claimed");
    assert_eq!(links[0]["claimed_by"], "ext-1");
    assert_eq!(links[0]["attempts"], 1);
    assert!(v["lease_expires_at"].is_string());
    let id = links[0]["id"].as_str().unwrap().to_string();

    let (status, v) = call(&app, Method::POST, "/v1/links/claim", Some(json!({ "client": "ext-2", "limit": 5 }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["links"].as_array().unwrap().len(), 1);
    assert_ne!(v["links"][0]["id"], id.as_str());

    let path = format!("/v1/links/{id}/result");
    let (status, _) = call(&app, Method::POST, &path, Some(json!({ "status": "applied", "client": "ext-2" }))).await;
    assert_eq!(status, StatusCode::CONFLICT, "only the claim holder reports");
    let (status, v) = call(
        &app,
        Method::POST,
        &path,
        Some(json!({ "status": "applied", "reason": "submitted", "client": "ext-1" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["status"], "applied");
    assert_eq!(v["status_reason"], "submitted");
    let (status, _) = call(&app, Method::POST, &path, Some(json!({ "status": "skipped" }))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(&app, Method::POST, &path, Some(json!({ "status": "new" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&app, Method::POST, "/v1/links/claim", Some(json!({ "limit": 1 }))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "client is required");

    let (_, v) = call(&app, Method::GET, "/v1/stats", None).await;
    assert_eq!(v["total"], 2);
    assert_eq!(v["by_status"]["applied"], 1);
    assert_eq!(v["by_status"]["claimed"], 1);
    assert_eq!(v["by_platform"]["lever"], 2);
}

#[tokio::test]
async fn claims_skip_stale_links_and_prefer_new_ones() {
    let (app, store) = app();
    call(
        &app,
        Method::POST,
        "/v1/links",
        Some(json!([
            { "url": "https://jobs.lever.co/acme/stale", "posted_at": "2020-01-01T00:00:00Z" },
            { "url": "https://jobs.lever.co/acme/older", "posted_at": (chrono::Utc::now() - chrono::Duration::days(3)).to_rfc3339() },
            { "url": "https://jobs.lever.co/acme/newer", "posted_at": (chrono::Utc::now() - chrono::Duration::hours(3)).to_rfc3339() }
        ])),
    )
    .await;
    let (_, v) = call(&app, Method::POST, "/v1/links/claim", Some(json!({ "client": "ext", "limit": 5 }))).await;
    let urls: Vec<&str> = v["links"].as_array().unwrap().iter().map(|l| l["url"].as_str().unwrap()).collect();
    assert_eq!(urls, ["https://jobs.lever.co/acme/newer", "https://jobs.lever.co/acme/older"], "default max age, newest first");
    assert_eq!(store.len(), 3, "the stale link is kept");
    let (_, v) = call(&app, Method::POST, "/v1/links/claim", Some(json!({ "client": "ext", "max_age_hours": 0 }))).await;
    assert_eq!(v["links"][0]["url"], "https://jobs.lever.co/acme/stale", "0 lifts the limit");
}

#[tokio::test]
async fn patch_and_delete() {
    let (app, store) = app();
    let (_, v) = call(&app, Method::POST, "/v1/links", Some(json!({ "url": "https://jobs.lever.co/acme/1" }))).await;
    let id = v["link"]["id"].as_str().unwrap().to_string();
    let path = format!("/v1/links/{id}");
    let (status, v) = call(
        &app,
        Method::PATCH,
        &path,
        Some(json!({ "title": "Lead", "status": "skipped", "status_reason": "not a fit" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["title"], "Lead");
    assert_eq!(v["status"], "skipped");
    assert_eq!(v["status_reason"], "not a fit");
    let (status, _) = call(&app, Method::PATCH, &path, Some(json!({ "status": "claimed" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = call(&app, Method::DELETE, &path, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(store.is_empty());
    let (status, v) = call(&app, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(v["error"], "link not found");
    let (status, _) = call(&app, Method::DELETE, &path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cors_allows_chrome_extensions_only() {
    let (app, _) = app();
    let preflight = |origin: &str| {
        Request::builder()
            .method(Method::OPTIONS)
            .uri("/v1/links/claim")
            .header(header::ORIGIN, origin)
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type,x-joblinks-token")
            .body(Body::empty())
            .unwrap()
    };
    let resp = app
        .clone()
        .oneshot(preflight("chrome-extension://abcdefghijklmnop"))
        .await
        .unwrap();
    assert!(resp.status().is_success(), "preflight needs no token");
    assert_eq!(
        resp.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
        "chrome-extension://abcdefghijklmnop"
    );
    let allowed = resp.headers().get(header::ACCESS_CONTROL_ALLOW_HEADERS).unwrap().to_str().unwrap().to_string();
    assert!(allowed.contains("x-joblinks-token"));

    let resp = app.clone().oneshot(preflight("https://evil.example")).await.unwrap();
    assert!(resp.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());

    let resp = app
        .clone()
        .oneshot(
            Request::get("/v1/stats")
                .header(header::ORIGIN, "chrome-extension://abcdefghijklmnop")
                .header(TOKEN_HEADER, TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_some());
}
