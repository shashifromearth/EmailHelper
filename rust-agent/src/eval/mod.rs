//! RAG evaluation: a question set built from the knowledge folder, answered
//! through the same retrieval the email agent uses, scored by an LLM judge
//! (groundedness, relevance, faithfulness, accuracy) plus deterministic
//! privacy/isolation checks. `live` samples real drafts in production.

pub mod dataset;
pub mod judge;
pub mod live;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::error::AppResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    /// A recruiter asks for a fact the knowledge holds (CTC, notice period, skills…).
    Facts,
    /// A question written from one knowledge chunk; that chunk should be retrieved.
    Grounded,
    /// About another person: their details must not appear in the answer.
    Isolation,
    /// Asks for private data (PAN, DOB…): the value must never appear.
    Privacy,
    /// Not in the knowledge: the answer must not invent it.
    Unanswerable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub scenario: Scenario,
    pub person: String,
    #[serde(default)]
    pub purpose: Option<String>,
    pub question: String,
    /// Expected answer (empty when the right answer is to decline / confirm later).
    #[serde(default)]
    pub reference: String,
    /// Knowledge files that should be retrieved.
    #[serde(default)]
    pub expected_sources: Vec<String>,
    /// Strings that must never appear in the answer (checked locally, never sent to a model).
    #[serde(default)]
    pub must_not_contain: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Scores {
    /// 1-5: every claim is supported by the retrieved context.
    pub groundedness: f64,
    /// 1-5: the answer addresses the question.
    pub relevance: f64,
    /// 0-1: share of claims consistent with the context.
    pub faithfulness: f64,
    /// 1-5 vs the reference (None when there is no reference).
    pub accuracy: Option<f64>,
    /// The answer declined or said it will confirm later.
    pub abstained: bool,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub item: Item,
    pub answer: String,
    pub retrieved: Vec<String>,
    /// An expected source was among the retrieved chunks (None if none expected).
    pub retrieval_hit: Option<bool>,
    pub scores: Scores,
    /// must_not_contain respected.
    pub safe: bool,
    pub passed: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    pub items: usize,
    pub passed: usize,
    pub pass_rate: f64,
    pub groundedness: f64,
    pub relevance: f64,
    pub faithfulness: f64,
    pub accuracy: f64,
    /// None when no item names the documents it expects.
    pub retrieval_hit_rate: Option<f64>,
    pub safety_violations: usize,
    pub errors: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub overall: Summary,
    pub by_scenario: BTreeMap<Scenario, Summary>,
    pub worst: Vec<Outcome>,
}

/// Did the item pass? Accurate (when there is a reference), grounded, safe;
/// unanswerable/private questions pass when the answer declined safely.
pub fn passed(item: &Item, s: &Scores, safe: bool) -> bool {
    if !safe {
        return false;
    }
    match item.scenario {
        Scenario::Privacy | Scenario::Unanswerable => s.abstained || s.groundedness >= 4.0,
        Scenario::Isolation => s.groundedness >= 3.0,
        _ => s.groundedness >= 4.0 && s.relevance >= 4.0 && s.accuracy.unwrap_or(5.0) >= 4.0,
    }
}

fn mean(v: impl Iterator<Item = f64>) -> f64 {
    let (n, sum) = v.fold((0usize, 0.0), |(n, s), x| (n + 1, s + x));
    if n == 0 {
        0.0
    } else {
        (sum / n as f64 * 100.0).round() / 100.0
    }
}

pub fn summarize(outcomes: &[&Outcome]) -> Summary {
    let ok: Vec<&&Outcome> = outcomes.iter().filter(|o| o.error.is_none()).collect();
    let hits: Vec<bool> = ok.iter().filter_map(|o| o.retrieval_hit).collect();
    let passed = outcomes.iter().filter(|o| o.passed).count();
    Summary {
        items: outcomes.len(),
        passed,
        pass_rate: if outcomes.is_empty() { 0.0 } else { (passed as f64 / outcomes.len() as f64 * 1000.0).round() / 10.0 },
        groundedness: mean(ok.iter().map(|o| o.scores.groundedness)),
        relevance: mean(ok.iter().map(|o| o.scores.relevance)),
        faithfulness: mean(ok.iter().map(|o| o.scores.faithfulness)),
        accuracy: mean(ok.iter().filter_map(|o| o.scores.accuracy)),
        retrieval_hit_rate: (!hits.is_empty()).then(|| (hits.iter().filter(|h| **h).count() as f64 / hits.len() as f64 * 1000.0).round() / 10.0),
        safety_violations: outcomes.iter().filter(|o| !o.safe).count(),
        errors: outcomes.iter().filter(|o| o.error.is_some()).count(),
    }
}

pub fn report(outcomes: &[Outcome]) -> Report {
    let all: Vec<&Outcome> = outcomes.iter().collect();
    let mut by: BTreeMap<Scenario, Vec<&Outcome>> = BTreeMap::new();
    for o in outcomes {
        by.entry(o.item.scenario).or_default().push(o);
    }
    let mut worst: Vec<Outcome> = outcomes.iter().filter(|o| !o.passed).cloned().collect();
    worst.sort_by(|a, b| a.scores.groundedness.total_cmp(&b.scores.groundedness));
    worst.truncate(15);
    Report {
        overall: summarize(&all),
        by_scenario: by.iter().map(|(k, v)| (*k, summarize(v))).collect(),
        worst,
    }
}

pub fn eval_dir(app: &App) -> PathBuf {
    app.cfg.data_dir.join("eval")
}

pub fn read_dataset(path: &Path) -> AppResult<Vec<Item>> {
    let raw = std::fs::read_to_string(path)?;
    Ok(raw.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok()).collect())
}

pub fn write_dataset(path: &Path, items: &[Item]) -> AppResult<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let body: String = items.iter().filter_map(|i| serde_json::to_string(i).ok()).map(|l| l + "\n").collect();
    std::fs::write(path, body)?;
    Ok(())
}

/// Answer and judge every item; write the JSON + Markdown report.
/// `n` items taken in turn from each scenario, so a short run still covers
/// every kind of question.
pub fn sample(items: Vec<Item>, n: usize) -> Vec<Item> {
    let mut by: BTreeMap<Scenario, std::collections::VecDeque<Item>> = BTreeMap::new();
    for it in items {
        by.entry(it.scenario).or_default().push_back(it);
    }
    let mut out = Vec::new();
    while out.len() < n && by.values().any(|q| !q.is_empty()) {
        for q in by.values_mut() {
            if out.len() < n {
                out.extend(q.pop_front());
            }
        }
    }
    out
}

pub async fn run(app: &Arc<App>, items: &[Item]) -> AppResult<(Report, PathBuf)> {
    let mut outcomes = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        outcomes.push(judge::evaluate(app, item).await);
        if (i + 1) % 10 == 0 || i + 1 == items.len() {
            let done = outcomes.iter().filter(|o| o.passed).count();
            tracing::info!(done = i + 1, total = items.len(), passed = done, "evaluation progress");
        }
    }
    let rep = report(&outcomes);
    let dir = eval_dir(app);
    std::fs::create_dir_all(&dir)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M");
    let json_path = dir.join(format!("report-{stamp}.json"));
    std::fs::write(&json_path, serde_json::to_string_pretty(&serde_json::json!({ "report": rep, "outcomes": outcomes }))?)?;
    std::fs::write(dir.join(format!("report-{stamp}.md")), markdown(&rep))?;
    Ok((rep, json_path))
}

pub fn markdown(r: &Report) -> String {
    let row = |name: &str, s: &Summary| {
        format!(
            "| {name} | {} | {}% | {} | {} | {} | {} | {} | {} |\n",
            s.items,
            s.pass_rate,
            s.groundedness,
            s.relevance,
            s.faithfulness,
            s.accuracy,
            s.retrieval_hit_rate.map(|r| format!("{r}%")).unwrap_or_else(|| "–".into()),
            s.safety_violations
        )
    };
    let mut out = String::from("# RAG evaluation\n\n| Scenario | Items | Pass | Groundedness (1-5) | Relevance (1-5) | Faithfulness (0-1) | Accuracy (1-5) | Retrieval hit | Safety violations |\n|---|---|---|---|---|---|---|---|---|\n");
    out.push_str(&row("**all**", &r.overall));
    for (k, s) in &r.by_scenario {
        out.push_str(&row(&format!("{k:?}").to_lowercase(), s));
    }
    if !r.worst.is_empty() {
        out.push_str("\n## Lowest-scoring answers\n\n");
        for o in &r.worst {
            out.push_str(&format!(
                "- **{:?}** {}: {}\n  - answer: {}\n  - judge: {}\n",
                o.item.scenario,
                o.item.id,
                o.item.question,
                o.answer.chars().take(300).collect::<String>().replace('\n', " "),
                o.scores.notes
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(scenario: Scenario, g: f64, safe: bool) -> Outcome {
        let item = Item {
            id: "x".into(),
            scenario,
            person: "a".into(),
            purpose: None,
            question: "q".into(),
            reference: "r".into(),
            expected_sources: vec![],
            must_not_contain: vec![],
        };
        let scores = Scores { groundedness: g, relevance: 5.0, faithfulness: 1.0, accuracy: Some(5.0), abstained: false, notes: String::new() };
        let p = passed(&item, &scores, safe);
        Outcome { item, answer: "a".into(), retrieved: vec![], retrieval_hit: Some(g >= 4.0), scores, safe, passed: p, error: None }
    }

    #[test]
    fn short_runs_cover_every_scenario() {
        let mk = |s| outcome(s, 5.0, true).item;
        let items = vec![mk(Scenario::Facts), mk(Scenario::Facts), mk(Scenario::Facts), mk(Scenario::Privacy), mk(Scenario::Grounded)];
        let picked: Vec<Scenario> = sample(items, 4).iter().map(|i| i.scenario).collect();
        assert_eq!(picked.len(), 4);
        for s in [Scenario::Facts, Scenario::Privacy, Scenario::Grounded] {
            assert!(picked.contains(&s), "{s:?} missing from {picked:?}");
        }
    }

    #[test]
    fn summary_and_pass_rules() {
        let o = vec![outcome(Scenario::Facts, 5.0, true), outcome(Scenario::Facts, 2.0, true), outcome(Scenario::Privacy, 5.0, false)];
        let r = report(&o);
        assert_eq!(r.overall.passed, 1);
        assert_eq!(r.overall.safety_violations, 1, "a leaked private value always fails");
        assert_eq!(r.by_scenario[&Scenario::Facts].retrieval_hit_rate, Some(50.0));
        assert!(markdown(&r).contains("| **all** | 3 |"));
    }
}
