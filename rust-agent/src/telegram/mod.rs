//! Notification agent: Telegram alerts for every event in the brief, plus the
//! interactive bot used for human-in-the-loop answers.

pub mod bot;
pub mod client;

use std::sync::Arc;

pub use client::TelegramClient;

use crate::metrics::Metrics;

pub struct Notifier {
    client: Option<TelegramClient>,
    metrics: Arc<Metrics>,
}

/// Notification events (label values for `telegram_notifications_total`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    DraftCreated,
    ValidationFailed,
    ProcessingFailed,
    JobFound,
    JobsDiscovered,
    RecruiterDiscovered,
    ApplicationSubmitted,
    ApplicationBlocked,
    ApplicationFailed,
    ApprovalRequired,
    InterviewRequest,
    RecruiterResponse,
    Error,
    Reply,
}

impl Event {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DraftCreated => "draft_created",
            Self::ValidationFailed => "validation_failed",
            Self::ProcessingFailed => "processing_failed",
            Self::JobFound => "job_found",
            Self::JobsDiscovered => "jobs_discovered",
            Self::RecruiterDiscovered => "recruiter_discovered",
            Self::ApplicationSubmitted => "application_submitted",
            Self::ApplicationBlocked => "application_blocked",
            Self::ApplicationFailed => "application_failed",
            Self::ApprovalRequired => "approval_required",
            Self::InterviewRequest => "interview_request",
            Self::RecruiterResponse => "recruiter_response",
            Self::Error => "error",
            Self::Reply => "reply",
        }
    }
}

impl Notifier {
    pub fn new(client: Option<TelegramClient>, metrics: Arc<Metrics>) -> Self {
        Self { client, metrics }
    }

    pub fn disabled(metrics: Arc<Metrics>) -> Self {
        Self::new(None, metrics)
    }

    pub fn enabled(&self) -> bool {
        self.client.is_some()
    }

    pub fn client(&self) -> Option<&TelegramClient> {
        self.client.as_ref()
    }

    /// Send a notification. Failures are logged and counted, never propagated:
    /// a Telegram outage must not break email processing.
    pub async fn notify(&self, event: Event, text: &str) -> Option<i64> {
        self.send(event, text, None).await
    }

    pub async fn send(&self, event: Event, text: &str, reply_to: Option<i64>) -> Option<i64> {
        let client = self.client.as_ref()?;
        self.metrics
            .telegram_requests_total
            .with_label_values(&["out"])
            .inc();
        match client.send(text, reply_to).await {
            Ok(id) => {
                self.metrics
                    .telegram_notifications_total
                    .with_label_values(&[event.as_str()])
                    .inc();
                Some(id)
            }
            Err(e) => {
                tracing::warn!(error = %e, event = event.as_str(), "telegram notification failed");
                self.metrics.failure("telegram");
                None
            }
        }
    }
}

/// Format helpers keep message layouts consistent with documents/API.md.
pub mod format {
    pub fn draft_created(
        sender: &str,
        subject: &str,
        account: &str,
        draft_id: &str,
        validation: &str,
    ) -> String {
        let head = if validation == "APPROVED" {
            "✉️ New draft generated"
        } else {
            "📝 New draft generated (REVIEW REQUIRED)"
        };
        format!("{head}\n\nSender: {sender}\nSubject: {subject}\nAccount: {account}\nDraft ID: {draft_id}")
    }

    pub fn validation_failed(sender: &str, subject: &str, notes: &[String]) -> String {
        format!(
            "⚠️ Draft needs review\n\nSender: {sender}\nSubject: {subject}\nIssues: {}",
            notes.join("; ")
        )
    }

    pub fn processing_failed(kind: &str, reference: &str, error: &str) -> String {
        format!("❌ Processing failed\n\nWorkflow: {kind}\nRef: {reference}\nError: {error}")
    }

    pub fn blocked(
        company: &str,
        role: &str,
        url: &str,
        reason: &str,
        question: &str,
        ref_code: &str,
    ) -> String {
        format!(
            "⚠️ Application Blocked\nCompany: {company}\nRole: {role}\nURL: {url}\nReason: {reason}\nQuestion: {question}\nRef: {ref_code}\n\nReply to this message with your answer (or \"skip\")."
        )
    }

    pub fn submitted(company: &str, role: &str, url: &str, score: Option<i64>) -> String {
        format!(
            "✅ Application Submitted\nCompany: {company}\nRole: {role}\nScore: {}\nURL: {url}",
            score.map(|s| s.to_string()).unwrap_or_else(|| "-".into())
        )
    }
}
