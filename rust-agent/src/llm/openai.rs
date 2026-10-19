use async_trait::async_trait;
use serde_json::{json, Value};

use super::{truncate_err, LlmError, LlmProvider, LlmRequest};

/// OpenAI-compatible Chat Completions API.
pub struct OpenAiProvider {
    http: reqwest::Client,
    base: String,
    key: String,
    model: String,
}

impl OpenAiProvider {
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
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai"
    }
    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, req: &LlmRequest) -> Result<String, LlmError> {
        let mut body = json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": req.system },
                { "role": "user", "content": req.prompt }
            ],
            "temperature": req.temperature,
            "max_tokens": req.max_tokens,
        });
        if req.json {
            body["response_format"] = json!({ "type": "json_object" });
        }
        let resp = self
            .http
            .post(format!("{}/chat/completions", self.base))
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Request {
                provider: "openai".into(),
                message: e.to_string(),
            })?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Status {
                provider: "openai".into(),
                status: status.as_u16(),
                body: truncate_err(&body),
            });
        }
        let v: Value = resp.json().await.map_err(|e| LlmError::BadResponse {
            provider: "openai".into(),
            message: e.to_string(),
        })?;
        v["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| LlmError::BadResponse {
                provider: "openai".into(),
                message: "missing choices[0].message.content".into(),
            })
    }

    async fn health(&self) -> bool {
        !self.key.is_empty()
    }
}
