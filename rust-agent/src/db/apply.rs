//! The job appliers' failure log (events from the Chrome extensions).

use std::collections::BTreeMap;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use super::now;
use crate::error::AppResult;

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
