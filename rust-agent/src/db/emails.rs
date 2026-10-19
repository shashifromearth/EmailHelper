use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{from_json, new_id, now, to_json};
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AttachmentMeta {
    pub name: String,
    #[serde(default)]
    pub content_type: String,
    #[serde(default)]
    pub size: u64,
}

/// Email as submitted by the Thunderbird extension (`POST /api/v1/emails`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EmailIn {
    pub account_id: String,
    #[serde(default)]
    pub account_name: Option<String>,
    #[serde(default)]
    pub account_email: Option<String>,
    #[serde(default)]
    pub account_type: Option<String>,
    pub message_id: String,
    pub tb_message_id: i64,
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub in_reply_to: Option<String>,
    #[serde(default)]
    pub references: Vec<String>,
    pub from: String,
    #[serde(default)]
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    pub subject: String,
    #[serde(default)]
    pub date: Option<String>,
    pub body_text: String,
    #[serde(default)]
    pub body_html: Option<String>,
    #[serde(default)]
    pub attachments: Vec<AttachmentMeta>,
}

pub const ACCOUNT_TYPES: &[&str] = &["gmail", "outlook", "zoho", "imap", "pop3", "other"];

impl EmailIn {
    /// Validate and normalise in place. Truncates oversized text instead of
    /// rejecting, so a long newsletter never blocks the pipeline.
    pub fn validate(&mut self, max_text_bytes: usize) -> AppResult<()> {
        fn check(name: &str, v: &str, max: usize) -> AppResult<()> {
            if v.len() > max {
                return Err(AppError::Validation(format!(
                    "{name} exceeds {max} characters"
                )));
            }
            if v.chars().any(|c| c == '\0') {
                return Err(AppError::Validation(format!("{name} contains NUL")));
            }
            Ok(())
        }
        self.account_id = self.account_id.trim().to_string();
        self.message_id = self.message_id.trim().to_string();
        if self.account_id.is_empty() {
            return Err(AppError::Validation("account_id is required".into()));
        }
        if self.message_id.is_empty() {
            return Err(AppError::Validation("message_id is required".into()));
        }
        if self.from.trim().is_empty() {
            return Err(AppError::Validation("from is required".into()));
        }
        check("account_id", &self.account_id, 200)?;
        check("message_id", &self.message_id, 998)?;
        check("from", &self.from, 998)?;
        check("subject", &self.subject, 998)?;
        if self.to.len() > 500 || self.cc.len() > 500 {
            return Err(AppError::Validation("too many recipients".into()));
        }
        if self.references.len() > 200 {
            self.references.truncate(200);
        }
        if self.attachments.len() > 100 {
            return Err(AppError::Validation("too many attachments".into()));
        }
        let t = self
            .account_type
            .as_deref()
            .unwrap_or("imap")
            .to_ascii_lowercase();
        self.account_type = Some(if ACCOUNT_TYPES.contains(&t.as_str()) {
            t
        } else {
            "other".into()
        });
        self.body_text = truncate_bytes(&self.body_text.replace('\0', ""), max_text_bytes);
        if let Some(h) = &self.body_html {
            self.body_html = Some(truncate_bytes(&h.replace('\0', ""), max_text_bytes * 2));
        }
        Ok(())
    }
}

/// Truncate to at most `max` bytes on a char boundary.
pub fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Extract the bare address from `"Name" <a@b.com>` or `a@b.com`.
pub fn email_address(from: &str) -> String {
    let s = from.trim();
    if let (Some(l), Some(r)) = (s.rfind('<'), s.rfind('>')) {
        if l < r {
            return s[l + 1..r].trim().to_ascii_lowercase();
        }
    }
    s.trim_matches(|c| c == '"' || c == '\'')
        .to_ascii_lowercase()
}

/// Display name part of a From header (falls back to the local part).
pub fn display_name(from: &str) -> String {
    let s = from.trim();
    if let Some(l) = s.rfind('<') {
        let name = s[..l].trim().trim_matches('"').trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    let addr = email_address(s);
    addr.split('@').next().unwrap_or("").to_string()
}

#[derive(Debug, Clone, Serialize)]
pub struct EmailRecord {
    pub id: String,
    pub account_id: String,
    pub account_name: Option<String>,
    pub account_email: Option<String>,
    pub account_type: String,
    pub message_id: String,
    pub tb_message_id: i64,
    pub folder: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub from: String,
    pub from_email: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub received_at: Option<String>,
    pub body_text: String,
    pub body_html: Option<String>,
    pub attachments: Vec<AttachmentMeta>,
    pub category: Option<String>,
    pub is_job: bool,
    pub status: String,
    pub created_at: String,
}

const COLS: &str =
    "id, account_id, account_name, account_email, account_type, message_id, tb_message_id,
    folder, in_reply_to, refs, from_addr, from_email, to_addrs, cc_addrs, subject, received_at,
    body_text, body_html, attachments, category, is_job, status, created_at";

fn map_row(r: &Row<'_>) -> rusqlite::Result<EmailRecord> {
    Ok(EmailRecord {
        id: r.get(0)?,
        account_id: r.get(1)?,
        account_name: r.get(2)?,
        account_email: r.get(3)?,
        account_type: r.get(4)?,
        message_id: r.get(5)?,
        tb_message_id: r.get(6)?,
        folder: r.get(7)?,
        in_reply_to: r.get(8)?,
        references: from_json(r.get(9)?),
        from: r.get(10)?,
        from_email: r.get(11)?,
        to: from_json(r.get(12)?),
        cc: from_json(r.get(13)?),
        subject: r.get(14)?,
        received_at: r.get(15)?,
        body_text: r.get(16)?,
        body_html: r.get(17)?,
        attachments: from_json(r.get(18)?),
        category: r.get(19)?,
        is_job: r.get::<_, i64>(20)? != 0,
        status: r.get(21)?,
        created_at: r.get(22)?,
    })
}

pub enum InsertOutcome {
    Inserted(String),
    Existing(String),
}

/// Insert an email; returns the existing id when `(account_id, message_id)` exists.
pub fn insert(c: &Connection, e: &EmailIn) -> AppResult<InsertOutcome> {
    if let Some(id) = c
        .query_row(
            "SELECT id FROM emails WHERE account_id=?1 AND message_id=?2",
            params![e.account_id, e.message_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(InsertOutcome::Existing(id));
    }
    let id = new_id();
    let ts = now();
    let mut h = Sha256::new();
    h.update(e.body_text.as_bytes());
    let hash = format!("{:x}", h.finalize());
    c.execute(
        "INSERT INTO emails (id, account_id, account_name, account_email, account_type, message_id,
            tb_message_id, folder, in_reply_to, refs, from_addr, from_email, to_addrs, cc_addrs,
            subject, received_at, body_text, body_html, body_hash, attachments, status,
            created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,'received',?21,?21)",
        params![
            id,
            e.account_id,
            e.account_name,
            e.account_email,
            e.account_type.as_deref().unwrap_or("imap"),
            e.message_id,
            e.tb_message_id,
            e.folder,
            e.in_reply_to,
            to_json(&e.references),
            e.from,
            email_address(&e.from),
            to_json(&e.to),
            to_json(&e.cc),
            e.subject,
            e.date,
            e.body_text,
            e.body_html,
            hash,
            to_json(&e.attachments),
            ts,
        ],
    )?;
    Ok(InsertOutcome::Inserted(id))
}

pub fn get(c: &Connection, id: &str) -> AppResult<EmailRecord> {
    c.query_row(
        &format!("SELECT {COLS} FROM emails WHERE id=?1"),
        [id],
        map_row,
    )
    .optional()?
    .ok_or_else(|| AppError::NotFound(format!("email {id}")))
}

pub fn set_status(c: &Connection, id: &str, status: &str) -> AppResult<()> {
    c.execute(
        "UPDATE emails SET status=?2, updated_at=?3 WHERE id=?1",
        params![id, status, now()],
    )?;
    Ok(())
}

pub fn set_classification(
    c: &Connection,
    id: &str,
    category: &str,
    classification_json: &str,
    is_job: bool,
) -> AppResult<()> {
    c.execute(
        "UPDATE emails SET category=?2, classification=?3, is_job=?4, updated_at=?5 WHERE id=?1",
        params![id, category, classification_json, is_job as i64, now()],
    )?;
    Ok(())
}

/// Prior messages in the same thread or from the same sender (most recent first).
pub fn related(c: &Connection, e: &EmailRecord, limit: usize) -> AppResult<Vec<EmailRecord>> {
    let mut ids: Vec<String> = e.references.clone();
    if let Some(r) = &e.in_reply_to {
        ids.push(r.clone());
    }
    let ids_json = to_json(&ids);
    let mut stmt = c.prepare(&format!(
        "SELECT {COLS} FROM emails
         WHERE id != ?1 AND account_id = ?2
           AND (message_id IN (SELECT value FROM json_each(?3)) OR from_email = ?4)
         ORDER BY created_at DESC LIMIT ?5"
    ))?;
    let rows = stmt
        .query_map(
            params![e.id, e.account_id, ids_json, e.from_email, limit as i64],
            map_row,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn count(c: &Connection) -> AppResult<i64> {
    Ok(
        c.query_row("SELECT COUNT(*) FROM emails WHERE status='done'", [], |r| {
            r.get(0)
        })?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_parsing() {
        assert_eq!(email_address("Jane Doe <Jane@Acme.com>"), "jane@acme.com");
        assert_eq!(email_address("jane@acme.com"), "jane@acme.com");
        assert_eq!(display_name("\"Jane Doe\" <jane@acme.com>"), "Jane Doe");
        assert_eq!(display_name("jane@acme.com"), "jane");
    }

    #[test]
    fn validation_rules() {
        let mut e = EmailIn {
            account_id: "a".into(),
            message_id: "<m@x>".into(),
            tb_message_id: 1,
            from: "x@y.z".into(),
            subject: "s".into(),
            body_text: "é".repeat(100),
            account_type: Some("GMAIL".into()),
            ..Default::default()
        };
        e.validate(51).unwrap();
        assert!(e.body_text.len() <= 51);
        assert_eq!(e.account_type.as_deref(), Some("gmail"));

        let mut bad = e.clone();
        bad.subject = "x".repeat(999);
        assert!(bad.validate(100).is_err());
        let mut bad = e.clone();
        bad.account_id = "  ".into();
        assert!(bad.validate(100).is_err());
        let mut odd = e.clone();
        odd.account_type = Some("exchange-weird".into());
        odd.validate(100).unwrap();
        assert_eq!(odd.account_type.as_deref(), Some("other"));
    }
}
