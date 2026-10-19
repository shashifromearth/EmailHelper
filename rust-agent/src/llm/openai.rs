use async_trait::async_trait;
use serde_json::{json, Value};

use super::{truncate_err, LlmError, LlmProvider, LlmRequest};

/// OpenAI-compatible Chat Completions API.
pub struct OpenAiProvider {
    http: reqwest::Client,
    base: String,
    key: String,
    model: String,
    name: &'static str,
}

impl OpenAiProvider {
    pub fn new(http: reqwest::Client, base: &str, key: &str, model: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            key: key.to_string(),
            model: model.to_string(),
            name: "openai",
        }
    }

    /// Same API under another provider name (Groq, OpenRouter, Mistral, …).
    pub fn named(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }

    /// Reasoning models spend completion tokens thinking before they answer.
    fn is_reasoning_model(&self) -> bool {
        let m = self.model.to_ascii_lowercase();
        m.contains("gpt-oss") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") || m.contains("qwen3")
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &str {
        self.name
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
        if self.is_reasoning_model() {
            // Keep reasoning short and leave room for it on top of the answer.
            body["reasoning_effort"] = json!("low");
            body["max_tokens"] = json!(req.max_tokens + 1024);
        }
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
                provider: self.name.into(),
                message: e.to_string(),
            })?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Status {
                provider: self.name.into(),
                status: status.as_u16(),
                body: truncate_err(&body),
            });
        }
        let v: Value = resp.json().await.map_err(|e| LlmError::BadResponse {
            provider: self.name.into(),
            message: e.to_string(),
        })?;
        v["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| LlmError::BadResponse {
                provider: self.name.into(),
                message: "missing choices[0].message.content".into(),
            })
    }

    async fn health(&self) -> bool {
        !self.key.is_empty()
    }
}
