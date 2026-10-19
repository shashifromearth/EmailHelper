//! Local knowledge base: the user's profile plus SQLite FTS5 retrieval over
//! every markdown, text and JSON file in the knowledge directory.

pub mod docx;
pub mod people;
pub mod profile;

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use sha2::{Digest, Sha256};

use crate::db::knowledge::{self as kdb, Snippet};
use crate::db::Db;
use crate::error::AppResult;
pub use people::{People, Person, Route};
pub use profile::Profile;

pub struct KnowledgeBase {
    dir: PathBuf,
    db: Db,
    people: RwLock<Arc<People>>,
}

/// Restricts retrieval to one person (and optionally one purpose) plus
/// `shared/`. The default scope searches everything (legacy mode).
#[derive(Debug, Clone, Default)]
pub struct Scope {
    /// Index prefix of the person's folder ("people/<id>"); None = everything.
    pub person_dir: Option<String>,
    pub purpose: Option<String>,
}

impl Scope {
    pub fn for_person(p: &Person, purpose: Option<&str>) -> Self {
        Self {
            person_dir: (!p.rel_dir.is_empty()).then(|| p.rel_dir.clone()),
            purpose: purpose.map(str::to_string),
        }
    }

    pub fn allows(&self, path: &str) -> bool {
        let Some(pd) = &self.person_dir else {
            return true;
        };
        if path.starts_with("shared/") {
            return true;
        }
        let Some(rest) = path.strip_prefix(&format!("{pd}/")) else {
            return false;
        };
        if !rest.contains('/') {
            return true; // person-level files (profile, resume.md, ...)
        }
        match &self.purpose {
            Some(purpose) => rest.starts_with(&format!("{purpose}/")),
            None => true,
        }
    }
}

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const CHUNK_CHARS: usize = 900;

impl KnowledgeBase {
    pub fn new(dir: PathBuf, db: Db) -> Self {
        let people = Arc::new(People::load(&dir));
        Self {
            dir,
            db,
            people: RwLock::new(people),
        }
    }

    pub fn people(&self) -> Arc<People> {
        self.people
            .read()
            .map(|p| p.clone())
            .unwrap_or_else(|e| e.into_inner().clone())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The default person's profile (legacy mode: the only profile).
    pub fn profile(&self) -> Arc<Profile> {
        self.people().default_person().profile.clone()
    }

    /// Re-index changed files and reload the profile. Cheap when nothing changed
    /// (only file metadata is read).
    pub async fn reindex(&self) -> AppResult<usize> {
        let dir = self.dir.clone();
        let files = tokio::task::spawn_blocking(move || collect_files(&dir)).await?;
        let mut changed = 0usize;
        let mut keep = Vec::with_capacity(files.len());
        for path in files {
            let rel = rel_path(&self.dir, &path);
            keep.push(rel.clone());
            let meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.len() > MAX_FILE_BYTES {
                tracing::warn!(file = %rel, "knowledge file too large; skipped");
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let rel2 = rel.clone();
            let existing = self.db.call(move |c| kdb::registry_entry(c, &rel2)).await?;
            if matches!(existing, Some((m, _)) if m == mtime) {
                continue;
            }
            let raw = if kind_of(&path) == "docx" {
                let p2 = path.clone();
                match tokio::task::spawn_blocking(move || docx::extract_text(&p2)).await? {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(file = %rel, error = %e, "unreadable docx");
                        continue;
                    }
                }
            } else {
                match tokio::fs::read_to_string(&path).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(file = %rel, error = %e, "unreadable knowledge file");
                        continue;
                    }
                }
            };
            let mut h = Sha256::new();
            h.update(raw.as_bytes());
            let hash = format!("{:x}", h.finalize());
            if matches!(&existing, Some((_, eh)) if *eh == hash) {
                continue;
            }
            let kind = kind_of(&path);
            let chunks = chunk_document(&rel, kind, &raw);
            let rel3 = rel.clone();
            self.db
                .call(move |c| kdb::replace_document(c, &rel3, kind, mtime, &hash, &chunks))
                .await?;
            changed += 1;
        }
        let removed = self.db.call(move |c| kdb::prune(c, &keep)).await?;
        if changed > 0 || removed > 0 {
            tracing::info!(changed, removed, "knowledge index updated");
            self.reload_people();
        }
        Ok(changed)
    }

    /// Re-read people.json and every person's files.
    pub fn reload_people(&self) {
        let p = Arc::new(People::load(&self.dir));
        if let Ok(mut w) = self.people.write() {
            *w = p;
        }
    }

    pub async fn search(&self, query: &str, limit: usize) -> AppResult<Vec<Snippet>> {
        self.search_scoped(query, limit, &Scope::default()).await
    }

    /// Search within one person's (and purpose's) knowledge plus `shared/`.
    pub async fn search_scoped(
        &self,
        query: &str,
        limit: usize,
        scope: &Scope,
    ) -> AppResult<Vec<Snippet>> {
        let q = query.to_string();
        let fetch = if scope.person_dir.is_some() {
            limit * 8
        } else {
            limit
        };
        let hits = self.db.call(move |c| kdb::search(c, &q, fetch)).await?;
        Ok(hits
            .into_iter()
            .filter(|h| scope.allows(&h.path))
            .take(limit)
            .collect())
    }
}

fn rel_path(base: &Path, p: &Path) -> String {
    p.strip_prefix(base)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

fn kind_of(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "json" => "json",
        "txt" => "text",
        "docx" => "docx",
        _ => "markdown",
    }
}

fn collect_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            // Private facts are never indexed (they must not reach prompts).
            if name.starts_with('.')
                || name.eq_ignore_ascii_case("private.json")
                || name.eq_ignore_ascii_case("private")
            {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("md" | "markdown" | "txt" | "json" | "docx")
            ) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Split a document into (title, content) chunks for FTS.
pub fn chunk_document(rel: &str, kind: &str, raw: &str) -> Vec<(String, String)> {
    let base_title = rel
        .rsplit('/')
        .next()
        .unwrap_or(rel)
        .trim_end_matches(".md")
        .trim_end_matches(".json")
        .trim_end_matches(".txt")
        .trim_end_matches(".docx")
        .replace(['_', '-'], " ");
    let text = if kind == "json" {
        match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(v) => {
                let mut lines = Vec::new();
                flatten_json("", &v, &mut lines);
                lines.join("\n")
            }
            Err(_) => raw.to_string(),
        }
    } else {
        raw.to_string()
    };

    let mut chunks = Vec::new();
    let mut title = base_title.clone();
    let mut buf = String::new();
    let flush = |title: &str, buf: &mut String, chunks: &mut Vec<(String, String)>| {
        let t = buf.trim();
        if !t.is_empty() {
            chunks.push((title.to_string(), t.to_string()));
        }
        buf.clear();
    };
    for line in text.lines() {
        if let Some(h) = line.strip_prefix('#') {
            flush(&title, &mut buf, &mut chunks);
            title = format!("{base_title}: {}", h.trim_start_matches('#').trim());
            continue;
        }
        if buf.len() + line.len() > CHUNK_CHARS && !buf.is_empty() {
            flush(&title, &mut buf, &mut chunks);
        }
        buf.push_str(line);
        buf.push('\n');
    }
    flush(&title, &mut buf, &mut chunks);
    chunks
}

fn flatten_json(prefix: &str, v: &serde_json::Value, out: &mut Vec<String>) {
    use serde_json::Value;
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                let key = if prefix.is_empty() {
                    k.replace('_', " ")
                } else {
                    format!("{prefix} {}", k.replace('_', " "))
                };
                flatten_json(&key, v, out);
            }
        }
        Value::Array(a) => {
            if a.iter().all(|x| !x.is_object() && !x.is_array()) {
                let items: Vec<String> = a
                    .iter()
                    .map(|x| {
                        x.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| x.to_string())
                    })
                    .collect();
                out.push(format!("{prefix}: {}", items.join(", ")));
            } else {
                for x in a {
                    flatten_json(prefix, x, out);
                    out.push(String::new());
                }
            }
        }
        Value::Null => {}
        Value::String(s) => out.push(format!("{prefix}: {s}")),
        other => out.push(format!("{prefix}: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_markdown_by_heading() {
        let c = chunk_document(
            "approved_responses/pricing.md",
            "markdown",
            "# Pricing\nStandard is 49 USD.\n## Enterprise\nContact sales.",
        );
        assert_eq!(c.len(), 2);
        assert!(c[0].0.contains("Pricing"));
        assert!(c[1].0.contains("Enterprise"));
    }

    #[test]
    fn flattens_json() {
        let c = chunk_document(
            "skills.json",
            "json",
            r#"{"core":["Rust","Python"],"years":{"rust":5}}"#,
        );
        let all = c.iter().map(|x| x.1.clone()).collect::<String>();
        assert!(all.contains("core: Rust, Python"));
        assert!(all.contains("years rust: 5"));
    }

    #[tokio::test]
    async fn reindex_detects_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("faq.md"),
            "# Shipping\nWe ship worldwide in 5 days.",
        )
        .unwrap();
        let db = Db::open_in_memory().unwrap();
        let kb = KnowledgeBase::new(dir.path().to_path_buf(), db);
        assert_eq!(kb.reindex().await.unwrap(), 1);
        assert_eq!(
            kb.reindex().await.unwrap(),
            0,
            "unchanged files are skipped"
        );
        let hits = kb.search("how long does shipping take", 3).await.unwrap();
        assert_eq!(hits.len(), 1);
        std::fs::remove_file(dir.path().join("faq.md")).unwrap();
        kb.reindex().await.unwrap();
        assert!(kb.search("shipping", 3).await.unwrap().is_empty());
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;

    #[test]
    fn scope_filters_paths() {
        let s = Scope {
            person_dir: Some("people/shashi".into()),
            purpose: Some("job_seeking".into()),
        };
        assert!(s.allows("people/shashi/profile.json"));
        assert!(s.allows("people/shashi/job_seeking/replies/recruiting.md"));
        assert!(!s.allows("people/shashi/consulting/rates.md"));
        assert!(!s.allows("people/himanshu/profile.json"));
        assert!(s.allows("shared/household.md"));
        assert!(Scope::default().allows("anything/at/all.md"));
    }

    #[tokio::test]
    async fn private_json_is_never_indexed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("private.json"), r#"{"pan": "ABCDE1234F"}"#).unwrap();
        std::fs::write(
            dir.path().join("notes.md"),
            "# Notes
PAN card questions go to the secure channel.",
        )
        .unwrap();
        let kb = KnowledgeBase::new(dir.path().to_path_buf(), Db::open_in_memory().unwrap());
        kb.reindex().await.unwrap();
        let hits = kb.search("ABCDE1234F pan", 5).await.unwrap();
        assert!(hits.iter().all(|h| !h.content.contains("ABCDE1234F")));
        assert!(hits.iter().any(|h| h.path == "notes.md"));
    }
}
