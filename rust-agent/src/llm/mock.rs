//! Deterministic offline LLM. Used by tests, CI and `MODEL_MODE=MOCK` demos.
//! Output depends only on the request, so pipelines are reproducible.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use super::{LlmError, LlmProvider, LlmRequest, LlmTask};

type Handler = Arc<dyn Fn(&LlmRequest) -> Option<Result<String, LlmError>> + Send + Sync>;

#[derive(Default, Clone)]
pub struct MockProvider {
    /// Optional override; return `None` to fall through to the defaults.
    handler: Option<Handler>,
    calls: Arc<AtomicUsize>,
    delay_ms: u64,
}

impl MockProvider {
    pub fn with_handler<F>(f: F) -> Self
    where
        F: Fn(&LlmRequest) -> Option<Result<String, LlmError>> + Send + Sync + 'static,
    {
        Self {
            handler: Some(Arc::new(f)),
            ..Default::default()
        }
    }

    /// Simulate model latency (load tests).
    pub fn with_delay(mut self, ms: u64) -> Self {
        self.delay_ms = ms;
        self
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub fn default_response(req: &LlmRequest) -> String {
        let p = req.prompt.to_lowercase();
        match req.task {
            LlmTask::Classify => {
                let (cat, job) = if has(&p, &["job alert", "apply now", "recommended job", "jobs you may", "new jobs", "job opportunity", "we are hiring", "your profile", "recruiter", "interview"]) {
                    ("recruiting", true)
                } else if has(&p, &["invoice", "payment due", "amount due", "receipt"]) {
                    ("invoice", false)
                } else if has(&p, &["unacceptable", "disappointed", "complaint", "terrible", "refund immediately"]) {
                    ("complaint", false)
                } else if has(&p, &["partnership", "partner with", "collaborate", "collaboration"]) {
                    ("partnership", false)
                } else if has(&p, &["pricing", "quote", "purchase", "buy", "demo", "plan"]) {
                    ("sales", false)
                } else if has(&p, &["following up", "follow up", "follow-up", "checking in", "any update"]) {
                    ("follow_up", false)
                } else if has(&p, &["lottery", "winner", "crypto giveaway", "viagra", "claim your prize"]) {
                    ("spam", false)
                } else {
                    ("support", false)
                };
                json!({
                    "category": cat,
                    "confidence": 0.9,
                    "is_job_opportunity": job,
                    "needs_reply": cat != "spam",
                    "urgency": if cat == "complaint" { "high" } else { "normal" },
                    "summary": "Mock classification",
                })
                .to_string()
            }
            LlmTask::Draft => "Thank you for your message. I have received it and will review the details you shared. I will follow up with a complete answer shortly.\n\nBest regards".to_string(),
            LlmTask::Validate => json!({ "verdict": "APPROVED", "issues": [], "confidence": 0.9 }).to_string(),
            LlmTask::JobExtract => json!({}).to_string(),
            LlmTask::CoverLetter => "Dear Hiring Team,\n\nI am excited to apply for this role. Over the past years I have designed and shipped production AI systems, led engineering teams and built high-performance services in Rust and Python. The responsibilities described in this posting closely match the work I enjoy most: architecting reliable machine-learning platforms, mentoring engineers and turning research into products that customers rely on every day.\n\nIn my recent roles I owned the architecture of retrieval-augmented generation systems, built evaluation pipelines and reduced inference costs significantly while improving latency. I care deeply about observability, security and pragmatic engineering trade-offs, and I enjoy collaborating with product and research partners.\n\nI would welcome the opportunity to discuss how my experience can help your team succeed.\n\nKind regards".to_string(),
            LlmTask::FormAnswer => {
                if p.contains("question: why") || p.contains("question: what interests you") {
                    json!({ "answer": "I am excited about the mission and the chance to build reliable AI systems at scale, which matches my experience leading AI platform work.", "confidence": 0.8 }).to_string()
                } else {
                    json!({ "answer": null, "confidence": 0.0 }).to_string()
                }
            }
            LlmTask::Outreach => "Hi there,\n\nI came across the open AI engineering roles at your company and would love to connect. I lead AI platform work and build production systems in Rust and Python. Would you be open to a short conversation about the team's hiring plans?\n\nThank you!".to_string(),
            LlmTask::Summarize => req.prompt.chars().take(280).collect(),
        }
    }
}

fn has(p: &str, words: &[&str]) -> bool {
    words.iter().any(|w| p.contains(w))
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }
    fn model(&self) -> &str {
        "mock-1"
    }
    async fn complete(&self, req: &LlmRequest) -> Result<String, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        }
        if let Some(h) = &self.handler {
            if let Some(r) = h(req) {
                return r;
            }
        }
        Ok(Self::default_response(req))
    }
    async fn health(&self) -> bool {
        true
    }
}
