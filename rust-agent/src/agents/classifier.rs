//! Agent 1: classification. Combines cheap deterministic rules with the LLM
//! and degrades gracefully to the rules when the model is unavailable.

use serde::{Deserialize, Serialize};

use crate::db::emails::EmailRecord;
use crate::jobs::detect::{self, JobSignal};
use crate::llm::{LlmRequest, LlmRouter, LlmTask};
use crate::prompts;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Support,
    Sales,
    Complaint,
    Partnership,
    Recruiting,
    Invoice,
    FollowUp,
    Spam,
    Other,
}

impl Category {
    pub fn parse(s: &str) -> Self {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace(['-', ' '], "_")
            .as_str()
        {
            "support" => Self::Support,
            "sales" => Self::Sales,
            "complaint" => Self::Complaint,
            "partnership" => Self::Partnership,
            "recruiting" | "recruitment" | "job" | "jobs" => Self::Recruiting,
            "invoice" | "billing" => Self::Invoice,
            "newsletter" | "notification" | "marketing" | "promotion" => Self::Other,
            "follow_up" | "followup" => Self::FollowUp,
            "spam" | "junk" => Self::Spam,
            _ => Self::Other,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Support => "support",
            Self::Sales => "sales",
            Self::Complaint => "complaint",
            Self::Partnership => "partnership",
            Self::Recruiting => "recruiting",
            Self::Invoice => "invoice",
            Self::FollowUp => "follow_up",
            Self::Spam => "spam",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Classification {
    pub category: Category,
    pub confidence: f32,
    pub is_job_opportunity: bool,
    pub needs_reply: bool,
    pub urgency: String,
    pub summary: String,
    /// "llm", "rules" or "llm+rules".
    pub method: String,
    pub job_signal: Option<JobSignal>,
    pub injection_flags: Vec<String>,
}

#[derive(Deserialize)]
struct LlmClassification {
    category: String,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    is_job_opportunity: Option<bool>,
    #[serde(default)]
    needs_reply: Option<bool>,
    #[serde(default)]
    urgency: Option<String>,
    #[serde(default)]
    summary: Option<String>,
}

/// Senders whose mail is automated and must never get a reply draft.
pub fn is_automated_sender(from_email: &str) -> bool {
    let local = from_email.split('@').next().unwrap_or("");
    let l = local.to_ascii_lowercase();
    [
        "noreply",
        "no-reply",
        "no_reply",
        "donotreply",
        "do-not-reply",
        "notifications",
        "notification",
        "alerts",
        "alert",
        "mailer-daemon",
        "postmaster",
        "newsletter",
        "jobalerts",
        "jobs-noreply",
        "bounce",
    ]
    .iter()
    .any(|p| {
        l == *p
            || l.starts_with(&format!("{p}-"))
            || l.starts_with(&format!("{p}+"))
            || (p.len() > 6 && l.contains(p))
    })
}

/// Deterministic rules. Returns a category guess with low confidence.
pub fn rule_based(e: &EmailRecord) -> (Category, f32) {
    let text = format!("{} {}", e.subject, e.body_text).to_lowercase();
    let has = |ws: &[&str]| ws.iter().any(|w| text.contains(w));
    if has(&[
        "claim your prize",
        "you have won",
        "lottery",
        "crypto giveaway",
        "wire transfer fee",
        "viagra",
    ]) {
        return (Category::Spam, 0.6);
    }
    if has(&[
        "invoice",
        "amount due",
        "payment due",
        "remittance",
        "billing statement",
    ]) {
        return (Category::Invoice, 0.55);
    }
    if has(&[
        "unacceptable",
        "very disappointed",
        "formal complaint",
        "worst service",
        "escalate this",
    ]) {
        return (Category::Complaint, 0.55);
    }
    if has(&[
        "partnership",
        "partner with",
        "collaboration opportunity",
        "co-marketing",
    ]) {
        return (Category::Partnership, 0.5);
    }
    if has(&[
        "pricing",
        "quote",
        "purchase order",
        "book a demo",
        "how much",
    ]) {
        return (Category::Sales, 0.5);
    }
    if has(&[
        "following up",
        "follow up on",
        "just checking in",
        "any update on",
    ]) {
        return (Category::FollowUp, 0.5);
    }
    (Category::Support, 0.3)
}

/// Classify an email. Stale mail (older than the configured age limit) and
/// mail from automated senders is classified by rules only: no LLM call and
/// no reply draft. Job signals are still detected deterministically.
pub async fn classify(llm: &LlmRouter, e: &EmailRecord, stale: bool) -> Classification {
    let job_signal = detect::detect_job_email(e);
    let automated = is_automated_sender(&e.from_email);
    // The opening of a message is enough to classify it, and a shorter
    // prompt is much faster on CPU-only machines.
    let body = prompts::sanitize_untrusted(&e.body_text, 2500);
    let subject = prompts::sanitize_untrusted(&e.subject, 300);
    let from = prompts::sanitize_untrusted(&e.from, 300);
    let mut flags = body.injection_flags.clone();
    flags.extend(subject.injection_flags.iter().cloned());

    let req = LlmRequest::new(
        LlmTask::Classify,
        prompts::classify_system(),
        prompts::classify_prompt(&from.text, &subject.text, &body.text),
    )
    .json()
    .max_tokens(300);

    let (rule_cat, rule_conf) = rule_based(e);
    let skip_llm = if stale {
        Some("rules:stale")
    } else if automated {
        Some("rules:automated")
    } else if job_signal.is_some() {
        // Job mail is recognised reliably by rules; skipping the model here
        // saves a slow LLM call per recruiter email on CPU-only machines.
        Some("rules:job")
    } else {
        None
    };
    let result = match skip_llm {
        Some(_) => None,
        None => Some(llm.complete_json::<LlmClassification>(&req).await),
    };
    let mut c = match result {
        Some(Ok((v, _))) => Classification {
            category: Category::parse(&v.category),
            confidence: v.confidence.unwrap_or(0.6).clamp(0.0, 1.0),
            is_job_opportunity: v.is_job_opportunity.unwrap_or(false),
            needs_reply: v.needs_reply.unwrap_or(true),
            urgency: v.urgency.unwrap_or_else(|| "normal".into()),
            summary: v.summary.unwrap_or_default().chars().take(300).collect(),
            method: "llm".into(),
            job_signal: None,
            injection_flags: vec![],
        },
        Some(Err(err)) => {
            tracing::warn!(error = %err, email_id = %e.id, "LLM classification failed; using rules");
            Classification {
                category: rule_cat,
                confidence: rule_conf,
                is_job_opportunity: false,
                needs_reply: true,
                urgency: "normal".into(),
                summary: String::new(),
                method: "rules".into(),
                job_signal: None,
                injection_flags: vec![],
            }
        }
        None => Classification {
            category: if skip_llm == Some("rules:job") {
                Category::Recruiting
            } else {
                rule_cat
            },
            confidence: if skip_llm == Some("rules:job") {
                0.85
            } else {
                rule_conf
            },
            is_job_opportunity: false,
            // Stale and automated mail never needs a reply; job mail does
            // unless it's an alert (decided below from the job signal).
            needs_reply: skip_llm == Some("rules:job"),
            urgency: "low".into(),
            summary: String::new(),
            method: skip_llm.unwrap_or("rules").into(),
            job_signal: None,
            injection_flags: vec![],
        },
    };

    // Deterministic signals override the model where they are reliable.
    if let Some(sig) = &job_signal {
        c.is_job_opportunity = true;
        if c.category != Category::Spam {
            c.category = Category::Recruiting;
        }
        if c.method == "llm" {
            c.method = "llm+rules".into();
        }
        if sig.is_alert {
            c.needs_reply = false;
        }
    }
    if automated {
        c.needs_reply = false;
    }
    if c.category == Category::Spam {
        c.needs_reply = false;
        c.is_job_opportunity = false;
    }
    c.job_signal = job_signal;
    c.injection_flags = flags;
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automated_senders() {
        assert!(is_automated_sender("noreply@github.com"));
        assert!(is_automated_sender("jobalerts-noreply@linkedin.com"));
        assert!(is_automated_sender("notifications@service.com"));
        assert!(!is_automated_sender("jane.doe@acme.com"));
        assert!(!is_automated_sender("alexandra@acme.com"));
    }

    #[test]
    fn category_parsing() {
        assert_eq!(Category::parse("Follow-up"), Category::FollowUp);
        assert_eq!(Category::parse("RECRUITING"), Category::Recruiting);
        assert_eq!(Category::parse("weird"), Category::Other);
    }
}
