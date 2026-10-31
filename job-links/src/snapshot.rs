//! JSON snapshot of the store: loaded at startup, written atomically (temp
//! file + rename) every few seconds when something changed and on shutdown.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::model::LinkRecord;
use crate::store::Store;

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    saved_at: DateTime<Utc>,
    links: Vec<LinkRecord>,
}

/// Records from `path`; a missing file is an empty store. A file that can't
/// be read is an error, never silently replaced.
pub fn load(path: &Path) -> anyhow::Result<Vec<LinkRecord>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let snap: Snapshot = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "{} is not a valid job-links snapshot; move it aside to start empty",
            path.display()
        )
    })?;
    anyhow::ensure!(
        snap.version <= VERSION,
        "{} was written by a newer job-links (version {})",
        path.display(),
        snap.version
    );
    Ok(snap.links)
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Whether a snapshot could be written next to `path` (for `/readyz`).
pub fn check_writable(path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".probe");
    let probe = path.with_file_name(name);
    std::fs::write(&probe, b"ok")?;
    std::fs::remove_file(&probe)
}

/// Write `links` to `path` atomically.
pub fn save(path: &Path, links: Vec<LinkRecord>) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let snap = Snapshot {
        version: VERSION,
        saved_at: Utc::now(),
        links,
    };
    let bytes = serde_json::to_vec_pretty(&snap)?;
    let tmp = tmp_path(path);
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Save if dirty. Returns whether a file was written.
pub async fn flush(store: &Arc<Store>, path: &Path) -> std::io::Result<bool> {
    let Some(records) = store.take_dirty() else {
        return Ok(false);
    };
    let p = path.to_path_buf();
    let res = tokio::task::spawn_blocking(move || save(&p, records))
        .await
        .map_err(std::io::Error::other)?;
    if res.is_err() {
        store.mark_dirty(); // retry on the next tick
    }
    res.map(|_| true)
}

/// Flush every `every` until `stop` turns true, then flush once more.
pub async fn run(store: Arc<Store>, path: PathBuf, every: Duration, mut stop: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = stop.changed() => break,
        }
        if let Err(e) = flush(&store, &path).await {
            tracing::error!(error = %e, path = %path.display(), "snapshot write failed");
        }
    }
    match flush(&store, &path).await {
        Ok(_) => tracing::info!(path = %path.display(), links = store.len(), "snapshot saved"),
        Err(e) => tracing::error!(error = %e, path = %path.display(), "final snapshot write failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NewLink;

    #[tokio::test]
    async fn round_trip_and_flush_only_when_dirty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("links.json");
        assert!(load(&path).unwrap().is_empty());

        let store = Arc::new(Store::new());
        store
            .upsert(
                NewLink {
                    url: "https://jobs.lever.co/acme/1".into(),
                    title: Some("Engineer".into()),
                    ..Default::default()
                },
                Utc::now(),
            )
            .unwrap();
        assert!(flush(&store, &path).await.unwrap());
        assert!(!flush(&store, &path).await.unwrap(), "nothing changed");
        assert!(!tmp_path(&path).exists());

        let loaded = load(&path).unwrap();
        assert_eq!(loaded, store.records());
        let again = Store::from_records(loaded);
        assert_eq!(again.len(), 1);
        // Dedupe index is rebuilt.
        let up = again
            .upsert(
                NewLink {
                    url: "https://jobs.lever.co/acme/1/".into(),
                    ..Default::default()
                },
                Utc::now(),
            )
            .unwrap();
        assert!(!up.created);
    }

    #[test]
    fn writability_check() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_writable(&dir.path().join("a").join("links.json")).is_ok());
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, b"x").unwrap();
        assert!(check_writable(&blocker.join("links.json")).is_err(), "parent is a file");
    }

    #[test]
    fn corrupt_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("links.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(load(&path).unwrap_err().to_string().contains("move it aside"));
    }

    #[tokio::test]
    async fn run_saves_on_stop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("links.json");
        let store = Arc::new(Store::new());
        let (tx, rx) = watch::channel(false);
        let task = tokio::spawn(run(store.clone(), path.clone(), Duration::from_secs(3600), rx));
        store
            .upsert(
                NewLink {
                    url: "https://jobs.lever.co/acme/2".into(),
                    ..Default::default()
                },
                Utc::now(),
            )
            .unwrap();
        tx.send(true).unwrap();
        task.await.unwrap();
        assert_eq!(load(&path).unwrap().len(), 1);
    }
}
