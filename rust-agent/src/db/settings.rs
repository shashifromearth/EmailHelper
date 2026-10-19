use rusqlite::{params, Connection, OptionalExtension};

use super::now;
use crate::error::AppResult;

pub fn get(c: &Connection, key: &str) -> AppResult<Option<String>> {
    Ok(
        c.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
            r.get(0)
        })
        .optional()?,
    )
}

pub fn set(c: &Connection, key: &str, value: &str) -> AppResult<()> {
    c.execute(
        "INSERT INTO settings(key, value, updated_at) VALUES(?1,?2,?3)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
        params![key, value, now()],
    )?;
    Ok(())
}

/// Increment an integer counter stored in settings and return the new value.
pub fn incr(c: &Connection, key: &str) -> AppResult<i64> {
    let v = get(c, key)?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0)
        + 1;
    set(c, key, &v.to_string())?;
    Ok(v)
}
