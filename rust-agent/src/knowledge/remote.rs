//! Client for the knowledge service (hybrid RAG search). Calls are short and
//! guarded by a circuit breaker; while the service is down the knowledge base
//! falls back to its local keyword index, so drafting never stops.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::db::knowledge::Snippet;
use crate::error::{AppError, AppResult};

/// Consecutive failures that open the breaker, and how long it stays open.
const TRIP_AFTER: u32 = 3;
const OPEN_FOR: Duration = Duration::from_secs(30);
const TIMEOUT: Duration = Duration::from_secs(8);

pub struct KnowledgeClient {
    http: reqwest::Client,
    base: String,
    token: String,
    failures: AtomicU32,
    open_until: AtomicU64,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl KnowledgeClient {
    pub fn new(http: reqwest::Client, base: &str, token: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            failures: AtomicU32::new(0),
            open_until: AtomicU64::new(0),
        }
    }

    fn is_open(&self) -> bool {
        now_secs() < self.open_until.load(Ordering::Relaxed)
    }

    fn record(&self, ok: bool) {
        if ok {
            self.failures.store(0, Ordering::Relaxed);
        } else if self.failures.fetch_add(1, Ordering::Relaxed) + 1 >= TRIP_AFTER {
            self.open_until.store(now_secs() + OPEN_FOR.as_secs(), Ordering::Relaxed);
            self.failures.store(0, Ordering::Relaxed);
            tracing::warn!(base = %self.base, "knowledge service unavailable; using the local index for {}s", OPEN_FOR.as_secs());
        }
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> AppResult<Value> {
        if self.is_open() {
            return Err(AppError::other("knowledge service circuit open"));
        }
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .timeout(TIMEOUT)
            .header("x-knowledge-token", &self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = async {
            let resp = req.send().await?;
            let status = resp.status();
            let v: Value = resp.json().await.unwrap_or(Value::Null);
            if status.is_success() {
                Ok(v)
            } else {
                Err(AppError::other(format!("knowledge service {path}: HTTP {status} {v}")))
            }
        }
        .await;
        self.record(res.is_ok());
        res
    }

    /// Hybrid search within one person's (and purpose's) knowledge.
    pub async fn search(
        &self,
        query: &str,
        person: Option<&str>,
        purpose: Option<&str>,
        limit: usize,
    ) -> AppResult<Vec<Snippet>> {
        let v = self
            .send(
                reqwest::Method::POST,
                "/v1/search",
                Some(json!({ "query": query, "person": person, "purpose": purpose, "limit": limit })),
            )
            .await?;
        Ok(v["hits"]
            .as_array()
            .map(|hits| {
                hits.iter()
                    .map(|h| Snippet {
                        path: h["path"].as_str().unwrap_or("").to_string(),
                        title: h["title"].as_str().unwrap_or("").to_string(),
                        content: h["content"].as_str().unwrap_or("").to_string(),
                        score: h["score"].as_f64().unwrap_or(0.0),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Start an index run (the dashboard's "Re-index knowledge" button).
    pub async fn reindex(&self) -> AppResult<Value> {
        self.send(reqwest::Method::POST, "/v1/index/run", None).await
    }

    /// Index size, embedding coverage and the last run's report.
    pub async fn status(&self) -> AppResult<Value> {
        self.send(reqwest::Method::GET, "/v1/index", None).await
    }
}
