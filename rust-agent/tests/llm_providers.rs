//! Wire-format tests for every LLM provider against mocked HTTP endpoints,
//! including a full email→draft run through the real Ollama provider.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use emailhelper::app::{App, AppOverrides};
use emailhelper::config::Config;
use emailhelper::llm::{LlmRequest, LlmRouter, LlmTask};
use emailhelper::metrics::Metrics;
use serde_json::{json, Value};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Ollama stand-in that answers each agent's prompt sensibly.
struct FakeOllama;
impl Respond for FakeOllama {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let system = body["messages"][0]["content"].as_str().unwrap_or("");
        let content = if system.contains("triage classifier") {
            json!({"category":"sales","confidence":0.92,"is_job_opportunity":false,"needs_reply":true,"urgency":"normal","summary":"Pricing question"}).to_string()
        } else if system.contains("strict reviewer") {
            json!({"verdict":"APPROVED","issues":[],"confidence":0.9}).to_string()
        } else {
            "Hi Jane,\n\nThanks for reaching out. Our standard consulting rate is 150 USD per hour, and a two-week discovery and prototype sprint starts at 6000 USD. Happy to set up a call to discuss scope.\n\nBest regards,\nAsha Rao".to_string()
        };
        ResponseTemplate::new(200).set_body_json(json!({
            "model": body["model"], "message": {"role": "assistant", "content": content}, "done": true
        }))
    }
}

#[tokio::test]
async fn ollama_end_to_end_draft() {
    let ollama = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .and(body_partial_json(
            json!({"model": "qwen2.5:7b", "stream": false}),
        ))
        .respond_with(FakeOllama)
        .mount(&ollama)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": []})))
        .mount(&ollama)
        .await;

    // Build without the mock override so the real Ollama provider is used.
    let dir = tempfile::tempdir().unwrap();
    let env = std::collections::HashMap::from([
        ("EH_API_TOKEN", TOKEN.to_string()),
        ("EH_DATA_DIR", dir.path().to_string_lossy().to_string()),
        (
            "EH_KNOWLEDGE_DIR",
            knowledge_dir().to_string_lossy().to_string(),
        ),
        ("MODEL_MODE", "LOCAL".to_string()),
        ("OLLAMA_BASE_URL", ollama.uri()),
        ("OLLAMA_MODEL", "qwen2.5:7b".to_string()),
    ]);
    let cfg = Config::from_lookup(|k| env.get(k).cloned()).unwrap();
    let app = App::build(cfg, AppOverrides::default()).unwrap();
    assert_eq!(app.llm.provider_name(), "ollama");
    let _bg = emailhelper::start_background(&app).await.unwrap();

    let mut e: emailhelper::db::emails::EmailIn = serde_json::from_value(email(
        "gmail-1",
        "<ollama@acme.com>",
        "Jane Doe <jane@acme.com>",
        "Consulting rates",
        "Hi Asha, what is your hourly rate, and how much is a two week prototype sprint?",
    ))
    .unwrap();
    e.validate(65536).unwrap();
    let id = app
        .db
        .call(move |c| match emailhelper::db::emails::insert(c, &e)? {
            emailhelper::db::emails::InsertOutcome::Inserted(id) => Ok(id),
            _ => unreachable!(),
        })
        .await
        .unwrap();
    app.queue
        .enqueue(
            "email",
            "email:gmail-1:<ollama@acme.com>:v1".into(),
            Some(id.clone()),
            None,
            1,
        )
        .await
        .unwrap();
    let drafts = wait_for("ollama draft", Duration::from_secs(20), || async {
        let d = app
            .db
            .call(|c| emailhelper::db::drafts::claim_pending(c, 10))
            .await
            .ok()?;
        (!d.is_empty()).then_some(d)
    })
    .await;
    let d = &drafts[0];
    assert_eq!(
        d.validation, "APPROVED",
        "grounded draft passes validation: {:?}",
        d.notes
    );
    assert!(d.body.contains("150 USD"));
    let rec = {
        let id = id.clone();
        app.db
            .call(move |c| emailhelper::db::emails::get(c, &id))
            .await
            .unwrap()
    };
    assert_eq!(rec.category.as_deref(), Some("sales"));
    assert!(app.llm.health().await);
    let reqs = ollama.received_requests().await.unwrap();
    let chat: Vec<Value> = reqs
        .iter()
        .filter(|r| r.url.path() == "/api/chat")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(chat.len(), 3, "classify + draft + validate");
    assert_eq!(chat[0]["format"], "json", "JSON mode for classification");
    assert!(chat[1].get("format").is_none(), "free text for drafting");
}

fn router(cfg_pairs: &[(&str, String)]) -> LlmRouter {
    let map: std::collections::HashMap<String, String> = cfg_pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    let cfg = Config::from_lookup(|k| map.get(k).cloned()).unwrap();
    LlmRouter::from_config(&cfg.llm, reqwest::Client::new(), Arc::new(Metrics::new()))
}

#[tokio::test]
async fn openai_wire_format() {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-test"))
        .and(body_partial_json(
            json!({"model": "gpt-4o-mini", "response_format": {"type": "json_object"}}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices": [{"message": {"content": "{\"ok\":true}"}}]})),
        )
        .mount(&s)
        .await;
    let r = router(&[
        ("MODEL_MODE", "REMOTE_OPENAI".into()),
        ("OPENAI_API_KEY", "sk-test".into()),
        ("OPENAI_BASE_URL", format!("{}/v1", s.uri())),
    ]);
    assert_eq!(r.provider_name(), "openai");
    let (v, resp): (Value, _) = r
        .complete_json(&LlmRequest::new(LlmTask::Classify, "sys", "hi").json())
        .await
        .unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(resp.provider, "openai");
}

#[tokio::test]
async fn gemini_wire_format_uses_header_key() {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-2.0-flash:generateContent"))
        .and(header("x-goog-api-key", "g-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"parts": [{"text": "Hello "}, {"text": "world"}]}}]
        })))
        .mount(&s)
        .await;
    let r = router(&[
        ("MODEL_MODE", "REMOTE_GEMINI".into()),
        ("GEMINI_API_KEY", "g-test".into()),
        ("GEMINI_BASE_URL", format!("{}/v1beta", s.uri())),
    ]);
    let resp = r
        .complete(&LlmRequest::new(LlmTask::Draft, "sys", "hi"))
        .await
        .unwrap();
    assert_eq!(resp.text, "Hello world");
    let req = &s.received_requests().await.unwrap()[0];
    assert!(!req.url.as_str().contains("g-test"), "API key never in URL");
    let body: Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(body["systemInstruction"]["parts"][0]["text"], "sys");
}

#[tokio::test]
async fn anthropic_wire_format() {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "a-test"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(body_partial_json(
            json!({"model": "claude-haiku-5-5", "system": "sys"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type": "text", "text": "Drafted reply"}]
        })))
        .mount(&s)
        .await;
    let r = router(&[
        ("MODEL_MODE", "REMOTE_ANTHROPIC".into()),
        ("ANTHROPIC_API_KEY", "a-test".into()),
        ("ANTHROPIC_BASE_URL", format!("{}/v1", s.uri())),
    ]);
    let resp = r
        .complete(&LlmRequest::new(LlmTask::Draft, "sys", "hi"))
        .await
        .unwrap();
    assert_eq!(resp.text, "Drafted reply");
    assert_eq!(resp.model, "claude-haiku-5-5");
}

#[tokio::test]
async fn remote_failure_falls_back_to_ollama() {
    let remote = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).set_body_string("overloaded"))
        .mount(&remote)
        .await;
    let ollama = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"message": {"content": "local answer"}})),
        )
        .mount(&ollama)
        .await;
    let r = router(&[
        ("MODEL_MODE", "REMOTE_OPENAI".into()),
        ("OPENAI_API_KEY", "sk".into()),
        ("OPENAI_BASE_URL", remote.uri()),
        ("OLLAMA_BASE_URL", ollama.uri()),
    ]);
    let resp = r
        .complete(&LlmRequest::new(LlmTask::Draft, "s", "p"))
        .await
        .unwrap();
    assert_eq!(resp.provider, "ollama");
    assert_eq!(resp.text, "local answer");
}
