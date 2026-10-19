//! LLM abstraction: Ollama (default), OpenAI, Gemini, Anthropic and a
//! deterministic mock. The [`LlmRouter`] picks the configured provider, falls
//! back to Ollama when cloud keys are missing, and retries on Ollama when a
//! remote call fails.

mod anthropic;
mod gemini;
pub mod mock;
mod ollama;
mod openai;

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde::de::DeserializeOwned;

use crate::config::{LlmConfig, ModelMode};
use crate::metrics::Metrics;

pub use anthropic::AnthropicProvider;
pub use gemini::GeminiProvider;
pub use mock::MockProvider;
pub use ollama::OllamaProvider;
pub use openai::OpenAiProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LlmTask {
    Classify,
    Draft,
    Validate,
    JobExtract,
    CoverLetter,
    FormAnswer,
    Outreach,
    Summarize,
}

impl LlmTask {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Classify => "classify",
            Self::Draft => "draft",
            Self::Validate => "validate",
            Self::JobExtract => "job_extract",
            Self::CoverLetter => "cover_letter",
            Self::FormAnswer => "form_answer",
            Self::Outreach => "outreach",
            Self::Summarize => "summarize",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub task: LlmTask,
    pub system: String,
    pub prompt: String,
    /// Ask the provider for a JSON object response.
    pub json: bool,
    pub max_tokens: u32,
    pub temperature: f32,
}

impl LlmRequest {
    pub fn new(task: LlmTask, system: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            task,
            system: system.into(),
            prompt: prompt.into(),
            json: false,
            max_tokens: 800,
            temperature: 0.3,
        }
    }
    pub fn json(mut self) -> Self {
        self.json = true;
        self.temperature = 0.0;
        self
    }
    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub text: String,
    pub provider: String,
    pub model: String,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("provider {provider} request failed: {message}")]
    Request { provider: String, message: String },
    #[error("provider {provider} returned status {status}: {body}")]
    Status {
        provider: String,
        status: u16,
        body: String,
    },
    #[error("provider {provider} returned an unexpected response: {message}")]
    BadResponse { provider: String, message: String },
    #[error("provider {provider} timed out")]
    Timeout { provider: String },
    #[error("could not parse model output as JSON: {0}")]
    Parse(String),
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    async fn complete(&self, req: &LlmRequest) -> Result<String, LlmError>;
    async fn health(&self) -> bool;
}

pub struct LlmRouter {
    primary: Arc<dyn LlmProvider>,
    fallback: Option<Arc<dyn LlmProvider>>,
    timeout: std::time::Duration,
    metrics: Arc<Metrics>,
}

impl LlmRouter {
    pub fn new(
        primary: Arc<dyn LlmProvider>,
        fallback: Option<Arc<dyn LlmProvider>>,
        timeout: std::time::Duration,
        metrics: Arc<Metrics>,
    ) -> Self {
        Self {
            primary,
            fallback,
            timeout,
            metrics,
        }
    }

    /// Build the router from configuration. Never fails: a remote mode without
    /// an API key silently becomes local Ollama.
    pub fn from_config(cfg: &LlmConfig, http: reqwest::Client, metrics: Arc<Metrics>) -> Self {
        let ollama: Arc<dyn LlmProvider> = Arc::new(OllamaProvider::new(
            http.clone(),
            &cfg.ollama_base_url,
            &cfg.ollama_model,
        ));
        let remote: Option<Arc<dyn LlmProvider>> = match cfg.mode {
            ModelMode::RemoteOpenAi => cfg.openai_api_key.as_ref().map(|k| {
                Arc::new(OpenAiProvider::new(
                    http.clone(),
                    &cfg.openai_base_url,
                    k,
                    &cfg.openai_model,
                )) as Arc<dyn LlmProvider>
            }),
            ModelMode::RemoteGemini => cfg.gemini_api_key.as_ref().map(|k| {
                Arc::new(GeminiProvider::new(
                    http.clone(),
                    &cfg.gemini_base_url,
                    k,
                    &cfg.gemini_model,
                )) as Arc<dyn LlmProvider>
            }),
            ModelMode::RemoteAnthropic => cfg.anthropic_api_key.as_ref().map(|k| {
                Arc::new(AnthropicProvider::new(
                    http.clone(),
                    &cfg.anthropic_base_url,
                    k,
                    &cfg.anthropic_model,
                )) as Arc<dyn LlmProvider>
            }),
            ModelMode::Mock => {
                return Self::new(
                    Arc::new(MockProvider::default()),
                    None,
                    cfg.timeout,
                    metrics,
                )
            }
            ModelMode::Local => None,
        };
        match remote {
            Some(r) => {
                let fb = cfg.fallback_to_local.then_some(ollama);
                Self::new(r, fb, cfg.timeout, metrics)
            }
            None => {
                if cfg.mode != ModelMode::Local {
                    tracing::warn!(
                        mode = ?cfg.mode,
                        "API key for remote model mode is missing; using local Ollama"
                    );
                }
                Self::new(ollama, None, cfg.timeout, metrics)
            }
        }
    }

    pub fn provider_name(&self) -> &str {
        self.primary.name()
    }
    pub fn model_name(&self) -> &str {
        self.primary.model()
    }

    pub async fn health(&self) -> bool {
        self.primary.health().await
    }

    async fn call(&self, p: &Arc<dyn LlmProvider>, req: &LlmRequest) -> Result<String, LlmError> {
        let started = Instant::now();
        let res = match tokio::time::timeout(self.timeout, p.complete(req)).await {
            Ok(r) => r,
            Err(_) => Err(LlmError::Timeout {
                provider: p.name().to_string(),
            }),
        };
        self.metrics
            .llm_request_duration_seconds
            .with_label_values(&[p.name()])
            .observe(started.elapsed().as_secs_f64());
        let status = if res.is_ok() { "ok" } else { "error" };
        self.metrics
            .llm_requests_total
            .with_label_values(&[p.name(), status])
            .inc();
        res
    }

    pub async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        match self.call(&self.primary, req).await {
            Ok(text) => Ok(LlmResponse {
                text,
                provider: self.primary.name().to_string(),
                model: self.primary.model().to_string(),
            }),
            Err(e) => {
                let Some(fb) = &self.fallback else {
                    return Err(e);
                };
                tracing::warn!(error = %e, task = req.task.as_str(), "primary LLM failed; falling back");
                self.metrics
                    .llm_requests_total
                    .with_label_values(&[fb.name(), "fallback"])
                    .inc();
                let text = self.call(fb, req).await?;
                Ok(LlmResponse {
                    text,
                    provider: fb.name().to_string(),
                    model: fb.model().to_string(),
                })
            }
        }
    }

    /// Complete and parse a JSON object; retries once with a stricter reminder.
    pub async fn complete_json<T: DeserializeOwned>(
        &self,
        req: &LlmRequest,
    ) -> Result<(T, LlmResponse), LlmError> {
        let mut req = req.clone();
        req.json = true;
        let mut last_err = String::new();
        for attempt in 0..2 {
            if attempt == 1 {
                req.prompt.push_str(
                    "\n\nIMPORTANT: Respond with ONE valid JSON object only. No prose, no code fences.",
                );
            }
            let resp = self.complete(&req).await?;
            match parse_json_lenient::<T>(&resp.text) {
                Ok(v) => return Ok((v, resp)),
                Err(e) => last_err = e,
            }
        }
        Err(LlmError::Parse(last_err))
    }
}

/// Parse JSON from model output, tolerating code fences and leading prose.
pub fn parse_json_lenient<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<T>(t) {
        return Ok(v);
    }
    let stripped = t
        .trim_start_matches("```json")
        .trim_start_matches("```JSON")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(v) = serde_json::from_str::<T>(stripped) {
        return Ok(v);
    }
    if let (Some(s), Some(e)) = (t.find('{'), t.rfind('}')) {
        if s < e {
            return serde_json::from_str::<T>(&t[s..=e]).map_err(|e| e.to_string());
        }
    }
    Err(format!(
        "no JSON object found in model output ({} chars)",
        t.len()
    ))
}

pub(crate) fn truncate_err(body: &str) -> String {
    body.chars().take(500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize, Debug, PartialEq)]
    struct X {
        a: i32,
    }

    #[test]
    fn lenient_json() {
        assert_eq!(parse_json_lenient::<X>("{\"a\":1}").unwrap(), X { a: 1 });
        assert_eq!(
            parse_json_lenient::<X>("```json\n{\"a\":2}\n```").unwrap(),
            X { a: 2 }
        );
        assert_eq!(
            parse_json_lenient::<X>("Sure! Here you go: {\"a\":3} hope it helps").unwrap(),
            X { a: 3 }
        );
        assert!(parse_json_lenient::<X>("nope").is_err());
    }

    struct Failing;
    #[async_trait]
    impl LlmProvider for Failing {
        fn name(&self) -> &str {
            "failing"
        }
        fn model(&self) -> &str {
            "x"
        }
        async fn complete(&self, _: &LlmRequest) -> Result<String, LlmError> {
            Err(LlmError::Request {
                provider: "failing".into(),
                message: "boom".into(),
            })
        }
        async fn health(&self) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn falls_back_when_primary_fails() {
        let m = Arc::new(Metrics::new());
        let r = LlmRouter::new(
            Arc::new(Failing),
            Some(Arc::new(MockProvider::default())),
            std::time::Duration::from_secs(5),
            m.clone(),
        );
        let resp = r
            .complete(&LlmRequest::new(LlmTask::Summarize, "s", "hello world"))
            .await
            .unwrap();
        assert_eq!(resp.provider, "mock");
        assert_eq!(
            m.llm_requests_total
                .with_label_values(&["mock", "fallback"])
                .get(),
            1
        );
    }

    #[test]
    fn missing_keys_use_ollama() {
        let cfg = crate::config::Config::from_lookup(|k| match k {
            "MODEL_MODE" => Some("REMOTE_OPENAI".into()),
            _ => None,
        })
        .unwrap();
        let r = LlmRouter::from_config(&cfg.llm, reqwest::Client::new(), Arc::new(Metrics::new()));
        assert_eq!(r.provider_name(), "ollama");
    }
}
