use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use super::now;
use crate::error::AppResult;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Snippet {
    pub path: String,
    pub title: String,
    pub content: String,
    pub score: f64,
}

pub fn registry_entry(c: &Connection, path: &str) -> AppResult<Option<(i64, String)>> {
    Ok(c.query_row(
        "SELECT mtime, hash FROM knowledge_registry WHERE path=?1",
        [path],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

/// Replace all chunks for a document.
pub fn replace_document(
    c: &mut Connection,
    path: &str,
    kind: &str,
    mtime: i64,
    hash: &str,
    chunks: &[(String, String)],
) -> AppResult<()> {
    let tx = c.transaction()?;
    tx.execute("DELETE FROM knowledge_fts WHERE path=?1", [path])?;
    {
        let mut ins =
            tx.prepare("INSERT INTO knowledge_fts(path, title, content) VALUES (?1,?2,?3)")?;
        for (title, content) in chunks {
            ins.execute(params![path, title, content])?;
        }
    }
    tx.execute(
        "INSERT INTO knowledge_registry(path, kind, mtime, hash, chunks, indexed_at)
         VALUES (?1,?2,?3,?4,?5,?6)
         ON CONFLICT(path) DO UPDATE SET kind=excluded.kind, mtime=excluded.mtime,
            hash=excluded.hash, chunks=excluded.chunks, indexed_at=excluded.indexed_at",
        params![path, kind, mtime, hash, chunks.len() as i64, now()],
    )?;
    tx.commit()?;
    Ok(())
}

/// Remove documents that no longer exist on disk.
pub fn prune(c: &mut Connection, keep: &[String]) -> AppResult<usize> {
    let existing: Vec<String> = {
        let mut stmt = c.prepare("SELECT path FROM knowledge_registry")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let tx = c.transaction()?;
    let mut removed = 0;
    for p in existing.iter().filter(|p| !keep.contains(p)) {
        tx.execute("DELETE FROM knowledge_fts WHERE path=?1", [p])?;
        tx.execute("DELETE FROM knowledge_registry WHERE path=?1", [p])?;
        removed += 1;
    }
    tx.commit()?;
    Ok(removed)
}

/// Build a safe FTS5 MATCH expression from free text: alphanumeric terms of
/// length >= 3, quoted and OR-ed together. Returns `None` if nothing usable.
pub fn fts_query(text: &str, max_terms: usize) -> Option<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "you", "your", "with", "this", "that", "are", "was", "have", "has",
        "but", "not", "from", "our", "can", "will", "would", "could", "please", "thanks", "thank",
        "hello", "regards", "best", "dear", "what", "when", "where", "which", "who", "how",
        "about", "there", "their", "they", "them", "been", "were", "also", "just", "any", "all",
        "out",
    ];
    let mut terms: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        let t = raw.to_lowercase();
        if t.chars().count() < 3 || STOP.contains(&t.as_str()) || terms.contains(&t) {
            continue;
        }
        terms.push(t);
        if terms.len() >= max_terms {
            break;
        }
    }
    if terms.is_empty() {
        return None;
    }
    Some(
        terms
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

pub fn search(c: &Connection, query_text: &str, limit: usize) -> AppResult<Vec<Snippet>> {
    let Some(q) = fts_query(query_text, 24) else {
        return Ok(vec![]);
    };
    let mut stmt = c.prepare(
        "SELECT path, title, content, bm25(knowledge_fts, 0.0, 2.0, 1.0) AS score
         FROM knowledge_fts WHERE knowledge_fts MATCH ?1
         ORDER BY score LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![q, limit as i64], |r| {
            Ok(Snippet {
                path: r.get(0)?,
                title: r.get(1)?,
                content: r.get(2)?,
                score: r.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn document_count(c: &Connection) -> AppResult<i64> {
    Ok(c.query_row("SELECT COUNT(*) FROM knowledge_registry", [], |r| r.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_query_is_sanitised() {
        assert_eq!(fts_query("the a an", 10), None);
        let q = fts_query("Pricing \"OR\" NEAR(x) refund; DROP TABLE", 10).unwrap();
        assert!(!q.contains('('));
        assert!(q.contains("\"pricing\""));
        assert!(q.contains("\"refund\""));
    }

    #[tokio::test]
    async fn index_and_search() {
        let db = crate::db::Db::open_in_memory().unwrap();
        db.call(|c| {
            replace_document(
                c,
                "approved_responses/pricing.md",
                "markdown",
                1,
                "h",
                &[(
                    "Pricing".into(),
                    "Our standard plan costs 49 USD per seat per month.".into(),
                )],
            )?;
            replace_document(
                c,
                "approved_responses/refunds.md",
                "markdown",
                1,
                "h",
                &[(
                    "Refunds".into(),
                    "Refunds are processed within 14 days.".into(),
                )],
            )
        })
        .await
        .unwrap();
        let hits = db
            .call(|c| search(c, "How much does a seat cost on the standard plan?", 5))
            .await
            .unwrap();
        assert_eq!(hits[0].path, "approved_responses/pricing.md");
        let removed = db
            .call(|c| prune(c, &["approved_responses/pricing.md".to_string()]))
            .await
            .unwrap();
        assert_eq!(removed, 1);
    }
}
