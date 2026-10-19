use async_trait::async_trait;
use serde_json::{json, Value};

use super::{truncate_err, LlmError, LlmProvider, LlmRequest};

/// Local Ollama (`/api/chat`, non-streaming).
pub struct OllamaProvider {
    http: reqwest::Client,
    base: String,
    model: String,
}

impl OllamaProvider {
    pub fn new(http: reqwest::Client, base: &str, model: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            model: model.to_string(),
        }
    }
    fn err(&self, message: impl ToString) -> LlmError {
        LlmError::Request {
            provider: "ollama".into(),
            message: message.to_string(),
        }
    }
}

#[async_trait]
impl LlmProvider for OllamaProvider {
    fn name(&self) -> &str {
        "ollama"
    }
    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, req: &LlmRequest) -> Result<String, LlmError> {
        let mut body = json!({
            "model": self.model,
            "stream": false,
            "messages": [
                { "role": "system", "content": req.system },
                { "role": "user", "content": req.prompt }
            ],
            "options": { "temperature": req.temperature, "num_predict": req.max_tokens },
            "keep_alive": "10m"
        });
        if req.json {
            body["format"] = json!("json");
        }
        let resp = self
            .http
            .post(format!("{}/api/chat", self.base))
            .json(&body)
            .send()
            .await
            .map_err(|e| self.err(e))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Status {
                provider: "ollama".into(),
                status: status.as_u16(),
                body: truncate_err(&body),
            });
        }
        let v: Value = resp.json().await.map_err(|e| self.err(e))?;
        v["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| LlmError::BadResponse {
                provider: "ollama".into(),
                message: "missing message.content".into(),
            })
    }

    async fn health(&self) -> bool {
        match self
            .http
            .get(format!("{}/api/tags", self.base))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }
}
