//! SQLite storage. A single connection guarded by a mutex is used from
//! `spawn_blocking`; SQLite serialises writers anyway and every statement here
//! is short, so this keeps memory low and behaviour predictable.

pub mod audit;
pub mod drafts;
pub mod emails;
pub mod jobs;
pub mod knowledge;
pub mod queue;
pub mod settings;

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::error::{AppError, AppResult};

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("migrations/001_init.sql")),
    (2, include_str!("migrations/002_people.sql")),
    (3, include_str!("migrations/003_attachments.sql")),
];

impl Db {
    /// Open (creating if needed) the database file and run migrations.
    pub fn open(path: &Path) -> AppResult<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// In-memory database (tests).
    pub fn open_in_memory() -> AppResult<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> AppResult<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA temp_store=MEMORY;",
        )?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate_blocking()?;
        Ok(db)
    }

    fn migrate_blocking(&self) -> AppResult<()> {
        let mut conn = self.lock()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations(
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL)",
        )?;
        for (version, sql) in MIGRATIONS {
            let applied: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=?1)",
                [version],
                |r| r.get(0),
            )?;
            if applied {
                continue;
            }
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES(?1, ?2)",
                rusqlite::params![version, now()],
            )?;
            tx.commit()?;
            tracing::info!(version, "applied database migration");
        }
        Ok(())
    }

    fn lock(&self) -> AppResult<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| AppError::other("database mutex poisoned"))
    }

    /// Run a closure against the connection on the blocking thread pool.
    pub async fn call<F, R>(&self, f: F) -> AppResult<R>
    where
        F: FnOnce(&mut Connection) -> AppResult<R> + Send + 'static,
        R: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn
                .lock()
                .map_err(|_| AppError::other("database mutex poisoned"))?;
            f(&mut guard)
        })
        .await?
    }

    /// Cheap liveness probe used by `/readiness`.
    pub async fn ping(&self) -> bool {
        self.call(|c| Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?))
            .await
            .is_ok()
    }
}

/// RFC 3339 UTC timestamp with millisecond precision. Sorts lexicographically.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

pub fn to_json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

pub fn from_json<T: serde::de::DeserializeOwned + Default>(s: Option<String>) -> T {
    s.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        {
            let db = Db::open(&path).unwrap();
            assert!(db.ping().await);
        }
        // Re-open: migrations must not re-run or fail.
        let db = Db::open(&path).unwrap();
        let n: i64 = db
            .call(|c| Ok(c.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(n, MIGRATIONS.len() as i64);
    }

    #[tokio::test]
    async fn all_tables_exist() {
        let db = Db::open_in_memory().unwrap();
        for t in [
            "emails",
            "jobs",
            "applications",
            "application_status",
            "recruiters",
            "processing_jobs",
            "audit_logs",
            "drafts",
            "settings",
            "knowledge_registry",
            "knowledge_fts",
            "human_questions",
            "answer_memory",
            "interviews",
            "research_findings",
            "person_facts",
        ] {
            let t2 = t.to_string();
            let exists: bool = db
                .call(move |c| {
                    Ok(c.query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)",
                        [t2],
                        |r| r.get(0),
                    )?)
                })
                .await
                .unwrap();
            assert!(exists, "missing table {t}");
        }
    }
}
