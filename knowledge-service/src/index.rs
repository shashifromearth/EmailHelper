//! The ETL run: walk the knowledge folder, re-chunk changed files, drop
//! removed ones, then embed new chunks with every available model.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

use eh_knowledge::etl;

use crate::embed::{Embedder, Kind};
use crate::error::{Error, Result};
use crate::store::{self, Store};

/// Files larger than this are skipped (scans, archives).
const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
const EMBED_BATCH: usize = 64;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub files: usize,
    pub changed: usize,
    pub removed: usize,
    pub chunks_written: usize,
    pub embedded: Vec<(String, usize)>,
    pub errors: Vec<String>,
    pub seconds: f64,
}

pub struct Indexer {
    pub dir: PathBuf,
    pub store: Store,
    pub embedders: Vec<Arc<dyn Embedder>>,
    running: AtomicBool,
}

/// Knowledge files to index; private facts never are.
pub fn collect(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if name.starts_with('.') || name == "private.json" || name == "private" || name == "people.json" {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if p
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| etl::EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn rel(base: &Path, p: &Path) -> String {
    p.strip_prefix(base).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

impl Indexer {
    pub fn new(dir: PathBuf, store: Store, embedders: Vec<Arc<dyn Embedder>>) -> Self {
        Self { dir, store, embedders, running: AtomicBool::new(false) }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Chunks still missing a vector from some model (e.g. after a quota stop).
    pub async fn missing_vectors(&self) -> usize {
        let ids: Vec<String> = self.embedders.iter().map(|e| e.id().to_string()).collect();
        self.store
            .call(move |c| {
                let total = store::status(c)?.chunks;
                Ok(ids.iter().map(|m| (total - store::embedded_count(c, m).unwrap_or(0)).max(0) as usize).sum())
            })
            .await
            .unwrap_or(0)
    }

    /// Embed what is missing without re-reading the folder.
    pub async fn top_up(&self) -> Result<Vec<(String, usize)>> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(Error::Invalid("an index run is already in progress".into()));
        }
        let mut done = Vec::new();
        for e in &self.embedders {
            match self.embed_pending(e.as_ref()).await {
                Ok(n) => done.push((e.id().to_string(), n)),
                Err(err) => tracing::info!(model = e.id(), error = %err, "embedding top-up stopped; will retry later"),
            }
        }
        self.running.store(false, Ordering::SeqCst);
        Ok(done)
    }

    /// One full ETL pass. Only one runs at a time.
    pub async fn run(&self) -> Result<Report> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(Error::Invalid("an index run is already in progress".into()));
        }
        let res = self.run_inner().await;
        self.running.store(false, Ordering::SeqCst);
        res
    }

    async fn run_inner(&self) -> Result<Report> {
        let started = Instant::now();
        let mut report = Report::default();
        let dir = self.dir.clone();
        let files = tokio::task::spawn_blocking(move || collect(&dir))
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;
        report.files = files.len();
        let mut keep = Vec::with_capacity(files.len());
        for path in files {
            let r = rel(&self.dir, &path);
            keep.push(r.clone());
            match self.index_file(&path, &r).await {
                Ok(Some(n)) => {
                    report.changed += 1;
                    report.chunks_written += n;
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(file = %r, error = %e, "could not index file");
                    report.errors.push(format!("{r}: {e}"));
                }
            }
        }
        report.removed = self.store.call(move |c| store::prune(c, &keep)).await?;
        for e in &self.embedders {
            match self.embed_pending(e.as_ref()).await {
                Ok(n) => report.embedded.push((e.id().to_string(), n)),
                Err(err) => {
                    tracing::warn!(model = e.id(), error = %err, "embedding pass stopped");
                    report.errors.push(format!("{} embeddings: {err}", e.id()));
                }
            }
        }
        report.seconds = started.elapsed().as_secs_f64();
        let json = serde_json::to_string(&report).unwrap_or_default();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        self.store
            .call(move |c| {
                store::set_meta(c, "last_etl_at", &chrono::Utc::now().to_rfc3339())?;
                store::set_meta(c, "last_etl_date", &today)?;
                store::set_meta(c, "last_etl_report", &json)
            })
            .await?;
        tracing::info!(files = report.files, changed = report.changed, removed = report.removed,
            chunks = report.chunks_written, errors = report.errors.len(), seconds = report.seconds, "knowledge index updated");
        Ok(report)
    }

    /// Re-chunk a file if its content changed; returns the new chunk count.
    async fn index_file(&self, path: &Path, rel: &str) -> Result<Option<usize>> {
        if std::fs::metadata(path)?.len() > MAX_FILE_BYTES {
            return Err(Error::Invalid("file too large".into()));
        }
        let bytes = tokio::fs::read(path).await?;
        // the ETL version is part of the hash: new chunking rules re-index everything
        let hash = etl::sha([etl::ETL_VERSION.as_bytes(), &bytes].concat());
        let (r2, h2) = (rel.to_string(), hash.clone());
        let unchanged = self.store.call(move |c| Ok(store::doc_hash(c, &r2)?.as_deref() == Some(h2.as_str()))).await?;
        if unchanged {
            return Ok(None);
        }
        let (p, r) = (path.to_path_buf(), rel.to_string());
        let (meta, chunks) = tokio::task::spawn_blocking(move || -> Result<_> {
            let meta = etl::meta_for(&r);
            let sections = etl::extract(&p)?;
            let chunks = etl::chunk(&etl::doc_title(&r, &meta), &sections);
            Ok((meta, chunks))
        })
        .await
        .map_err(|e| Error::Internal(e.to_string()))??;
        let n = chunks.len();
        self.store.call(move |c| store::replace_doc(c, &meta, &hash, &chunks)).await?;
        Ok(Some(n))
    }

    async fn embed_pending(&self, e: &dyn Embedder) -> Result<usize> {
        let model = e.id().to_string();
        let mut total = 0;
        loop {
            let m = model.clone();
            let batch = self.store.call(move |c| store::pending(c, &m, EMBED_BATCH)).await?;
            if batch.is_empty() {
                return Ok(total);
            }
            let texts: Vec<String> = batch.iter().map(|(_, t)| t.clone()).collect();
            let vectors = e.embed(&texts, Kind::Document).await?;
            let rows: Vec<(i64, Vec<f32>)> = batch.into_iter().map(|(id, _)| id).zip(vectors).collect();
            total += rows.len();
            let m = model.clone();
            self.store.call(move |c| store::put_vectors(c, &m, &rows)).await?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    /// Deterministic test embedder: one dimension per keyword.
    pub struct Fake;
    #[async_trait]
    impl Embedder for Fake {
        fn id(&self) -> &'static str {
            "local"
        }
        fn model(&self) -> &str {
            "fake"
        }
        async fn embed(&self, texts: &[String], _: Kind) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    let t = t.to_lowercase();
                    let mut v = vec![0.0f32; crate::embed::DIMS];
                    for (i, w) in ["python", "dotnet", "salary", "notice", "consulting"].iter().enumerate() {
                        if t.contains(w) {
                            v[i] = 1.0;
                        }
                    }
                    v[10] = 0.01;
                    v
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn indexes_incrementally_and_skips_private_files() {
        let dir = tempfile::tempdir().unwrap();
        let k = dir.path();
        std::fs::create_dir_all(k.join("people/asha/resumes/python")).unwrap();
        std::fs::write(k.join("people/asha/resumes/python/resume.md"), "# Skills\nPython, FastAPI\n").unwrap();
        std::fs::write(k.join("people/asha/private.json"), r#"{"pan":"ABCDE1234F"}"#).unwrap();
        let ix = Indexer::new(k.to_path_buf(), Store::open_in_memory().unwrap(), vec![Arc::new(Fake)]);
        let r = ix.run().await.unwrap();
        assert_eq!((r.files, r.changed), (1, 1), "private.json is never read: {r:?}");
        assert_eq!(r.embedded, vec![("local".to_string(), 1)]);
        let again = ix.run().await.unwrap();
        assert_eq!((again.changed, again.embedded[0].1), (0, 0), "unchanged files are skipped");
        std::fs::remove_file(k.join("people/asha/resumes/python/resume.md")).unwrap();
        assert_eq!(ix.run().await.unwrap().removed, 1);
    }
}
