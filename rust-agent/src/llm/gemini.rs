use async_trait::async_trait;
use serde_json::{json, Value};

use super::{truncate_err, LlmError, LlmProvider, LlmRequest};

/// Google Gemini `generateContent` API. The key is sent in the
/// `x-goog-api-key` header, never in the URL.
pub struct GeminiProvider {
    http: reqwest::Client,
    base: String,
    key: String,
    model: String,
}

impl GeminiProvider {
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
impl LlmProvider for GeminiProvider {
    fn name(&self) -> &str {
        "gemini"
    }
    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, req: &LlmRequest) -> Result<String, LlmError> {
        let mut gen = json!({
            "temperature": req.temperature,
            // Current Gemini models think first and the thinking counts
            // against this cap, so leave room for it on top of the answer.
            "maxOutputTokens": req.max_tokens + 1024,
        });
        if req.json {
            gen["responseMimeType"] = json!("application/json");
        }
        let body = json!({
            "systemInstruction": { "parts": [ { "text": req.system } ] },
            "contents": [ { "role": "user", "parts": [ { "text": req.prompt } ] } ],
            "generationConfig": gen,
        });
        let resp = self
            .http
            .post(format!(
                "{}/models/{}:generateContent",
                self.base, self.model
            ))
            .header("x-goog-api-key", &self.key)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Request {
                provider: "gemini".into(),
                message: e.to_string(),
            })?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Status {
                provider: "gemini".into(),
                status: status.as_u16(),
                body: truncate_err(&body),
            });
        }
        let v: Value = resp.json().await.map_err(|e| LlmError::BadResponse {
            provider: "gemini".into(),
            message: e.to_string(),
        })?;
        let parts = v["candidates"][0]["content"]["parts"]
            .as_array()
            .ok_or_else(|| LlmError::BadResponse {
                provider: "gemini".into(),
                message: format!(
                    "missing candidates[0].content.parts (finishReason {})",
                    v["candidates"][0]["finishReason"].as_str().unwrap_or("?")
                ),
            })?;
        Ok(parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join(""))
    }

    async fn health(&self) -> bool {
        !self.key.is_empty()
    }
}
