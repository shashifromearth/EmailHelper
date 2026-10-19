//! Cover-letter generation grounded in the user's profile and knowledge base.

use crate::db::jobs::JobDetails;
use crate::knowledge::{KnowledgeBase, Profile};
use crate::llm::{LlmRequest, LlmRouter, LlmTask};
use crate::prompts;

const MIN_WORDS: usize = 90;
const MAX_WORDS: usize = 450;

pub fn template(job: &JobDetails, p: &Profile) -> String {
    let company = job.company.as_deref().unwrap_or("your company");
    let skills = p
        .skills
        .core
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let exp = if p.resume.total_experience_years > 0.0 {
        format!("{} years of experience", p.resume.total_experience_years)
    } else {
        "extensive experience".to_string()
    };
    format!(
        "Dear Hiring Team at {company},\n\n\
I am writing to apply for the {title} role. I bring {exp} building and leading production systems, \
with deep expertise in {skills}. {summary}\n\n\
I am particularly drawn to this position because it combines hands-on engineering with technical \
leadership, and I am confident I can contribute quickly to your team's goals.\n\n\
Thank you for your consideration. I would welcome the chance to discuss how I can help {company}.\n\n\
Kind regards,\n{name}",
        title = job.title,
        summary = p.resume.summary,
        name = p.display_name(),
    )
}

fn acceptable(text: &str) -> bool {
    let words = text.split_whitespace().count();
    let lower = text.to_lowercase();
    (MIN_WORDS..=MAX_WORDS).contains(&words)
        && !lower.contains("[your")
        && !lower.contains("{{")
        && !lower.contains("untrusted")
        && !lower.contains("as an ai")
}

pub async fn generate(
    llm: &LlmRouter,
    kb: &KnowledgeBase,
    job: &JobDetails,
    p: &Profile,
    scope: &crate::knowledge::Scope,
) -> String {
    let query = format!("{} {}", job.title, job.skills.join(" "));
    let mut evidence = String::new();
    for sn in kb.search_scoped(&query, 5, scope).await.unwrap_or_default() {
        if sn.path.contains("projects")
            || sn.path.contains("employment")
            || sn.path.contains("resume")
            || sn.path.contains("achievements")
        {
            evidence.push_str(&format!(
                "- {}\n",
                sn.content.chars().take(500).collect::<String>()
            ));
        }
    }
    let desc = prompts::sanitize_untrusted(job.description.as_deref().unwrap_or(""), 4000).text;
    let req = LlmRequest::new(
        LlmTask::CoverLetter,
        format!(
            "You write concise, specific cover letters (180–320 words) in the candidate's voice. \
Use only facts from CANDIDATE and EVIDENCE; never invent employers, numbers or credentials. \
No placeholders, no markdown. End with 'Kind regards,' and the candidate's name. {}",
            prompts::DATA_ONLY_RULE
        ),
        format!(
            "CANDIDATE:\n{}\nEVIDENCE:\n{}\nROLE: {} at {}\n\nJOB DESCRIPTION:\n{}",
            p.summary_text(),
            if evidence.is_empty() {
                "(none)"
            } else {
                &evidence
            },
            job.title,
            job.company.as_deref().unwrap_or("the company"),
            prompts::untrusted_block("JOB", &desc)
        ),
    )
    .max_tokens(700);
    match llm.complete(&req).await {
        Ok(r) => {
            let mut t = crate::agents::drafter::clean_body(&r.text);
            if !t.contains(p.display_name()) && p.display_name() != "the user" {
                t = format!("{t}\n{}", p.display_name());
            }
            if acceptable(&t) {
                return t;
            }
            tracing::warn!("generated cover letter failed checks; using template");
            template(job, p)
        }
        Err(e) => {
            tracing::warn!(error = %e, "cover letter generation failed; using template");
            template(job, p)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_is_acceptable() {
        let mut p = Profile::default();
        p.identity.full_name = "Asha Rao".into();
        p.skills.core = vec!["Rust".into(), "Python".into()];
        p.resume.total_experience_years = 12.0;
        p.resume.summary = "I have led AI platform teams that shipped retrieval and evaluation systems used by millions of customers across several product lines.".into();
        let j = JobDetails {
            title: "Staff AI Engineer".into(),
            company: Some("Acme".into()),
            ..Default::default()
        };
        let t = template(&j, &p);
        assert!(acceptable(&t), "{}", t.split_whitespace().count());
        assert!(t.contains("Acme"));
        assert!(t.ends_with("Asha Rao"));
    }
}
