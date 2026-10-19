//! Agents 2 + 3 (context retrieval / knowledge): prior discussions from
//! SQLite, the user and company profile, tone guidance, approved responses and
//! knowledge-base snippets via FTS5.

use serde::Serialize;

use crate::app::App;
use crate::db::emails::{self, EmailRecord};
use crate::db::knowledge::Snippet;
use crate::error::AppResult;
use crate::prompts;

#[derive(Debug, Clone, Serialize, Default)]
pub struct ContextBundle {
    pub user_name: String,
    pub signature: String,
    pub tone: String,
    pub thread_text: String,
    pub thread_count: usize,
    pub knowledge_text: String,
    pub sources: Vec<String>,
    /// All trusted text, used by the validator to detect invented facts.
    pub grounding: String,
}

const MAX_KNOWLEDGE_CHARS: usize = 5000;
const MAX_THREAD_CHARS: usize = 3000;

pub async fn retrieve(app: &App, e: &EmailRecord) -> AppResult<ContextBundle> {
    if let Err(err) = app.knowledge.reindex().await {
        tracing::warn!(error = %err, "knowledge reindex failed; using existing index");
    }
    let profile = app.knowledge.profile();

    let e2 = e.clone();
    let related = app.db.call(move |c| emails::related(c, &e2, 5)).await?;
    let mut thread_text = String::new();
    for r in &related {
        let body = prompts::sanitize_untrusted(&r.body_text, 800).text;
        thread_text.push_str(&format!(
            "--- {} | {} | {}\n{}\n",
            r.received_at.as_deref().unwrap_or(&r.created_at),
            r.from,
            r.subject,
            body
        ));
        if thread_text.len() > MAX_THREAD_CHARS {
            break;
        }
    }

    let query = format!(
        "{} {}",
        e.subject,
        e.body_text.chars().take(1500).collect::<String>()
    );
    let snippets: Vec<Snippet> = app.knowledge.search(&query, 6).await.unwrap_or_default();
    let mut knowledge_text = String::new();
    let mut sources = Vec::new();
    if !profile.company_profile.trim().is_empty() {
        knowledge_text.push_str("[company_profile.md]\n");
        knowledge_text.push_str(
            &profile
                .company_profile
                .chars()
                .take(1500)
                .collect::<String>(),
        );
        knowledge_text.push_str("\n\n");
        sources.push("company_profile.md".to_string());
    }
    for s in snippets {
        if knowledge_text.len() > MAX_KNOWLEDGE_CHARS {
            break;
        }
        // Profile JSON files carry personal data that is irrelevant to replies.
        if s.path.ends_with(".json") || s.path == "company_profile.md" || s.path == "tone.md" {
            continue;
        }
        knowledge_text.push_str(&format!("[{}: {}]\n{}\n\n", s.path, s.title, s.content));
        if !sources.contains(&s.path) {
            sources.push(s.path);
        }
    }

    let tone = if profile.tone.trim().is_empty() {
        "Professional, warm and concise.".to_string()
    } else {
        profile.tone.chars().take(1200).collect()
    };
    let grounding = format!(
        "{knowledge_text}\n{thread_text}\n{}\n{}",
        e.subject, e.body_text
    );
    Ok(ContextBundle {
        user_name: profile.display_name().to_string(),
        signature: profile.signature(),
        tone,
        thread_count: related.len(),
        thread_text,
        knowledge_text: knowledge_text.trim().to_string(),
        sources,
        grounding,
    })
}
