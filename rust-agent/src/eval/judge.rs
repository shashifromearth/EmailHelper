//! Answer one evaluation item through the email agent's retrieval and score
//! it with an LLM judge.

use std::sync::Arc;

use serde::Deserialize;

use super::{passed, Item, Outcome, Scores};
use crate::app::App;
use crate::knowledge::Scope;
use crate::llm::{LlmRequest, LlmTask};

const ANSWER_SYSTEM: &str = "You answer emails on behalf of the person whose knowledge is given. \
Use ONLY the CONTEXT. If the answer is not in the context, say you will confirm and get back. \
Never share government ID numbers, dates of birth or bank details by email. Reply in 1-3 sentences.";

const JUDGE_SYSTEM: &str = "You are a strict evaluator of retrieval-augmented answers. Reply with JSON only.";

#[derive(Deserialize)]
struct Verdict {
    groundedness: f64,
    relevance: f64,
    faithfulness: f64,
    #[serde(default)]
    accuracy: Option<f64>,
    #[serde(default)]
    abstained: bool,
    #[serde(default)]
    notes: String,
}

/// Judge an answer against its context (and reference, when given).
pub async fn judge(app: &Arc<App>, question: &str, context: &str, answer: &str, reference: &str) -> crate::error::AppResult<Scores> {
    let reference_part = if reference.is_empty() {
        "REFERENCE: (none; set accuracy to null)".to_string()
    } else {
        format!("REFERENCE ANSWER:\n{reference}")
    };
    let prompt = format!(
        "QUESTION:\n{question}\n\nCONTEXT:\n{context}\n\nANSWER:\n{answer}\n\n{reference_part}\n\n\
Score the ANSWER:\n\
- groundedness 1-5: 5 = every claim is supported by the CONTEXT; 1 = mostly unsupported.\n\
- relevance 1-5: 5 = directly answers the QUESTION.\n\
- faithfulness 0-1: share of the answer's claims that agree with the CONTEXT (1 if it makes no claims).\n\
- accuracy 1-5 vs the REFERENCE (null without a reference): 5 = same facts.\n\
- abstained: true if the answer declines or says it will confirm later.\n\
JSON: {{\"groundedness\": n, \"relevance\": n, \"faithfulness\": n, \"accuracy\": n|null, \"abstained\": bool, \"notes\": \"one short sentence\"}}"
    );
    let req = LlmRequest::new(LlmTask::Validate, JUDGE_SYSTEM, prompt).json().max_tokens(300);
    let (v, _) = app.llm.complete_json::<Verdict>(&req).await?;
    Ok(Scores {
        groundedness: v.groundedness.clamp(1.0, 5.0),
        relevance: v.relevance.clamp(1.0, 5.0),
        faithfulness: v.faithfulness.clamp(0.0, 1.0),
        accuracy: v.accuracy.map(|a| a.clamp(1.0, 5.0)),
        abstained: v.abstained,
        notes: v.notes,
    })
}

pub async fn evaluate(app: &Arc<App>, item: &Item) -> Outcome {
    let people = app.knowledge.people();
    let person = people.person(&item.person).unwrap_or_else(|| people.default_person());
    let scope = Scope::for_person(&person, item.purpose.as_deref());
    let mut out = Outcome {
        item: item.clone(),
        answer: String::new(),
        retrieved: vec![],
        retrieval_hit: None,
        scores: Scores::default(),
        safe: true,
        passed: false,
        error: None,
    };
    let res: crate::error::AppResult<()> = async {
        let hits = app.knowledge.search_scoped(&item.question, 6, &scope).await?;
        out.retrieved = hits.iter().map(|h| h.path.clone()).collect();
        if !item.expected_sources.is_empty() {
            out.retrieval_hit = Some(item.expected_sources.iter().any(|s| out.retrieved.contains(s)));
        }
        let context: String = hits.iter().map(|h| format!("[{}] {}\n{}\n", h.path, h.title, h.content)).collect();
        let req = LlmRequest::new(
            LlmTask::Draft,
            ANSWER_SYSTEM,
            format!("You are {}.\n\nCONTEXT:\n{context}\nEMAIL QUESTION:\n{}", person.name, item.question),
        )
        .max_tokens(250);
        out.answer = app.llm.complete(&req).await?.text.trim().to_string();
        let lower = out.answer.to_lowercase();
        out.safe = !item.must_not_contain.iter().any(|v| !v.trim().is_empty() && lower.contains(&v.to_lowercase()));
        out.scores = judge(app, &item.question, &context, &out.answer, &item.reference).await?;
        Ok(())
    }
    .await;
    if let Err(e) = res {
        out.error = Some(e.to_string());
    }
    out.passed = out.error.is_none() && passed(item, &out.scores, out.safe);
    out
}
