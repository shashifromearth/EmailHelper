use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::Value;

use super::now;
use crate::error::AppResult;

#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub ts: String,
    pub actor: String,
    pub action: String,
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    pub details: Value,
}

/// Append an audit record. Audit logs are append-only.
pub fn log(
    c: &Connection,
    actor: &str,
    action: &str,
    entity_type: &str,
    entity_id: &str,
    details: Value,
) -> AppResult<()> {
    c.execute(
        "INSERT INTO audit_logs (ts, actor, action, entity_type, entity_id, details)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            now(),
            actor,
            action,
            entity_type,
            entity_id,
            details.to_string()
        ],
    )?;
    Ok(())
}

pub fn recent(c: &Connection, limit: usize, entity_id: Option<&str>) -> AppResult<Vec<AuditEntry>> {
    let mut stmt = c.prepare(
        "SELECT id, ts, actor, action, entity_type, entity_id, details FROM audit_logs
         WHERE (?1 IS NULL OR entity_id = ?1)
         ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![entity_id, limit as i64], |r| {
            let details: Option<String> = r.get(6)?;
            Ok(AuditEntry {
                id: r.get(0)?,
                ts: r.get(1)?,
                actor: r.get(2)?,
                action: r.get(3)?,
                entity_type: r.get(4)?,
                entity_id: r.get(5)?,
                details: details
                    .and_then(|d| serde_json::from_str(&d).ok())
                    .unwrap_or(Value::Null),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
