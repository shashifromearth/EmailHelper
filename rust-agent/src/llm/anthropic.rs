use async_trait::async_trait;
use serde_json::{json, Value};

use super::{truncate_err, LlmError, LlmProvider, LlmRequest};

/// Anthropic Messages API.
pub struct AnthropicProvider {
    http: reqwest::Client,
    base: String,
    key: String,
    model: String,
}

impl AnthropicProvider {
    pub fn new(http: reqwest::Client, base: &str, key: &str, model: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            key: key.to_string(),
            model: model.to_string(),
        }
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }
    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, req: &LlmRequest) -> Result<String, LlmError> {
        let mut system = req.system.clone();
        if req.json {
            system.push_str("\n\nRespond with a single JSON object and nothing else.");
        }
        let body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens,
            "system": system,
            "messages": [ { "role": "user", "content": req.prompt } ],
        });
        let resp = self
            .http
            .post(format!("{}/messages", self.base))
            .header("x-api-key", &self.key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Request {
                provider: "anthropic".into(),
                message: e.to_string(),
            })?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Status {
                provider: "anthropic".into(),
                status: status.as_u16(),
                body: truncate_err(&body),
            });
        }
        let v: Value = resp.json().await.map_err(|e| LlmError::BadResponse {
            provider: "anthropic".into(),
            message: e.to_string(),
        })?;
        let blocks = v["content"]
            .as_array()
            .ok_or_else(|| LlmError::BadResponse {
                provider: "anthropic".into(),
                message: "missing content".into(),
            })?;
        Ok(blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join(""))
    }

    async fn health(&self) -> bool {
        !self.key.is_empty()
    }
}
