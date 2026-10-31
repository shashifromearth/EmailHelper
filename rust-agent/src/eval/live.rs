//! LLM-as-judge in production: a sample of real drafts is scored in the
//! background (groundedness, relevance, faithfulness) and stored, so quality
//! can be watched without slowing drafting down. Drafts are judged with
//! private values masked.

use std::sync::Arc;

use rusqlite::params;
use serde_json::{json, Value};

use crate::app::App;
use crate::error::AppResult;

/// Should this draft be judged? (`EVAL_LIVE_SAMPLE` = share 0.0-1.0)
pub fn sampled(rate: f64) -> bool {
    rate > 0.0 && (uuid::Uuid::new_v4().as_u128() % 10_000) as f64 / 10_000.0 < rate
}

/// Judge a draft in the background and store the scores.
pub fn spawn(app: &Arc<App>, draft_id: String, question: String, context: String, masked_draft: String) {
    if !sampled(app.cfg.eval_live_sample) {
        return;
    }
    let app = app.clone();
    tokio::spawn(async move {
        match super::judge::judge(&app, &question, &context, &masked_draft, "").await {
            Ok(s) => {
                let notes = s.notes.clone();
                let res = app
                    .db
                    .call(move |c| {
                        c.execute(
                            "INSERT INTO eval_live(draft_id, ts, groundedness, relevance, faithfulness, abstained, notes)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![draft_id, crate::db::now(), s.groundedness, s.relevance, s.faithfulness, s.abstained, notes],
                        )?;
                        Ok(())
                    })
                    .await;
                if let Err(e) = res {
                    tracing::warn!(error = %e, "could not store the live evaluation");
                }
                if s.groundedness < 3.0 {
                    tracing::warn!(groundedness = s.groundedness, notes = %s.notes, "a draft scored low on groundedness");
                }
            }
            Err(e) => tracing::warn!(error = %e, "live draft evaluation failed"),
        }
    });
}

/// Averages over the last `days` days (for the status API / dashboard).
pub async fn summary(app: &App, days: i64) -> AppResult<Value> {
    let since = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    app.db
        .call(move |c| {
            let (n, g, r, f): (i64, Option<f64>, Option<f64>, Option<f64>) = c.query_row(
                "SELECT COUNT(*), AVG(groundedness), AVG(relevance), AVG(faithfulness) FROM eval_live WHERE ts >= ?1",
                [since],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            let round = |x: Option<f64>| x.map(|v| (v * 100.0).round() / 100.0);
            Ok(json!({ "drafts_judged": n, "groundedness": round(g), "relevance": round(r), "faithfulness": round(f), "days": days }))
        })
        .await
}

#[cfg(test)]
mod tests {
    #[test]
    fn sampling_rate_bounds() {
        assert!(!super::sampled(0.0));
        assert!(super::sampled(1.0));
    }
}
