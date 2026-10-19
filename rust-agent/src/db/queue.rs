//! Durable processing queue (`processing_jobs`). The in-memory dispatcher in
//! `crate::queue` only carries job ids; the row is the source of truth, so work
//! survives restarts.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use super::{new_id, now};
use crate::error::AppResult;

#[derive(Debug, Clone, Serialize)]
pub struct QueuedJob {
    pub id: String,
    pub kind: String,
    pub idempotency_key: String,
    pub ref_id: Option<String>,
    pub payload: Option<String>,
    pub status: String,
    pub attempts: i64,
    pub max_attempts: i64,
    pub last_error: Option<String>,
    pub run_after: String,
}

fn map(r: &rusqlite::Row<'_>) -> rusqlite::Result<QueuedJob> {
    Ok(QueuedJob {
        id: r.get(0)?,
        kind: r.get(1)?,
        idempotency_key: r.get(2)?,
        ref_id: r.get(3)?,
        payload: r.get(4)?,
        status: r.get(5)?,
        attempts: r.get(6)?,
        max_attempts: r.get(7)?,
        last_error: r.get(8)?,
        run_after: r.get(9)?,
    })
}

const COLS: &str =
    "id, kind, idempotency_key, ref_id, payload, status, attempts, max_attempts, last_error, run_after";

/// Insert a job unless the idempotency key already exists.
/// Returns `Some(id)` for a new job, `None` for a duplicate.
pub fn enqueue(
    c: &Connection,
    kind: &str,
    key: &str,
    ref_id: Option<&str>,
    payload: Option<&str>,
    max_attempts: u32,
) -> AppResult<Option<String>> {
    let id = new_id();
    let ts = now();
    let n = c.execute(
        "INSERT INTO processing_jobs (id, kind, idempotency_key, ref_id, payload, status,
            attempts, max_attempts, run_after, created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,'queued',0,?6,?7,?7,?7)
         ON CONFLICT(idempotency_key) DO NOTHING",
        params![id, kind, key, ref_id, payload, max_attempts, ts],
    )?;
    Ok((n == 1).then_some(id))
}

pub fn exists_key(c: &Connection, key: &str) -> AppResult<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM processing_jobs WHERE idempotency_key=?1)",
        [key],
        |r| r.get(0),
    )?)
}

pub fn get(c: &Connection, id: &str) -> AppResult<Option<QueuedJob>> {
    Ok(c.query_row(
        &format!("SELECT {COLS} FROM processing_jobs WHERE id=?1"),
        [id],
        map,
    )
    .optional()?)
}

/// Transition queued -> running. Returns the job only if this call won the claim.
pub fn claim(c: &Connection, id: &str) -> AppResult<Option<QueuedJob>> {
    let n = c.execute(
        "UPDATE processing_jobs SET status='running', attempts=attempts+1, updated_at=?2
         WHERE id=?1 AND status='queued'",
        params![id, now()],
    )?;
    if n == 0 {
        return Ok(None);
    }
    get(c, id)
}

pub fn complete(c: &Connection, id: &str) -> AppResult<()> {
    c.execute(
        "UPDATE processing_jobs SET status='done', last_error=NULL, updated_at=?2 WHERE id=?1",
        params![id, now()],
    )?;
    Ok(())
}

/// Record a failure. Returns `true` when the job will be retried.
pub fn fail(c: &Connection, id: &str, error: &str, retry_at: Option<&str>) -> AppResult<bool> {
    let ts = now();
    match retry_at {
        Some(at) => {
            c.execute(
                "UPDATE processing_jobs SET status='queued', last_error=?2, run_after=?3, updated_at=?4
                 WHERE id=?1",
                params![id, error, at, ts],
            )?;
            Ok(true)
        }
        None => {
            c.execute(
                "UPDATE processing_jobs SET status='dead', last_error=?2, updated_at=?3 WHERE id=?1",
                params![id, error, ts],
            )?;
            Ok(false)
        }
    }
}

/// Crash recovery: anything left `running` by a previous process goes back to
/// `queued`. Returns all queued jobs in creation order.
pub fn recover(c: &Connection) -> AppResult<Vec<QueuedJob>> {
    let n = c.execute(
        "UPDATE processing_jobs SET status='queued', updated_at=?1 WHERE status='running'",
        [now()],
    )?;
    if n > 0 {
        tracing::warn!(count = n, "requeued jobs interrupted by previous shutdown");
    }
    let mut stmt = c.prepare(&format!(
        "SELECT {COLS} FROM processing_jobs WHERE status='queued' ORDER BY created_at"
    ))?;
    let rows = stmt.query_map([], map)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn depth(c: &Connection) -> AppResult<i64> {
    Ok(c.query_row(
        "SELECT COUNT(*) FROM processing_jobs WHERE status IN ('queued','running')",
        [],
        |r| r.get(0),
    )?)
}

pub fn dead_count(c: &Connection) -> AppResult<i64> {
    Ok(c.query_row(
        "SELECT COUNT(*) FROM processing_jobs WHERE status='dead'",
        [],
        |r| r.get(0),
    )?)
}
