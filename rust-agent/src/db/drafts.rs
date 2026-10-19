use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

use super::{from_json, new_id, now, to_json};
use crate::error::{AppError, AppResult};

/// Max save attempts by the extension before a draft is marked failed.
pub const MAX_DELIVERY_ATTEMPTS: i64 = 3;
/// A handed-out draft that is not acknowledged within this window is re-offered.
pub const REDELIVER_AFTER_SECS: i64 = 600;

#[derive(Debug, Clone)]
pub struct NewDraft {
    pub email_id: Option<String>,
    pub kind: String,
    pub account_id: String,
    pub tb_message_id: Option<i64>,
    pub message_id: Option<String>,
    pub to: Vec<String>,
    pub subject: String,
    pub body: String,
    pub validation: String,
    pub notes: Vec<String>,
    pub confidence: Option<f32>,
    pub llm_provider: Option<String>,
    pub llm_model: Option<String>,
}

/// Wire shape returned by `GET /api/v1/drafts/pending` (see documents/API.md).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PendingDraft {
    pub draft_id: String,
    pub kind: String,
    pub account_id: String,
    pub tb_message_id: Option<i64>,
    pub message_id: Option<String>,
    pub to: Vec<String>,
    pub subject: String,
    pub body: String,
    pub validation: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DraftRecord {
    pub id: String,
    pub email_id: Option<String>,
    pub kind: String,
    pub account_id: String,
    pub subject: String,
    pub validation: String,
    pub status: String,
    pub delivery_attempts: i64,
    pub tb_draft_message_id: Option<i64>,
    pub error: Option<String>,
    pub created_at: String,
}

pub fn insert(c: &Connection, d: &NewDraft) -> AppResult<String> {
    let id = new_id();
    let ts = now();
    c.execute(
        "INSERT INTO drafts (id, email_id, kind, account_id, tb_message_id, message_id, to_addrs,
            subject, body, validation, notes, confidence, status, llm_provider, llm_model,
            created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'pending',?13,?14,?15,?15)",
        params![
            id,
            d.email_id,
            d.kind,
            d.account_id,
            d.tb_message_id,
            d.message_id,
            to_json(&d.to),
            d.subject,
            d.body,
            d.validation,
            to_json(&d.notes),
            d.confidence,
            d.llm_provider,
            d.llm_model,
            ts,
        ],
    )?;
    Ok(id)
}

fn map_pending(r: &Row<'_>) -> rusqlite::Result<PendingDraft> {
    Ok(PendingDraft {
        draft_id: r.get(0)?,
        kind: r.get(1)?,
        account_id: r.get(2)?,
        tb_message_id: r.get(3)?,
        message_id: r.get(4)?,
        to: from_json(r.get(5)?),
        subject: r.get(6)?,
        body: r.get(7)?,
        validation: r.get(8)?,
        notes: from_json(r.get(9)?),
    })
}

/// Atomically claim drafts that are pending, or delivered-but-unacknowledged
/// for longer than [`REDELIVER_AFTER_SECS`], and mark them delivered.
pub fn claim_pending(c: &mut Connection, limit: usize) -> AppResult<Vec<PendingDraft>> {
    let cutoff = (chrono::Utc::now() - chrono::Duration::seconds(REDELIVER_AFTER_SECS))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let tx = c.transaction()?;
    let drafts = {
        let mut stmt = tx.prepare(
            "SELECT id, kind, account_id, tb_message_id, message_id, to_addrs, subject, body,
                    validation, notes
             FROM drafts
             WHERE status='pending' OR (status='delivered' AND delivered_at < ?1)
             ORDER BY created_at LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![cutoff, limit as i64], map_pending)?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let ts = now();
    for d in &drafts {
        tx.execute(
            "UPDATE drafts SET status='delivered', delivered_at=?2, updated_at=?2 WHERE id=?1",
            params![d.draft_id, ts],
        )?;
    }
    tx.commit()?;
    Ok(drafts)
}

pub enum AckOutcome {
    Saved,
    Retry,
    Failed,
}

/// Record the extension's acknowledgement for a draft.
pub fn ack(
    c: &Connection,
    id: &str,
    saved: bool,
    tb_draft_message_id: Option<i64>,
    error: Option<&str>,
) -> AppResult<AckOutcome> {
    let attempts: i64 = c
        .query_row(
            "SELECT delivery_attempts FROM drafts WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| AppError::NotFound(format!("draft {id}")))?;
    let ts = now();
    if saved {
        c.execute(
            "UPDATE drafts SET status='saved', tb_draft_message_id=?2, error=NULL,
                delivery_attempts=delivery_attempts+1, updated_at=?3 WHERE id=?1",
            params![id, tb_draft_message_id, ts],
        )?;
        return Ok(AckOutcome::Saved);
    }
    let attempts = attempts + 1;
    let (status, outcome) = if attempts >= MAX_DELIVERY_ATTEMPTS {
        ("failed", AckOutcome::Failed)
    } else {
        ("pending", AckOutcome::Retry)
    };
    c.execute(
        "UPDATE drafts SET status=?2, delivery_attempts=?3, error=?4, updated_at=?5 WHERE id=?1",
        params![id, status, attempts, error, ts],
    )?;
    Ok(outcome)
}

pub fn get(c: &Connection, id: &str) -> AppResult<DraftRecord> {
    c.query_row(
        "SELECT id, email_id, kind, account_id, subject, validation, status, delivery_attempts,
                tb_draft_message_id, error, created_at FROM drafts WHERE id=?1",
        [id],
        |r| {
            Ok(DraftRecord {
                id: r.get(0)?,
                email_id: r.get(1)?,
                kind: r.get(2)?,
                account_id: r.get(3)?,
                subject: r.get(4)?,
                validation: r.get(5)?,
                status: r.get(6)?,
                delivery_attempts: r.get(7)?,
                tb_draft_message_id: r.get(8)?,
                error: r.get(9)?,
                created_at: r.get(10)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| AppError::NotFound(format!("draft {id}")))
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct DraftCounts {
    pub total: i64,
    pub pending: i64,
    pub review_required: i64,
    pub saved: i64,
    pub failed: i64,
}

pub fn counts(c: &Connection) -> AppResult<DraftCounts> {
    Ok(c.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(status IN ('pending','delivered')),0),
                COALESCE(SUM(validation='REVIEW_REQUIRED'),0),
                COALESCE(SUM(status='saved'),0),
                COALESCE(SUM(status='failed'),0)
         FROM drafts",
        [],
        |r| {
            Ok(DraftCounts {
                total: r.get(0)?,
                pending: r.get(1)?,
                review_required: r.get(2)?,
                saved: r.get(3)?,
                failed: r.get(4)?,
            })
        },
    )?)
}

pub fn for_email(c: &Connection, email_id: &str) -> AppResult<Vec<DraftRecord>> {
    let mut stmt = c.prepare("SELECT id FROM drafts WHERE email_id=?1 ORDER BY created_at")?;
    let ids = stmt
        .query_map([email_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    ids.iter().map(|id| get(c, id)).collect()
}
