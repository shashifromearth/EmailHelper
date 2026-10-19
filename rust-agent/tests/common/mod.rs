//! Shared helpers for integration tests: a fully wired agent on a random
//! localhost port with a temp database, the sample knowledge base and a
//! deterministic mock LLM.

#![allow(dead_code)]

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use emailhelper::app::{App, AppOverrides};
use emailhelper::config::Config;
use emailhelper::db::Db;
use emailhelper::llm::{LlmProvider, MockProvider};
use serde_json::{json, Value};

pub const TOKEN: &str = "test-token-0123456789abcdef";

pub struct TestAgent {
    pub app: Arc<App>,
    pub base: String,
    pub http: reqwest::Client,
    pub dir: tempfile::TempDir,
    pub server: tokio::task::JoinHandle<()>,
    pub bg: emailhelper::Background,
}

impl Drop for TestAgent {
    fn drop(&mut self) {
        self.server.abort();
        self.bg.abort();
    }
}

pub fn knowledge_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/knowledge")
}

pub struct Builder {
    env: HashMap<String, String>,
    llm: Option<Arc<dyn LlmProvider>>,
    db_path: Option<PathBuf>,
    dir: Option<tempfile::TempDir>,
}

pub fn builder() -> Builder {
    Builder {
        env: HashMap::new(),
        llm: None,
        db_path: None,
        dir: None,
    }
}

impl Builder {
    pub fn env(mut self, k: &str, v: &str) -> Self {
        self.env.insert(k.to_string(), v.to_string());
        self
    }
    pub fn llm(mut self, p: impl LlmProvider + 'static) -> Self {
        self.llm = Some(Arc::new(p));
        self
    }
    /// Reuse an existing temp dir (restart tests).
    pub fn dir(mut self, d: tempfile::TempDir) -> Self {
        self.dir = Some(d);
        self
    }

    pub async fn start(self) -> TestAgent {
        let dir = self.dir.unwrap_or_else(|| tempfile::tempdir().unwrap());
        let db_path = self.db_path.unwrap_or_else(|| dir.path().join("test.db"));
        let mut env: HashMap<String, String> = HashMap::from([
            ("EH_API_TOKEN".into(), TOKEN.into()),
            ("EH_DATA_DIR".into(), dir.path().to_string_lossy().into()),
            (
                "EH_KNOWLEDGE_DIR".into(),
                knowledge_dir().to_string_lossy().into(),
            ),
            ("EH_BIND_ADDR".into(), "127.0.0.1:0".into()),
            ("MODEL_MODE".into(), "MOCK".into()),
            ("EH_MAX_RETRIES".into(), "1".into()),
        ]);
        env.extend(self.env);
        let cfg = Config::from_lookup(|k| env.get(k).cloned()).expect("config");
        let db = Db::open(&db_path).expect("db");
        let app = App::build(
            cfg,
            AppOverrides {
                llm: Some(
                    self.llm
                        .unwrap_or_else(|| Arc::new(MockProvider::default())),
                ),
                db: Some(db),
            },
        )
        .expect("app");
        let bg = emailhelper::start_background(&app)
            .await
            .expect("background");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let router = emailhelper::api::router(app.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        TestAgent {
            app,
            base: format!("http://{addr}"),
            http: reqwest::Client::new(),
            dir,
            server,
            bg,
        }
    }
}

impl TestAgent {
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    pub async fn post(&self, path: &str, body: &Value) -> reqwest::Response {
        self.http
            .post(self.url(path))
            .header("X-EmailHelper-Token", TOKEN)
            .json(body)
            .send()
            .await
            .unwrap()
    }

    pub async fn get(&self, path: &str) -> reqwest::Response {
        self.http
            .get(self.url(path))
            .header("X-EmailHelper-Token", TOKEN)
            .send()
            .await
            .unwrap()
    }

    pub async fn get_json(&self, path: &str) -> Value {
        let r = self.get(path).await;
        assert!(r.status().is_success(), "GET {path} -> {}", r.status());
        r.json().await.unwrap()
    }

    /// Long-poll until at least `n` drafts have been handed out.
    pub async fn collect_drafts(&self, n: usize, timeout: Duration) -> Vec<Value> {
        let start = Instant::now();
        let mut out = Vec::new();
        while out.len() < n && start.elapsed() < timeout {
            let v = self.get_json("/api/v1/drafts/pending?wait=5").await;
            out.extend(v["drafts"].as_array().cloned().unwrap_or_default());
        }
        out
    }

    pub async fn metrics(&self) -> String {
        self.http
            .get(self.url("/metrics"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
    }
}

/// Read a counter value (sum over all label sets) from Prometheus text.
pub fn metric_sum(text: &str, name: &str) -> f64 {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter(|l| l.starts_with(&format!("{name} ")) || l.starts_with(&format!("{name}{{")))
        .filter_map(|l| l.rsplit(' ').next()?.parse::<f64>().ok())
        .sum()
}

pub fn email(account: &str, msg_id: &str, from: &str, subject: &str, body: &str) -> Value {
    json!({
        "account_id": account,
        "account_name": format!("{account} mailbox"),
        "account_email": format!("{account}@example.com"),
        "account_type": "gmail",
        "message_id": msg_id,
        "tb_message_id": 1000 + msg_id.len() as i64,
        "folder": "INBOX",
        "references": [],
        "from": from,
        "to": [format!("{account}@example.com")],
        "cc": [],
        "subject": subject,
        "date": chrono::Utc::now().to_rfc3339(),
        "body_text": body,
        "attachments": []
    })
}

/// Poll an async condition until it returns `Some`, or panic after `timeout`.
pub async fn wait_for<T, F, Fut>(what: &str, timeout: Duration, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(v) = f().await {
            return v;
        }
        if start.elapsed() > timeout {
            panic!("timed out waiting for {what}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub async fn email_status(app: &App, id: &str) -> String {
    let id = id.to_string();
    app.db
        .call(move |c| Ok(emailhelper::db::emails::get(c, &id)?.status))
        .await
        .unwrap()
}
