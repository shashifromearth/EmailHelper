//! Agent 5: validation. Deterministic checks catch the common failure modes
//! (empty, placeholders, leaked prompts, invented contact details or prices,
//! risky promises, prompt injection); an optional LLM review adds a second
//! opinion. Anything suspicious becomes REVIEW_REQUIRED, never a hard failure.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::llm::{LlmRequest, LlmRouter, LlmTask};
use crate::prompts;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Verdict {
    Approved,
    ReviewRequired,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Approved => "APPROVED",
            Self::ReviewRequired => "REVIEW_REQUIRED",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Validation {
    pub verdict: Verdict,
    pub notes: Vec<String>,
    pub confidence: f32,
}

pub struct ValidationInput<'a> {
    pub draft: &'a str,
    /// Trusted + incoming text the draft may draw facts from.
    pub grounding: &'a str,
    pub classification_confidence: f32,
    pub injection_flags: &'a [String],
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("valid regex"))
}

fn emails_in(s: &str) -> HashSet<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}")
        .find_iter(s)
        .map(|m| m.as_str().to_ascii_lowercase())
        .collect()
}

fn urls_in(s: &str) -> HashSet<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r#"(?i)\bhttps?://[^\s<>"')]+"#)
        .find_iter(s)
        .map(|m| m.as_str().trim_end_matches(['.', ',']).to_ascii_lowercase())
        .collect()
}

fn money_in(s: &str) -> HashSet<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)(?:[$€£₹]\s?\d[\d,]*(?:\.\d+)?|\b\d[\d,]*(?:\.\d+)?\s?(?:usd|eur|gbp|inr|lpa|dollars|euros|rupees)\b)")
        .find_iter(s)
        .map(|m| {
            m.as_str()
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '.')
                .collect::<String>()
        })
        .filter(|d| !d.is_empty())
        .collect()
}

fn phones_in(s: &str) -> HashSet<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    // Spaces and tabs only: a number never continues on the next line
    // ("+91 98…" followed by a "3." list number).
    re(&R, r"\+?\d[\d \t().-]{8,}\d")
        .find_iter(s)
        .map(|m| {
            m.as_str()
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect::<String>()
        })
        .filter(|d| d.len() >= 9)
        .collect()
}

/// Deterministic validation rules.
pub fn rule_checks(i: &ValidationInput<'_>) -> Vec<String> {
    let mut notes = Vec::new();
    let draft = i.draft.trim();
    let lower = draft.to_lowercase();
    let word_count = draft.split_whitespace().count();

    if draft.is_empty() {
        notes.push("empty response".into());
        return notes;
    }
    if word_count < 8 {
        notes.push(format!("response too short ({word_count} words)"));
    }
    if word_count > 600 {
        notes.push(format!("response too long ({word_count} words)"));
    }
    static PLACEHOLDER: OnceLock<Regex> = OnceLock::new();
    if re(&PLACEHOLDER, r"(?i)\[[^\]\n]{0,40}\b(your|insert|recipient|company|organi[sz]ation|name|date|time|title|address|phone|email|position|role|link|url)\b[^\]\n]{0,40}\]|\{\{[^}]*\}\}|<insert|lorem ipsum|xx/xx")
        .is_match(draft)
    {
        notes.push("contains unfilled placeholder".into());
    }
    for leak in [
        "untrusted",
        "system prompt",
        "as an ai language model",
        "<<<",
        ">>>",
        "i cannot help with",
    ] {
        if lower.contains(leak) {
            notes.push(format!("possible prompt leakage or refusal (\"{leak}\")"));
            break;
        }
    }
    static PROMISE: OnceLock<Regex> = OnceLock::new();
    let grounding_lower = i.grounding.to_lowercase();
    for m in re(&PROMISE, r"(?i)\b(full refund|refund (you|your)|we will refund|guarantee[ds]?|discount of|% (off|discount)|free of charge|legally binding|we accept liability|waive)\b")
        .find_iter(draft)
    {
        if !grounding_lower.contains(&m.as_str().to_lowercase()) {
            notes.push(format!("policy: commitment \"{}\" not backed by knowledge base", m.as_str()));
        }
    }
    let ground_emails = emails_in(i.grounding);
    for e in emails_in(draft).difference(&ground_emails) {
        notes.push(format!(
            "hallucination risk: email address {e} not in sources"
        ));
    }
    let ground_urls = urls_in(i.grounding);
    for u in urls_in(draft) {
        if !ground_urls
            .iter()
            .any(|g| g.starts_with(&u) || u.starts_with(g.as_str()))
        {
            notes.push(format!("hallucination risk: link {u} not in sources"));
        }
    }
    let ground_money = money_in(i.grounding);
    for m in money_in(draft).difference(&ground_money) {
        notes.push(format!("hallucination risk: amount {m} not in sources"));
    }
    let ground_phones = phones_in(i.grounding);
    for p in phones_in(draft).difference(&ground_phones) {
        notes.push(format!(
            "hallucination risk: phone number {p} not in sources"
        ));
    }
    if !i.injection_flags.is_empty() {
        notes.push(format!(
            "incoming email contains possible prompt injection ({})",
            i.injection_flags.join(", ")
        ));
    }
    if i.classification_confidence < 0.5 {
        notes.push(format!(
            "low classification confidence ({:.2})",
            i.classification_confidence
        ));
    }
    notes
}

#[derive(Deserialize)]
struct LlmVerdict {
    verdict: String,
    #[serde(default)]
    issues: Vec<String>,
    #[serde(default)]
    confidence: Option<f32>,
}

/// Validate a draft. Rule checks always run; `use_llm` adds a second-opinion
/// LLM review (off by default for local models, where it costs a full call).
pub async fn validate(llm: &LlmRouter, i: &ValidationInput<'_>, use_llm: bool) -> Validation {
    let mut notes = rule_checks(i);
    if i.draft.trim().is_empty() {
        return Validation {
            verdict: Verdict::ReviewRequired,
            notes,
            confidence: 0.0,
        };
    }
    if !use_llm {
        let verdict = if notes.is_empty() {
            Verdict::Approved
        } else {
            Verdict::ReviewRequired
        };
        return Validation {
            verdict,
            notes,
            confidence: 0.75,
        };
    }
    let source = prompts::sanitize_untrusted(i.grounding, 6000).text;
    let draft = prompts::sanitize_untrusted(i.draft, 4000).text;
    let req = LlmRequest::new(
        LlmTask::Validate,
        prompts::validate_system(),
        prompts::validate_prompt(&source, &draft),
    )
    .json()
    .max_tokens(300);
    let confidence = match llm.complete_json::<LlmVerdict>(&req).await {
        Ok((v, _)) => {
            let confidence = v.confidence.unwrap_or(0.7).clamp(0.0, 1.0);
            if v.verdict.to_ascii_uppercase().contains("REVIEW") {
                if v.issues.is_empty() {
                    notes.push("LLM reviewer requested human review".into());
                }
                for issue in v.issues.into_iter().take(5) {
                    notes.push(format!(
                        "reviewer: {}",
                        issue.chars().take(200).collect::<String>()
                    ));
                }
            }
            if confidence < 0.5 {
                notes.push(format!("low reviewer confidence ({confidence:.2})"));
            }
            confidence
        }
        Err(e) => {
            tracing::warn!(error = %e, "LLM validation unavailable; rules only");
            notes.push("LLM review unavailable; rule checks only".into());
            0.5
        }
    };
    let verdict = if notes.is_empty() {
        Verdict::Approved
    } else {
        Verdict::ReviewRequired
    };
    Validation {
        verdict,
        notes,
        confidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(draft: &'a str, grounding: &'a str) -> ValidationInput<'a> {
        ValidationInput {
            draft,
            grounding,
            classification_confidence: 0.9,
            injection_flags: &[],
        }
    }

    #[test]
    fn clean_draft_passes_rules() {
        let n = rule_checks(&input(
            "Hi Jane, thanks for asking. Our standard plan costs $49 per seat per month. Best regards, Asha",
            "Standard plan: $49 per seat per month.",
        ));
        assert!(n.is_empty(), "{n:?}");
    }

    #[test]
    fn flags_empty_and_placeholders() {
        assert_eq!(rule_checks(&input("   ", "x")), vec!["empty response"]);
        let n = rule_checks(&input(
            "Hello [Recipient Name], thank you for your note, we will reply by {{date}} soon.",
            "",
        ));
        assert!(n.iter().any(|x| x.contains("placeholder")));
        let n = rule_checks(&input(
            "Thank you for your interest in [Example Company]. We will be in touch with next steps soon.",
            "",
        ));
        assert!(n.iter().any(|x| x.contains("placeholder")), "{n:?}");
        let ok = rule_checks(&input(
            "Thanks for the note about the [RFC] process; I will review it and reply this week.",
            "[RFC] process",
        ));
        assert!(!ok.iter().any(|x| x.contains("placeholder")), "{ok:?}");
    }

    #[test]
    fn numbers_on_separate_lines_are_not_joined() {
        let n = rule_checks(&input(
            "1. Contact Number: +91 9217154759
3. Email Address: a@b.test
27. Last Working Day: 19-06-2026
28. Joining: Immediately",
            "Phone number: +91 9217154759
Email: a@b.test
Last working day: 19-06-2026",
        ));
        assert!(!n.iter().any(|x| x.contains("phone")), "{n:?}");
    }

    #[test]
    fn flags_invented_facts() {
        let n = rule_checks(&input(
            "Hi, the price is $99 and you can email billing@acme.com or visit https://acme.com/buy. Call +1 415 555 0100.",
            "Our plan is $49.",
        ));
        assert!(n.iter().any(|x| x.contains("amount 99")));
        assert!(n.iter().any(|x| x.contains("billing@acme.com")));
        assert!(n.iter().any(|x| x.contains("https://acme.com/buy")));
        assert!(n.iter().any(|x| x.contains("phone")));
    }

    #[test]
    fn flags_unbacked_promises() {
        let n = rule_checks(&input(
            "We are sorry. We will refund your order and offer a discount of 20% off next time, guaranteed.",
            "Refunds require manager approval.",
        ));
        assert!(n.iter().any(|x| x.starts_with("policy")));
    }

    #[test]
    fn flags_injection_and_low_confidence() {
        let flags = vec!["ignore_instructions".to_string()];
        let n = rule_checks(&ValidationInput {
            draft: "Thanks for the message, I will look into it and get back to you shortly.",
            grounding: "",
            classification_confidence: 0.3,
            injection_flags: &flags,
        });
        assert!(n.iter().any(|x| x.contains("prompt injection")));
        assert!(n
            .iter()
            .any(|x| x.contains("low classification confidence")));
    }
}
