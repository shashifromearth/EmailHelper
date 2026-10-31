//! Settings from environment variables (and `.env`).

use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    /// Always bound on 127.0.0.1.
    pub port: u16,
    /// Shared secret for the `x-joblinks-token` header.
    pub token: String,
    pub snapshot_path: PathBuf,
    pub snapshot_interval: Duration,
    /// Lease given to a claim that doesn't ask for one.
    pub default_lease: Duration,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let s = |k: &str| get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let num = |k: &str, d: u64| -> anyhow::Result<u64> {
            match s(k) {
                None => Ok(d),
                Some(v) => v
                    .parse()
                    .map_err(|e| anyhow::anyhow!("invalid value for {k}: {v}: {e}")),
            }
        };
        let token = s("JOB_LINKS_TOKEN").unwrap_or_default();
        anyhow::ensure!(
            token.len() >= 16,
            "JOB_LINKS_TOKEN must be set to a shared secret of at least 16 characters"
        );
        let port = num("JOB_LINKS_PORT", 7810)?;
        anyhow::ensure!(port <= u16::MAX as u64, "invalid value for JOB_LINKS_PORT: {port}");
        Ok(Self {
            port: port as u16,
            token,
            snapshot_path: PathBuf::from(
                s("JOB_LINKS_SNAPSHOT").unwrap_or_else(|| "data/job-links.json".into()),
            ),
            snapshot_interval: Duration::from_secs(num("JOB_LINKS_SNAPSHOT_SECS", 5)?.max(1)),
            default_lease: Duration::from_secs(num("JOB_LINKS_LEASE_SECS", 900)?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg(pairs: &[(&str, &str)]) -> anyhow::Result<Config> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Config::from_lookup(|k| m.get(k).cloned())
    }

    #[test]
    fn defaults_and_token_required() {
        assert!(cfg(&[]).is_err());
        assert!(cfg(&[("JOB_LINKS_TOKEN", "short")]).is_err());
        let c = cfg(&[("JOB_LINKS_TOKEN", "0123456789abcdef")]).unwrap();
        assert_eq!(c.port, 7810);
        assert_eq!(c.snapshot_path, PathBuf::from("data/job-links.json"));
        assert_eq!(c.snapshot_interval, Duration::from_secs(5));
        assert!(cfg(&[("JOB_LINKS_TOKEN", "0123456789abcdef"), ("JOB_LINKS_PORT", "70000")]).is_err());
    }
}
