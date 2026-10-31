//! Form answers for the Chrome extension: it reads a page's fields, the agent
//! answers them with the same rules as its own applications (profile →
//! remembered answers → RAG + LLM; sensitive and legal questions go to the
//! person), and the person submits.

use std::sync::Arc;

use futures::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app::App;
use crate::db::jobs::JobDetails;
use crate::error::{AppError, AppResult};
use crate::jobs::answers::{self, Answer};
use crate::jobs::browser::Field;
use crate::jobs::{application, cover_letter};
use crate::knowledge::people::JOB_SEEKING;
use crate::knowledge::Scope;

/// More than this is not an application form.
const MAX_FIELDS: usize = 80;
/// Fields answered at once (each may call the LLM).
const PARALLEL: usize = 4;

#[derive(Debug, Deserialize)]
pub struct AnswersRequest {
    #[serde(default)]
    pub person: Option<String>,
    #[serde(default)]
    pub job: JobDetails,
    pub fields: Vec<Field>,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldAnswer {
    Value { id: String, value: Value },
    /// Upload the person's resume chosen for this job.
    Resume { id: String },
    Skip { id: String },
    /// Left for the person, with the reason.
    Ask { id: String, reason: String },
}

#[derive(Debug, Serialize)]
pub struct AnswersResponse {
    pub person: String,
    pub answers: Vec<FieldAnswer>,
}

pub async fn answer(app: &Arc<App>, req: AnswersRequest) -> AppResult<AnswersResponse> {
    if req.fields.len() > MAX_FIELDS {
        return Err(AppError::Validation(format!("at most {MAX_FIELDS} fields")));
    }
    let person = crate::jobs::person_by_id(app, req.person.as_deref())
        .ok_or_else(|| AppError::NotFound(format!("person {}", req.person.as_deref().unwrap_or("-"))))?;
    if let Some(problem) = application::profile_problem(&person.profile) {
        return Err(AppError::Validation(problem));
    }
    let scope = Scope::for_person(&person, Some(JOB_SEEKING));
    let memory_ns = application::memory_ns(&person);
    let job = &req.job;
    let job_ctx = format!(
        "{} at {}\nLocation: {}\n{}",
        job.title,
        job.company.as_deref().unwrap_or(""),
        job.location.as_deref().unwrap_or(""),
        job.description.as_deref().unwrap_or("")
    );
    let answered: Vec<(Field, Answer)> = stream::iter(req.fields.iter().cloned())
        .map(|f| {
            let (person, scope, job_ctx, memory_ns) = (&person, &scope, &job_ctx, &memory_ns);
            async move {
                let a = answers::answer_field(&f, &person.profile, &app.db, &app.knowledge, &app.llm, job_ctx, scope, memory_ns)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, field = %f.id, "form answer failed");
                        Answer::Ask { reason: "could not answer".into() }
                    });
                (f, a)
            }
        })
        .buffered(PARALLEL)
        .collect()
        .await;

    // One cover letter, written only when the form asks for it.
    let cover = if answered.iter().any(|(_, a)| *a == Answer::CoverLetter) {
        Some(cover_letter::generate(&app.llm, &app.knowledge, job, &person.profile, &scope).await)
    } else {
        None
    };
    let answers = answered
        .into_iter()
        .map(|(f, a)| match a {
            Answer::Value(value) => FieldAnswer::Value { id: f.id, value },
            Answer::CoverLetter => FieldAnswer::Value { id: f.id, value: Value::String(cover.clone().unwrap_or_default()) },
            Answer::ResumeFile => FieldAnswer::Resume { id: f.id },
            Answer::Skip => FieldAnswer::Skip { id: f.id },
            Answer::Ask { reason } => FieldAnswer::Ask { id: f.id, reason },
        })
        .collect();
    Ok(AnswersResponse { person: person.id.clone(), answers })
}

#[derive(Debug, Deserialize)]
pub struct GivenAnswer {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct RememberRequest {
    #[serde(default)]
    pub person: Option<String>,
    pub answers: Vec<GivenAnswer>,
}

/// Answers the person typed into questions left for them, reused the next
/// time the same question comes up. Sensitive, self-identification and legal
/// questions are never remembered: those stay the person's call every time.
pub async fn remember(app: &Arc<App>, req: RememberRequest) -> AppResult<usize> {
    let person = crate::jobs::person_by_id(app, req.person.as_deref())
        .ok_or_else(|| AppError::NotFound(format!("person {}", req.person.as_deref().unwrap_or("-"))))?;
    let ns = application::memory_ns(&person);
    let keep: Vec<(String, String)> = req
        .answers
        .into_iter()
        .filter(|a| a.label.trim().chars().count() >= 3 && !a.value.trim().is_empty())
        .filter(|a| {
            !answers::is_sensitive(&a.label)
                && !answers::is_self_identification(&a.label)
                && !answers::is_legal_agreement(&a.label, &[])
        })
        .take(MAX_FIELDS)
        .map(|a| (format!("{ns}{}", a.label.trim()), a.value.trim().chars().take(2000).collect()))
        .collect();
    let n = keep.len();
    app.db
        .call(move |c| keep.iter().try_for_each(|(q, a)| crate::db::jobs::remember_answer(c, q, a)))
        .await?;
    Ok(n)
}
