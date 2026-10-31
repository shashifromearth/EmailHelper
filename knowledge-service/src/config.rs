//! Settings from the environment (and ./.env).

use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub addr: SocketAddr,
    /// Required in requests as `x-knowledge-token` (empty = no auth, local tests only).
    pub token: String,
    pub knowledge_dir: PathBuf,
    pub db_path: PathBuf,
    pub gemini_key: Option<String>,
    pub gemini_base: String,
    pub gemini_model: String,
    pub ollama_base: String,
    pub local_model: String,
    /// How often to check whether today's ETL has run.
    pub etl_check_hours: u64,
    pub json_logs: bool,
}

fn get(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let addr: SocketAddr = get("KNOWLEDGE_SERVICE_ADDR")
            .unwrap_or_else(|| "127.0.0.1:7820".into())
            .parse()
            .map_err(|e| format!("KNOWLEDGE_SERVICE_ADDR: {e}"))?;
        if !addr.ip().is_loopback() && get("KNOWLEDGE_SERVICE_ALLOW_REMOTE").as_deref() != Some("true") {
            return Err("KNOWLEDGE_SERVICE_ADDR must be a loopback address (set KNOWLEDGE_SERVICE_ALLOW_REMOTE=true to override)".into());
        }
        let data = PathBuf::from(get("EH_DATA_DIR").unwrap_or_else(|| "data".into()));
        Ok(Self {
            addr,
            token: get("KNOWLEDGE_SERVICE_TOKEN").or_else(|| get("EH_API_TOKEN")).unwrap_or_default(),
            knowledge_dir: PathBuf::from(get("EH_KNOWLEDGE_DIR").unwrap_or_else(|| "knowledge".into())),
            db_path: get("KNOWLEDGE_DB").map(PathBuf::from).unwrap_or_else(|| data.join("knowledge.db")),
            gemini_key: if get("RAG_GEMINI_ENABLED").as_deref() == Some("false") { None } else { get("GEMINI_API_KEY") },
            gemini_base: get("GEMINI_BASE_URL").unwrap_or_else(|| "https://generativelanguage.googleapis.com/v1beta".into()),
            gemini_model: get("RAG_GEMINI_MODEL").unwrap_or_else(|| "gemini-embedding-001".into()),
            ollama_base: get("OLLAMA_BASE_URL").unwrap_or_else(|| "http://127.0.0.1:11434".into()),
            local_model: get("RAG_LOCAL_MODEL").unwrap_or_else(|| "nomic-embed-text".into()),
            etl_check_hours: get("RAG_ETL_CHECK_HOURS").and_then(|v| v.parse().ok()).unwrap_or(6).clamp(1, 24),
            json_logs: get("EH_LOG_FORMAT").map(|v| v != "pretty").unwrap_or(true),
        })
    }
}
