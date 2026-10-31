//! Embedding models: Gemini (free tier, best quality) and a local Ollama model
//! (always available, private). Every chunk is embedded by each available
//! model into its own vector table; queries use whichever model is up.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{Error, Result};

/// Vector size shared by both models (Gemini is asked for 768 dimensions).
pub const DIMS: usize = 768;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Document,
    Query,
}

#[async_trait]
pub trait Embedder: Send + Sync {
    /// Stable id used as the vector table suffix ("gemini" / "local").
    fn id(&self) -> &'static str;
    fn model(&self) -> &str;
    async fn embed(&self, texts: &[String], kind: Kind) -> Result<Vec<Vec<f32>>>;
}

fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
    v
}

/// "Please retry in 3.4s" / "try again in 500ms" in a rate-limit answer.
fn retry_after(body: &str) -> Option<Duration> {
    let i = body.find("retry in ").map(|i| i + 9).or_else(|| body.find("try again in ").map(|i| i + 13))?;
    let num: String = body[i..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let n: f64 = num.parse().ok()?;
    let ms = body[i + num.len()..].trim_start().starts_with("ms");
    Some(Duration::from_secs_f64(if ms { n / 1000.0 } else { n }))
}

/// POST JSON, retrying twice when the provider says it is busy (429/503).
async fn post_json(http: &reqwest::Client, url: &str, headers: &[(&str, &str)], body: &Value) -> Result<Value> {
    for attempt in 0..3 {
        let mut req = http.post(url).json(body).timeout(Duration::from_secs(60));
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let resp = req.send().await.map_err(|e| Error::Upstream(e.without_url().to_string()))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if status.is_success() {
            return serde_json::from_str(&text).map_err(|e| Error::Upstream(format!("bad JSON: {e}")));
        }
        if (status.as_u16() == 429 || status.as_u16() == 503) && attempt < 2 {
            let wait = retry_after(&text).unwrap_or(Duration::from_secs(3)).min(Duration::from_secs(30));
            tracing::info!(status = status.as_u16(), wait_ms = wait.as_millis() as u64, "embedding provider busy; retrying");
            tokio::time::sleep(wait).await;
            continue;
        }
        return Err(Error::Upstream(format!("HTTP {status}: {}", text.chars().take(200).collect::<String>())));
    }
    unreachable!("loop returns")
}

pub struct Gemini {
    pub http: reqwest::Client,
    pub key: String,
    pub model: String,
    pub base: String,
}

#[async_trait]
impl Embedder for Gemini {
    fn id(&self) -> &'static str {
        "gemini"
    }
    fn model(&self) -> &str {
        &self.model
    }
    async fn embed(&self, texts: &[String], kind: Kind) -> Result<Vec<Vec<f32>>> {
        let task = match kind {
            Kind::Document => "RETRIEVAL_DOCUMENT",
            Kind::Query => "RETRIEVAL_QUERY",
        };
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(50) {
            let requests: Vec<Value> = batch
                .iter()
                .map(|t| {
                    json!({
                        "model": format!("models/{}", self.model),
                        "content": {"parts": [{"text": t}]},
                        "taskType": task,
                        "outputDimensionality": DIMS,
                    })
                })
                .collect();
            let url = format!("{}/models/{}:batchEmbedContents", self.base.trim_end_matches('/'), self.model);
            let v = post_json(&self.http, &url, &[("x-goog-api-key", &self.key)], &json!({ "requests": requests })).await?;
            let embs = v["embeddings"]
                .as_array()
                .ok_or_else(|| Error::Upstream("gemini: no embeddings".into()))?;
            for e in embs {
                let vals: Vec<f32> = e["values"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect())
                    .unwrap_or_default();
                if vals.len() != DIMS {
                    return Err(Error::Upstream(format!("gemini: expected {DIMS} dims, got {}", vals.len())));
                }
                out.push(normalize(vals));
            }
        }
        Ok(out)
    }
}

pub struct Ollama {
    pub http: reqwest::Client,
    pub base: String,
    pub model: String,
}

#[async_trait]
impl Embedder for Ollama {
    fn id(&self) -> &'static str {
        "local"
    }
    fn model(&self) -> &str {
        &self.model
    }
    async fn embed(&self, texts: &[String], kind: Kind) -> Result<Vec<Vec<f32>>> {
        // nomic-embed-text expects task prefixes
        let prefix = match (self.model.contains("nomic"), kind) {
            (true, Kind::Document) => "search_document: ",
            (true, Kind::Query) => "search_query: ",
            _ => "",
        };
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(16) {
            let input: Vec<String> = batch.iter().map(|t| format!("{prefix}{t}")).collect();
            let url = format!("{}/api/embed", self.base.trim_end_matches('/'));
            let v = post_json(&self.http, &url, &[], &json!({ "model": self.model, "input": input })).await?;
            let embs = v["embeddings"]
                .as_array()
                .ok_or_else(|| Error::Upstream("ollama: no embeddings".into()))?;
            for e in embs {
                let vals: Vec<f32> = e
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect())
                    .unwrap_or_default();
                if vals.len() != DIMS {
                    return Err(Error::Upstream(format!("ollama: expected {DIMS} dims, got {}", vals.len())));
                }
                out.push(normalize(vals));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_hints() {
        assert_eq!(retry_after("Please retry in 3.5s."), Some(Duration::from_millis(3500)));
        assert_eq!(retry_after("try again in 500ms"), Some(Duration::from_millis(500)));
        assert_eq!(retry_after("overloaded"), None);
    }

    #[test]
    fn vectors_are_unit_length() {
        let v = normalize(vec![3.0, 4.0]);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
    }
}
