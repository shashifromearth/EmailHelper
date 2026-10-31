//! Chrome extension applier: escalations to a person and the failure log.

use std::collections::BTreeMap;

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::{new_id, now};
use crate::error::{AppError, AppResult};

/// Longest stored text; anything longer is cut.
const MAX_TEXT: usize = 2000;
/// Longest stored label (kind, platform, person, field, ...).
const MAX_LABEL: usize = 200;

fn clip(s: &str, n: usize) -> String {
    s.trim().chars().take(n).collect()
}

/// Trimmed and cut; blank becomes None so it groups as "unknown".
fn clip_opt(s: &Option<String>, n: usize) -> Option<String> {
    s.as_deref().map(|s| clip(s, n)).filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------- escalations

#[derive(Debug, Clone, Deserialize)]
pub struct EscalationIn {
    #[serde(default)]
    pub link_id: Option<String>,
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub company: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub person: Option<String>,
    pub kind: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Escalation {
    pub id: String,
    pub url: String,
    pub title: Option<String>,
    pub company: Option<String>,
    pub platform: Option<String>,
    pub kind: String,
    pub reason: String,
    /// open | skip | done
    pub status: String,
    pub answer: Option<String>,
    pub telegram_message_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

const E_COLS: &str = "id, url, title, company, platform, kind, reason, status, answer,
    telegram_message_id, created_at, updated_at";

fn map_e(r: &Row<'_>) -> rusqlite::Result<Escalation> {
    Ok(Escalation {
        id: r.get(0)?,
        url: r.get(1)?,
        title: r.get(2)?,
        company: r.get(3)?,
        platform: r.get(4)?,
        kind: r.get(5)?,
        reason: r.get(6)?,
        status: r.get(7)?,
        answer: r.get(8)?,
        telegram_message_id: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

/// Open an escalation unless one is already open for the same URL
/// (returned with `false`).
pub fn open_escalation(c: &Connection, e: &EscalationIn) -> AppResult<(Escalation, bool)> {
    let url = clip(&e.url, MAX_TEXT);
    if let Some(found) = c
        .query_row(
            &format!("SELECT {E_COLS} FROM apply_escalations WHERE url=?1 AND status='open'"),
            [&url],
            map_e,
        )
        .optional()?
    {
        return Ok((found, false));
    }
    let (id, ts) = (new_id(), now());
    c.execute(
        "INSERT INTO apply_escalations (id, link_id, url, title, company, platform, person_id,
            kind, reason, status, created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'open',?10,?10)",
        params![
            id,
            clip_opt(&e.link_id, MAX_LABEL),
            url,
            clip_opt(&e.title, MAX_LABEL),
            clip_opt(&e.company, MAX_LABEL),
            clip_opt(&e.platform, MAX_LABEL),
            clip_opt(&e.person, MAX_LABEL),
            clip(&e.kind, MAX_LABEL),
            clip(&e.reason, MAX_TEXT),
            ts
        ],
    )?;
    Ok((get_escalation(c, &id)?, true))
}

pub fn get_escalation(c: &Connection, id: &str) -> AppResult<Escalation> {
    c.query_row(
        &format!("SELECT {E_COLS} FROM apply_escalations WHERE id=?1"),
        [id],
        map_e,
    )
    .optional()?
    .ok_or_else(|| AppError::NotFound(format!("escalation {id}")))
}

pub fn set_escalation_telegram(c: &Connection, id: &str, chat: i64, msg_id: i64) -> AppResult<()> {
    c.execute(
        "UPDATE apply_escalations SET telegram_chat_id=?2, telegram_message_id=?3 WHERE id=?1",
        params![id, chat, msg_id],
    )?;
    Ok(())
}

/// The escalation a Telegram reply refers to (any status). Message ids are
/// per chat, so the chat must match too.
pub fn escalation_for_message(c: &Connection, chat: i64, msg_id: i64) -> AppResult<Option<Escalation>> {
    Ok(c.query_row(
        &format!(
            "SELECT {E_COLS} FROM apply_escalations
             WHERE telegram_chat_id=?1 AND telegram_message_id=?2"
        ),
        params![chat, msg_id],
        map_e,
    )
    .optional()?)
}

/// Close an open escalation; the first resolution wins, later ones are no-ops.
/// Returns the escalation as it now is.
pub fn resolve_escalation(c: &Connection, id: &str, status: &str, answer: Option<&str>) -> AppResult<Escalation> {
    c.execute(
        "UPDATE apply_escalations SET status=?2, answer=?3, updated_at=?4
         WHERE id=?1 AND status='open'",
        params![id, status, answer.map(|a| clip(a, MAX_TEXT)), now()],
    )?;
    get_escalation(c, id)
}

// ---------------------------------------------------------------- events

#[derive(Debug, Clone, Deserialize)]
pub struct EventIn {
    pub kind: String,
    #[serde(default)]
    pub link_id: Option<String>,
    pub url: String,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub person: Option<String>,
    /// The form field's label.
    #[serde(default)]
    pub field: Option<String>,
    pub reason: String,
    #[serde(default)]
    pub detail: Option<String>,
}

pub fn insert_events(c: &mut Connection, events: &[EventIn]) -> AppResult<usize> {
    let tx = c.transaction()?;
    let ts = now();
    {
        let mut stmt = tx.prepare(
            "INSERT INTO apply_events (kind, link_id, url, platform, person_id, field, reason, detail, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )?;
        for e in events {
            stmt.execute(params![
                clip(&e.kind, MAX_LABEL),
                clip_opt(&e.link_id, MAX_LABEL),
                clip(&e.url, MAX_TEXT),
                clip_opt(&e.platform, MAX_LABEL),
                clip_opt(&e.person, MAX_LABEL),
                clip_opt(&e.field, MAX_LABEL),
                clip(&e.reason, MAX_TEXT),
                clip_opt(&e.detail, MAX_TEXT),
                ts
            ])?;
        }
    }
    tx.commit()?;
    Ok(events.len())
}

/// One recurring problem: the same failure on the same platform and field.
#[derive(Debug, Clone, Serialize)]
pub struct Recurring {
    pub platform: Option<String>,
    pub kind: String,
    pub field: Option<String>,
    pub reason: String,
    pub count: i64,
    pub last_url: String,
    pub last_at: String,
}

/// Event counts by kind since `since`, and the most frequent problems
/// (successful applications are not problems and are left out of `top`).
pub fn event_summary(c: &Connection, since: &str, top: usize) -> AppResult<(BTreeMap<String, i64>, Vec<Recurring>)> {
    let mut stmt = c.prepare("SELECT kind, COUNT(*) FROM apply_events WHERE created_at>=?1 GROUP BY kind")?;
    let by_kind = stmt
        .query_map([since], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    // With a single max(), SQLite takes the bare `url` from the latest row.
    let mut stmt = c.prepare(
        "SELECT platform, kind, field, reason, COUNT(*) AS n, url, MAX(created_at)
         FROM apply_events WHERE created_at>=?1 AND kind<>'applied'
         GROUP BY platform, kind, field, reason
         ORDER BY n DESC, MAX(created_at) DESC LIMIT ?2",
    )?;
    let top = stmt
        .query_map(params![since, top as i64], |r| {
            Ok(Recurring {
                platform: r.get(0)?,
                kind: r.get(1)?,
                field: r.get(2)?,
                reason: r.get(3)?,
                count: r.get(4)?,
                last_url: r.get(5)?,
                last_at: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok((by_kind, top))
}
