//! Chunk store: SQLite + FTS5 (keywords) + sqlite-vec (one vector table per
//! embedding model) in one small database file.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use eh_knowledge::etl::{Chunk, ChunkMeta};

use crate::embed::DIMS;
use crate::error::Result;

/// Embedding model ids that have a vector table.
pub const MODELS: &[&str] = &["gemini", "local"];

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StoredChunk {
    pub id: i64,
    pub path: String,
    pub person_id: Option<String>,
    pub purpose: Option<String>,
    pub doc_type: String,
    pub stack: Option<String>,
    pub section: String,
    pub title: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct IndexStatus {
    pub documents: i64,
    pub chunks: i64,
    /// chunks with a vector, per model
    pub embedded: Vec<(String, i64)>,
    pub last_etl: Option<String>,
}

fn register_vec() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: sqlite3_vec_init has the SQLite extension entry-point
        // signature; auto-registration is how sqlite-vec is loaded into a
        // statically linked SQLite.
        unsafe {
            #[allow(clippy::missing_transmute_annotations)]
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
    });
}

fn schema() -> String {
    let mut s = String::from(
        "CREATE TABLE IF NOT EXISTS docs (
            path TEXT PRIMARY KEY, hash TEXT NOT NULL, chunks INTEGER NOT NULL, indexed_at TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS chunks (
            id INTEGER PRIMARY KEY, path TEXT NOT NULL, ord INTEGER NOT NULL,
            person_id TEXT, purpose TEXT, doc_type TEXT NOT NULL, stack TEXT,
            section TEXT NOT NULL, title TEXT NOT NULL, content TEXT NOT NULL, hash TEXT NOT NULL);
         CREATE INDEX IF NOT EXISTS idx_chunks_path ON chunks(path);
         CREATE INDEX IF NOT EXISTS idx_chunks_person ON chunks(person_id, purpose);
         CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(title, content, tokenize = 'porter unicode61');
         CREATE TABLE IF NOT EXISTS embedded (chunk_id INTEGER NOT NULL, model TEXT NOT NULL, PRIMARY KEY (chunk_id, model));
         CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    );
    for m in MODELS {
        s.push_str(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS vec_{m} USING vec0(embedding float[{DIMS}] distance_metric=cosine);"
        ));
    }
    s
}

fn as_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn row_chunk(r: &rusqlite::Row<'_>) -> rusqlite::Result<StoredChunk> {
    Ok(StoredChunk {
        id: r.get(0)?,
        path: r.get(1)?,
        person_id: r.get(2)?,
        purpose: r.get(3)?,
        doc_type: r.get(4)?,
        stack: r.get(5)?,
        section: r.get(6)?,
        title: r.get(7)?,
        content: r.get(8)?,
    })
}

const CHUNK_COLS: &str = "id, path, person_id, purpose, doc_type, stack, section, title, content";

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        register_vec();
        if let Some(p) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(p)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        register_vec();
        Self::init(Connection::open_in_memory()?)
    }

    fn init(c: Connection) -> Result<Self> {
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        c.execute_batch(&schema())?;
        Ok(Self { conn: Arc::new(Mutex::new(c)) })
    }

    /// Run `f` on the connection in a blocking thread.
    pub async fn call<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut Connection) -> Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = conn.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut c)
        })
        .await
        .map_err(|e| crate::error::Error::Internal(e.to_string()))?
    }
}

pub fn doc_hash(c: &Connection, path: &str) -> Result<Option<String>> {
    Ok(c.query_row("SELECT hash FROM docs WHERE path=?1", [path], |r| r.get(0)).optional()?)
}

fn delete_doc_chunks(tx: &rusqlite::Transaction<'_>, path: &str) -> Result<()> {
    let ids: Vec<i64> = {
        let mut st = tx.prepare("SELECT id FROM chunks WHERE path=?1")?;
        let v = st.query_map([path], |r| r.get(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
        v
    };
    for id in ids {
        tx.execute("DELETE FROM chunks_fts WHERE rowid=?1", [id])?;
        for m in MODELS {
            tx.execute(&format!("DELETE FROM vec_{m} WHERE rowid=?1"), [id])?;
        }
        tx.execute("DELETE FROM embedded WHERE chunk_id=?1", [id])?;
    }
    tx.execute("DELETE FROM chunks WHERE path=?1", [path])?;
    Ok(())
}

/// Replace a document's chunks (their vectors are re-made by the next embed pass).
pub fn replace_doc(c: &mut Connection, meta: &ChunkMeta, hash: &str, chunks: &[Chunk]) -> Result<()> {
    let tx = c.transaction()?;
    delete_doc_chunks(&tx, &meta.path)?;
    for ch in chunks {
        tx.execute(
            "INSERT INTO chunks(path, ord, person_id, purpose, doc_type, stack, section, title, content, hash)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![meta.path, ch.ord as i64, meta.person_id, meta.purpose, meta.doc_type, meta.stack, ch.section, ch.title, ch.content, ch.hash],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute("INSERT INTO chunks_fts(rowid, title, content) VALUES (?1,?2,?3)", params![id, ch.title, ch.content])?;
    }
    tx.execute(
        "INSERT INTO docs(path, hash, chunks, indexed_at) VALUES (?1,?2,?3,datetime('now'))
         ON CONFLICT(path) DO UPDATE SET hash=excluded.hash, chunks=excluded.chunks, indexed_at=excluded.indexed_at",
        params![meta.path, hash, chunks.len() as i64],
    )?;
    tx.commit()?;
    Ok(())
}

/// Remove documents that are no longer in the knowledge folder.
pub fn prune(c: &mut Connection, keep: &[String]) -> Result<usize> {
    let existing: Vec<String> = {
        let mut st = c.prepare("SELECT path FROM docs")?;
        let v = st.query_map([], |r| r.get(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
        v
    };
    let tx = c.transaction()?;
    let mut n = 0;
    for p in existing.iter().filter(|p| !keep.contains(p)) {
        delete_doc_chunks(&tx, p)?;
        tx.execute("DELETE FROM docs WHERE path=?1", [p])?;
        n += 1;
    }
    tx.commit()?;
    Ok(n)
}

/// Chunks without a vector from `model`: (id, text to embed).
pub fn pending(c: &Connection, model: &str, limit: usize) -> Result<Vec<(i64, String)>> {
    let mut st = c.prepare(
        "SELECT id, title, content FROM chunks
         WHERE id NOT IN (SELECT chunk_id FROM embedded WHERE model=?1) ORDER BY id LIMIT ?2",
    )?;
    let v = st
        .query_map(params![model, limit as i64], |r| {
            Ok((r.get::<_, i64>(0)?, format!("{}\n{}", r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(v)
}

pub fn put_vectors(c: &mut Connection, model: &str, rows: &[(i64, Vec<f32>)]) -> Result<()> {
    let tx = c.transaction()?;
    for (id, v) in rows {
        tx.execute(&format!("DELETE FROM vec_{model} WHERE rowid=?1"), [id])?;
        tx.execute(&format!("INSERT INTO vec_{model}(rowid, embedding) VALUES (?1, ?2)"), params![id, as_bytes(v)])?;
        tx.execute("INSERT OR REPLACE INTO embedded(chunk_id, model) VALUES (?1, ?2)", params![id, model])?;
    }
    tx.commit()?;
    Ok(())
}

pub fn embedded_count(c: &Connection, model: &str) -> Result<i64> {
    Ok(c.query_row("SELECT COUNT(*) FROM embedded WHERE model=?1", [model], |r| r.get(0))?)
}

/// Nearest chunks by cosine distance: (id, distance).
pub fn knn(c: &Connection, model: &str, query: &[f32], k: usize) -> Result<Vec<(i64, f64)>> {
    let mut st = c.prepare(&format!(
        "SELECT rowid, distance FROM vec_{model} WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance"
    ))?;
    let v = st
        .query_map(params![as_bytes(query), k as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(v)
}

/// Keyword matches by BM25: (id, score; lower is better).
pub fn keyword(c: &Connection, fts_query: &str, k: usize) -> Result<Vec<(i64, f64)>> {
    let mut st = c.prepare(
        "SELECT rowid, bm25(chunks_fts, 2.0, 1.0) AS s FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY s LIMIT ?2",
    )?;
    let v = st
        .query_map(params![fts_query, k as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(v)
}

pub fn chunks_by_id(c: &Connection, ids: &[i64]) -> Result<Vec<StoredChunk>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let list = ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    let mut st = c.prepare(&format!("SELECT {CHUNK_COLS} FROM chunks WHERE id IN ({list})"))?;
    let v = st.query_map([], row_chunk)?.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(v)
}

pub fn set_meta(c: &Connection, key: &str, value: &str) -> Result<()> {
    c.execute("INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)", params![key, value])?;
    Ok(())
}

pub fn get_meta(c: &Connection, key: &str) -> Result<Option<String>> {
    Ok(c.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0)).optional()?)
}

pub fn status(c: &Connection) -> Result<IndexStatus> {
    let mut s = IndexStatus {
        documents: c.query_row("SELECT COUNT(*) FROM docs", [], |r| r.get(0))?,
        chunks: c.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?,
        last_etl: get_meta(c, "last_etl_at")?,
        ..Default::default()
    };
    for m in MODELS {
        s.embedded.push((m.to_string(), embedded_count(c, m)?));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(ord: usize, title: &str, content: &str) -> Chunk {
        Chunk { ord, section: String::new(), title: title.into(), content: content.into(), hash: eh_knowledge::etl::sha(content) }
    }

    #[tokio::test]
    async fn index_embed_and_search() {
        let s = Store::open_in_memory().unwrap();
        s.call(|c| {
            let meta = ChunkMeta { path: "people/a/skills.json".into(), person_id: Some("a".into()), doc_type: "skills".into(), ..Default::default() };
            replace_doc(c, &meta, "h1", &[chunk(0, "skills", "Python and Rust"), chunk(1, "skills", "Azure and AWS")])?;
            assert_eq!(pending(c, "local", 10)?.len(), 2);
            let mut v1 = vec![0.0f32; DIMS];
            v1[0] = 1.0;
            let mut v2 = vec![0.0f32; DIMS];
            v2[1] = 1.0;
            let ids: Vec<i64> = pending(c, "local", 10)?.into_iter().map(|(i, _)| i).collect();
            put_vectors(c, "local", &[(ids[0], v1.clone()), (ids[1], v2)])?;
            assert!(pending(c, "local", 10)?.is_empty());
            let hits = knn(c, "local", &v1, 1)?;
            assert_eq!(hits[0].0, ids[0]);
            let kw = keyword(c, "\"azure\"", 5)?;
            assert_eq!(kw[0].0, ids[1]);
            // replacing the document drops its old vectors
            replace_doc(c, &meta, "h2", &[chunk(0, "skills", "Go")])?;
            assert_eq!(embedded_count(c, "local")?, 0);
            assert_eq!(prune(c, &[])?, 1);
            assert_eq!(status(c)?.chunks, 0);
            Ok(())
        })
        .await
        .unwrap();
    }
}
