//! The Chrome extension's applier: when it is stuck it asks a person over
//! Telegram (escalations), and it reports every failure (events) so problems
//! that keep coming back can be found and fixed.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::app::App;
use crate::db::apply::{self as adb, Escalation, EscalationIn, EventIn};
use crate::error::{AppError, AppResult};
use crate::telegram::{format, Event};

/// Events accepted in one call.
const MAX_EVENTS: usize = 200;
/// Recurring problems listed in the summary.
const TOP: usize = 50;
/// Event kinds the extension sends (metric labels; anything else is "other").
const KINDS: &[&str] = &["fill_failed", "blocked", "submit_failed", "error", "applied", "skipped", "escalated"];
/// Kinds logged as warnings.
const FAILURES: &[&str] = &["fill_failed", "blocked", "submit_failed", "error"];

/// Open an escalation and message the person. Returns it and whether a
/// Telegram message is out for it; an open escalation for the same URL is
/// returned as is, without messaging again.
pub async fn escalate(app: &Arc<App>, req: EscalationIn) -> AppResult<(Escalation, bool)> {
    if req.url.trim().is_empty() || req.kind.trim().is_empty() {
        return Err(AppError::Validation("url and kind are required".into()));
    }
    let person = req.person.as_deref().and_then(|id| crate::jobs::person_by_id(app, Some(id)));
    let r = req.clone();
    let (esc, created) = app.db.call(move |c| adb::open_escalation(c, &r)).await?;
    if !created {
        let sent = esc.telegram_message_id.is_some();
        return Ok((esc, sent));
    }
    tracing::warn!(platform = esc.platform.as_deref().unwrap_or("-"), kind = %esc.kind, "applier needs a person");
    let Some(client) = app.notifier.client() else {
        return Ok((esc, false));
    };
    let chat = person.as_ref().and_then(|p| p.telegram_chat_id).unwrap_or(client.chat_id);
    let mut text = String::new();
    if let Some(p) = &person {
        text.push_str(&format::person_tag(&p.name, None));
        text.push('\n');
    }
    text.push_str("🙋 Job applier needs you\n");
    let what: Vec<&str> = [esc.title.as_deref(), esc.company.as_deref()].into_iter().flatten().collect();
    if !what.is_empty() {
        text.push_str(&what.join(" — "));
        text.push('\n');
    }
    text.push_str(&format!(
        "{}\nWhy: {}\nFix it on the computer and I'll continue, or reply to this message with: skip",
        esc.url, esc.reason
    ));
    let Some(mid) = app.notifier.send_in(Some(chat), Event::ApplicationBlocked, &text, None).await else {
        return Ok((esc, false));
    };
    let id = esc.id.clone();
    app.db.call(move |c| adb::set_escalation_telegram(c, &id, chat, mid)).await?;
    Ok((esc, true))
}

/// A Telegram reply to an escalation: "skip" gives up, "done"/"fixed"/"ok"
/// means the page is fixed, anything else is an answer for the applier.
pub fn parse_reply(text: &str) -> (&'static str, Option<String>) {
    let t = text.trim();
    let word = t.trim_start_matches('/').trim_end_matches(['.', '!']).to_lowercase();
    match word.as_str() {
        "skip" => ("skip", None),
        "done" | "fixed" | "ok" => ("done", None),
        _ => ("done", Some(t.to_string())),
    }
}

/// Apply a Telegram reply; returns the bot's acknowledgement.
pub async fn handle_reply(app: &Arc<App>, esc: &Escalation, text: &str) -> AppResult<String> {
    if esc.status != "open" {
        return Ok(format!("That one is already handled ({}).", esc.status));
    }
    let (status, answer) = parse_reply(text);
    let id = esc.id.clone();
    app.db
        .call(move |c| adb::resolve_escalation(c, &id, status, answer.as_deref()))
        .await?;
    Ok(if status == "skip" { "Skipping it." } else { "Thanks, continuing." }.into())
}

/// The extension saw the page get fixed (`done`) or gave up (`skip`).
pub async fn resolve(app: &Arc<App>, id: String, status: String) -> AppResult<Escalation> {
    if status != "done" && status != "skip" {
        return Err(AppError::Validation("status must be done or skip".into()));
    }
    app.db.call(move |c| adb::resolve_escalation(c, &id, &status, None)).await
}

/// What the extension polls for.
pub fn view(e: &Escalation) -> Value {
    json!({ "id": e.id, "status": e.status, "answer": e.answer, "created_at": e.created_at, "updated_at": e.updated_at })
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_replies() {
        assert_eq!(parse_reply(" Skip "), ("skip", None));
        assert_eq!(parse_reply("/skip"), ("skip", None));
        assert_eq!(parse_reply("Fixed!"), ("done", None));
        assert_eq!(parse_reply("ok"), ("done", None));
        assert_eq!(parse_reply("Use 9876543210"), ("done", Some("Use 9876543210".into())));
    }
}
