//! Agent 4: draft generation.

use crate::agents::classifier::Classification;
use crate::agents::context::ContextBundle;
use crate::db::emails::EmailRecord;
use crate::error::AppResult;
use crate::llm::{LlmRequest, LlmRouter, LlmTask};
use crate::prompts::{self, DraftPromptInput};

#[derive(Debug, Clone)]
pub struct Draft {
    pub subject: String,
    pub body: String,
    pub provider: String,
    pub model: String,
}

pub fn reply_subject(subject: &str) -> String {
    let s = subject.trim();
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("re:") || lower.starts_with("aw:") || lower.starts_with("sv:") {
        s.to_string()
    } else if s.is_empty() {
        "Re: (no subject)".to_string()
    } else {
        format!("Re: {s}")
    }
}

/// Remove artefacts models commonly add around the body.
pub fn clean_body(raw: &str) -> String {
    let mut t = raw.trim().to_string();
    for fence in ["```text", "```plaintext", "```markdown", "```"] {
        if let Some(rest) = t.strip_prefix(fence) {
            t = rest.trim().to_string();
        }
    }
    if let Some(rest) = t.strip_suffix("```") {
        t = rest.trim().to_string();
    }
    let lines: Vec<&str> = t
        .lines()
        .skip_while(|l| {
            let l = l.trim().to_ascii_lowercase();
            l.starts_with("subject:")
                || l.is_empty()
                || l.starts_with("here is")
                || l.starts_with("here's")
        })
        .collect();
    lines.join("\n").trim().to_string()
}

pub async fn generate(
    llm: &LlmRouter,
    e: &EmailRecord,
    c: &Classification,
    ctx: &ContextBundle,
) -> AppResult<Draft> {
    let body = prompts::sanitize_untrusted(&e.body_text, 6000);
    let from = prompts::sanitize_untrusted(&e.from, 300);
    let subject = prompts::sanitize_untrusted(&e.subject, 300);
    let input = DraftPromptInput {
        user_name: &ctx.user_name,
        signature: &ctx.signature,
        tone: &ctx.tone,
        category: c.category.as_str(),
        from: &from.text,
        subject: &subject.text,
        body: &body.text,
        thread: &ctx.thread_text,
        knowledge: &ctx.knowledge_text,
    };
    let req = LlmRequest::new(
        LlmTask::Draft,
        prompts::draft_system(&ctx.tone),
        prompts::draft_prompt(&input),
    )
    .max_tokens(700);
    let resp = llm.complete(&req).await?;
    let mut text = clean_body(&resp.text);
    // Ensure the signature is present exactly once at the end.
    let sig_first = ctx.signature.lines().last().unwrap_or("").trim();
    if !sig_first.is_empty() && !text.contains(sig_first) {
        text = format!("{text}\n\n{}", ctx.signature);
    }
    Ok(Draft {
        subject: reply_subject(&e.subject),
        body: text,
        provider: resp.provider,
        model: resp.model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subjects() {
        assert_eq!(reply_subject("Hello"), "Re: Hello");
        assert_eq!(reply_subject("RE: Hello"), "RE: Hello");
        assert_eq!(reply_subject(""), "Re: (no subject)");
    }

    #[test]
    fn cleans_model_artefacts() {
        assert_eq!(
            clean_body("```\nSubject: Hi\n\nHello there\n```"),
            "Hello there"
        );
        assert_eq!(clean_body("Here is the reply:\nHi Jane"), "Hi Jane");
    }
}
