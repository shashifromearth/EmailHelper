//! Agents 2 + 3 (context retrieval / knowledge): prior discussions from
//! SQLite, the receiving person's profile and tone, and knowledge-base
//! snippets scoped to that person and purpose (plus `shared/`).

use serde::Serialize;

use crate::app::App;
use crate::db::emails::{self, EmailRecord};
use crate::db::knowledge::Snippet;
use crate::error::AppResult;
use crate::knowledge::{Person, Scope};
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
    /// Trusted text only (knowledge + the person's own details), used by the
    /// validator to detect invented facts.
    pub grounding: String,
    /// Third-party text (earlier thread messages + the incoming email). Never
    /// grounds facts; the validator flags values the draft copies from it.
    pub incoming: String,
    /// Prompt-injection flags found in earlier thread messages.
    pub injection_flags: Vec<String>,
}

// Kept small on purpose: on CPU-only machines reading the prompt is the
// slowest part of generation.
const MAX_KNOWLEDGE_CHARS: usize = 2200;
const MAX_THREAD_CHARS: usize = 1200;
const MAX_PROFILE_CHARS: usize = 900;
const SNIPPETS: usize = 3;

fn is_profile_doc(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.ends_with(".json") || name == "company_profile.md" || name == "tone.md"
}

pub async fn retrieve(
    app: &App,
    e: &EmailRecord,
    person: &Person,
    purpose: Option<&str>,
) -> AppResult<ContextBundle> {
    if let Err(err) = app.knowledge.reindex().await {
        tracing::warn!(error = %err, "knowledge reindex failed; using existing index");
    }
    let profile = &person.profile;

    let e2 = e.clone();
    let related = app.db.call(move |c| emails::related(c, &e2, 4)).await?;
    let mut thread_text = String::new();
    let mut injection_flags: Vec<String> = Vec::new();
    for r in &related {
        let s = prompts::sanitize_untrusted(&r.body_text, 500);
        for f in s.injection_flags {
            if !injection_flags.contains(&f) {
                injection_flags.push(f);
            }
        }
        let body = s.text;
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
    let scope = Scope::for_person(person, purpose);
    let snippets: Vec<Snippet> = app
        .knowledge
        .search_scoped(&query, SNIPPETS * 3, &scope)
        .await
        .unwrap_or_default();
    let mut knowledge_text = String::new();
    let mut sources = Vec::new();
    if !profile.company_profile.trim().is_empty() {
        knowledge_text.push_str("[about me]\n");
        knowledge_text.push_str(
            &profile
                .company_profile
                .chars()
                .take(MAX_PROFILE_CHARS)
                .collect::<String>(),
        );
        knowledge_text.push_str("\n\n");
        sources.push(format!("{}company_profile.md", prefix(person)));
    }
    let mut used = 0;
    for s in snippets {
        if knowledge_text.len() > MAX_KNOWLEDGE_CHARS || used >= SNIPPETS {
            break;
        }
        // Profile JSON files carry personal data that is irrelevant to replies.
        if is_profile_doc(&s.path) {
            continue;
        }
        knowledge_text.push_str(&format!("[{}]\n{}\n\n", s.title, s.content));
        used += 1;
        if !sources.contains(&s.path) {
            sources.push(s.path);
        }
    }

    let tone = if profile.tone.trim().is_empty() {
        "Professional, warm and concise.".to_string()
    } else {
        profile.tone.chars().take(600).collect()
    };
    // The person's own contact details may appear in their reply.
    let id = &profile.identity;
    let mut mine = vec![profile.signature_for(&person.name)];
    mine.extend(
        [&id.email, &id.phone, &id.linkedin_url, &id.github_url, &id.portfolio_url]
            .into_iter()
            .filter(|v| !v.trim().is_empty())
            .cloned(),
    );
    mine.extend(person.mailboxes.iter().map(|m| m.address.clone()));
    let grounding = format!("{knowledge_text}\n[my details]\n{}", mine.join("\n"));
    let incoming = format!("{thread_text}\n{}\n{}", e.subject, e.body_text);
    Ok(ContextBundle {
        user_name: person.name.clone(),
        signature: profile.signature_for(&person.name),
        tone,
        thread_count: related.len(),
        thread_text,
        knowledge_text: knowledge_text.trim().to_string(),
        sources,
        grounding,
        incoming,
        injection_flags,
    })
}

fn prefix(p: &Person) -> String {
    if p.rel_dir.is_empty() {
        String::new()
    } else {
        format!("{}/", p.rel_dir)
    }
}
