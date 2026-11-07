//! The job appliers (the Chrome extensions JobApplier and JobBoardApplier)
//! report every failure and outcome (events) so problems that keep coming
//! back can be found and fixed.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::app::App;
use crate::db::apply::{self as adb, EventIn};
use crate::error::{AppError, AppResult};

/// Events accepted in one call.
const MAX_EVENTS: usize = 200;
/// Recurring problems listed in the summary.
const TOP: usize = 50;
/// Event kinds the extension sends (metric labels; anything else is "other").
const KINDS: &[&str] = &["fill_failed", "blocked", "submit_failed", "error", "applied", "skipped"];
/// Kinds logged as warnings.
const FAILURES: &[&str] = &["fill_failed", "blocked", "submit_failed", "error"];

pub async fn record_events(app: &Arc<App>, events: Vec<EventIn>) -> AppResult<usize> {
    if events.len() > MAX_EVENTS {
        return Err(AppError::Validation(format!("at most {MAX_EVENTS} events")));
    }
    for e in &events {
        let kind = e.kind.trim();
        let label = if KINDS.contains(&kind) { kind } else { "other" };
        app.metrics.apply_events_total.with_label_values(&[label]).inc();
        if FAILURES.contains(&kind) {
            tracing::warn!(platform = e.platform.as_deref().unwrap_or("-"), kind, "applier failure");
        }
    }
    app.db.call(move |c| adb::insert_events(c, &events)).await
}

/// The feedback loop: counts by kind and what keeps failing where.
pub async fn summary(app: &Arc<App>, days: i64) -> AppResult<Value> {
    let days = days.clamp(1, 90);
    let since = (chrono::Utc::now() - chrono::Duration::days(days))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let (by_kind, top) = app.db.call(move |c| adb::event_summary(c, &since, TOP)).await?;
    Ok(json!({ "days": days, "by_kind": by_kind, "top": top }))
}
