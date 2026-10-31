//! Runtime configuration loaded from environment variables (and `.env`).
//!
//! Every setting has a safe default so the agent starts with zero configuration:
//! local Ollama, Telegram off, job and research agents off.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use chrono::NaiveTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelMode {
    Local,
    RemoteOpenAi,
    RemoteGemini,
    RemoteAnthropic,
    /// Groq's OpenAI-compatible API.
    RemoteGroq,
    /// Deterministic offline provider used for tests, demos and CI.
    Mock,
}

impl ModelMode {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_uppercase().as_str() {
            "REMOTE_OPENAI" | "OPENAI" => Self::RemoteOpenAi,
            "REMOTE_GEMINI" | "GEMINI" => Self::RemoteGemini,
            "REMOTE_ANTHROPIC" | "ANTHROPIC" => Self::RemoteAnthropic,
            "REMOTE_GROQ" | "GROQ" => Self::RemoteGroq,
            "MOCK" => Self::Mock,
            _ => Self::Local,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitMode {
    /// Submit automatically when score >= auto-apply threshold.
    Auto,
    /// Fill the form, then ask on Telegram before submitting.
    Review,
    /// Fill the form but never submit.
    DryRun,
}

impl SubmitMode {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Self::Auto,
            "dry_run" | "dry-run" | "dryrun" => Self::DryRun,
            _ => Self::Review,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Review => "review",
            Self::DryRun => "dry_run",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFormat {
    Json,
    Pretty,
}

#[derive(Clone, Debug)]
pub struct LlmConfig {
    pub mode: ModelMode,
    pub ollama_base_url: String,
    pub ollama_model: String,
    /// Context window requested from Ollama (its default is only 4096 tokens).
    pub ollama_num_ctx: u32,
    pub openai_api_key: Option<String>,
    pub openai_model: String,
    pub openai_base_url: String,
    pub gemini_api_key: Option<String>,
    pub gemini_model: String,
    pub gemini_base_url: String,
    pub anthropic_api_key: Option<String>,
    pub anthropic_model: String,
    pub anthropic_base_url: String,
    pub groq_api_key: Option<String>,
    pub groq_model: String,
    pub groq_base_url: String,
    pub timeout: Duration,
    /// When a remote provider fails, retry the request on local Ollama.
    pub fallback_to_local: bool,
    /// When a remote provider fails, first try the other cloud providers that
    /// have an API key (only applies when MODEL_MODE is a cloud mode).
    pub fallback_cloud: bool,
    /// LLM second-opinion review of drafts: Some(true/false) or None = auto
    /// (on for cloud models, off for local Ollama where it costs a full call).
    pub review: Option<bool>,
    /// Max concurrent LLM calls; 0 = auto (1 for local Ollama, 4 otherwise).
    pub max_parallel: usize,
    /// How long Ollama keeps the model loaded between calls.
    pub ollama_keep_alive: String,
}

#[derive(Clone, Debug)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub bot_token: Option<String>,
    pub chat_id: Option<i64>,
    pub api_base: String,
    pub poll_timeout_secs: u64,
}

impl TelegramConfig {
    /// Why the bot token can't be right, if it obviously can't. BotFather
    /// tokens look like `<numeric bot id>:<35-char secret>`. Never echoes it.
    pub fn token_problem(&self) -> Option<&'static str> {
        let t = self.bot_token.as_deref()?.trim();
        let Some((id, secret)) = t.split_once(':') else {
            return Some("TELEGRAM_BOT_TOKEN has no '<bot id>:' prefix; paste the complete token from BotFather (it looks like 123456789:AA...)");
        };
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
            return Some("TELEGRAM_BOT_TOKEN must start with the numeric bot id followed by ':'");
        }
        if secret.len() < 30 || !secret.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Some("TELEGRAM_BOT_TOKEN secret part after ':' looks truncated or has extra characters");
        }
        None
    }
}

#[derive(Clone, Debug)]
pub struct JobConfig {
    pub enabled: bool,
    pub submit_mode: SubmitMode,
    pub auto_apply_threshold: u8,
    pub review_threshold: u8,
    pub resume_path: PathBuf,
    pub worker_url: String,
    pub worker_token: String,
    pub worker_timeout: Duration,
    pub max_attempts: u32,
    /// Daily Telegram digest of jobs the person must apply to themselves.
    pub digest_time: NaiveTime,
    pub digest_max: usize,
    /// Queue job links on the job-links service (for the Chrome extension)
    /// instead of applying through the browser worker.
    pub links_enabled: bool,
    pub links_url: String,
    pub links_token: String,
}

#[derive(Clone, Debug)]
pub struct ResearchConfig {
    pub enabled: bool,
    pub daily_time: NaiveTime,
    pub searxng_url: Option<String>,
    pub search_terms: Vec<String>,
    pub rss_feeds: Vec<String>,
    pub greenhouse_boards: Vec<String>,
    pub lever_boards: Vec<String>,
    pub ashby_boards: Vec<String>,
    pub career_pages: Vec<String>,
    pub max_results_per_query: usize,
    pub summary_top_n: usize,
    pub greenhouse_api_base: String,
    pub lever_api_base: String,
    pub ashby_api_base: String,
    pub recruiter_enabled: bool,
    pub recruiter_linkedin_enabled: bool,
    pub recruiter_daily_limit: u32,
    /// Start applications for jobs found by discovery (requires the job agent).
    pub auto_apply: bool,
    /// Remote-job boards to read (remotive, remoteok, himalayas, jobicy, weworkremotely).
    pub remote_boards: Vec<String>,
    /// Endpoint overrides per board (RESEARCH_<BOARD>_URL), mainly for tests.
    pub remote_board_urls: Vec<(String, String)>,
    /// Drop postings older than this many hours (None = keep all).
    pub max_age_hours: Option<u32>,
    /// Applicants' country; None = the research person's authorized country.
    pub applicant_country: Option<String>,
    /// Tech stack a job must mention (empty = use the search terms instead).
    pub tech_stack: Vec<String>,
    /// Role types a job must be: contract / freelance / consultant (empty = any).
    pub work_types: Vec<String>,
    /// Jobs must be remote (or a distributed team).
    pub require_remote: bool,
    /// How often the scheduler checks whether today's discovery has run.
    pub check_interval_hours: u64,
    /// Run the daily scheduler (off in tests that trigger runs themselves).
    pub schedule_enabled: bool,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub allow_non_loopback: bool,
    pub api_token: String,
    pub data_dir: PathBuf,
    pub database_path: PathBuf,
    pub knowledge_dir: PathBuf,
    pub log_format: LogFormat,
    pub max_concurrency: usize,
    pub max_body_bytes: usize,
    pub max_email_text_bytes: usize,
    pub max_attachment_bytes: u64,
    pub workflow_version: u32,
    pub max_retries: u32,
    /// Mail older than this is recorded but not sent to the LLM (0 = no limit).
    pub max_email_age_hours: u64,
    pub llm: LlmConfig,
    pub telegram: TelegramConfig,
    pub jobs: JobConfig,
    pub research: ResearchConfig,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid value for {key}: {message}")]
    Invalid { key: &'static str, message: String },
}

/// Default search terms from the product brief.
pub const DEFAULT_SEARCH_TERMS: &[&str] = &[
    "AI Architect",
    "Lead AI Engineer",
    "Staff AI Engineer",
    "Rust Engineer",
    "Remote AI Jobs",
    "India AI Jobs",
    "GenAI Architect",
];

impl Config {
    /// Load from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Load using an arbitrary lookup function (used by tests).
    pub fn from_lookup<F>(get: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let s = |k: &str| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let s_or = |k: &str, d: &str| s(k).unwrap_or_else(|| d.to_string());
        let b = |k: &str, d: bool| s(k).map(|v| parse_bool(&v)).unwrap_or(d);
        let list = |k: &str| s(k).map(|v| split_list(&v)).unwrap_or_default();

        let bind_raw = s_or("EH_BIND_ADDR", "127.0.0.1:7800");
        let bind_addr: SocketAddr = bind_raw.parse().map_err(|e| ConfigError::Invalid {
            key: "EH_BIND_ADDR",
            message: format!("{bind_raw}: {e}"),
        })?;
        let allow_non_loopback = b("EH_ALLOW_NON_LOOPBACK", false);
        if !bind_addr.ip().is_loopback() && !allow_non_loopback {
            return Err(ConfigError::Invalid {
                key: "EH_BIND_ADDR",
                message: "must be a loopback address (set EH_ALLOW_NON_LOOPBACK=true only inside an isolated container network)".into(),
            });
        }

        let data_dir = PathBuf::from(s_or("EH_DATA_DIR", "./data"));
        let database_path = s("EH_DATABASE_PATH")
            .or_else(|| s("EH_DB_PATH"))
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("emailhelper.db"));
        let knowledge_dir = PathBuf::from(s_or("EH_KNOWLEDGE_DIR", "./knowledge"));

        let log_format = match s_or("EH_LOG_FORMAT", "json").to_ascii_lowercase().as_str() {
            "pretty" | "text" => LogFormat::Pretty,
            _ => LogFormat::Json,
        };

        let num = |k: &'static str, d: u64| -> Result<u64, ConfigError> {
            match s(k) {
                None => Ok(d),
                Some(v) => v.parse::<u64>().map_err(|e| ConfigError::Invalid {
                    key: k,
                    message: format!("{v}: {e}"),
                }),
            }
        };

        let llm = LlmConfig {
            mode: ModelMode::parse(&s_or("MODEL_MODE", "LOCAL")),
            ollama_base_url: s_or("OLLAMA_BASE_URL", "http://127.0.0.1:11434"),
            ollama_model: s_or("OLLAMA_MODEL", "qwen2.5:7b"),
            ollama_num_ctx: num("OLLAMA_NUM_CTX", 8192)?.clamp(2048, 131_072) as u32,
            openai_api_key: s("OPENAI_API_KEY"),
            openai_model: s_or("OPENAI_MODEL", "gpt-4o-mini"),
            openai_base_url: s_or("OPENAI_BASE_URL", "https://api.openai.com/v1"),
            gemini_api_key: s("GEMINI_API_KEY"),
            gemini_model: s_or("GEMINI_MODEL", "gemini-flash-latest"),
            gemini_base_url: s_or(
                "GEMINI_BASE_URL",
                "https://generativelanguage.googleapis.com/v1beta",
            ),
            anthropic_api_key: s("ANTHROPIC_API_KEY"),
            anthropic_model: s_or("ANTHROPIC_MODEL", "claude-haiku-5-5"),
            anthropic_base_url: s_or("ANTHROPIC_BASE_URL", "https://api.anthropic.com/v1"),
            groq_api_key: s("GROQ_API_KEY"),
            groq_model: s_or("GROQ_MODEL", "openai/gpt-oss-20b"),
            groq_base_url: s_or("GROQ_BASE_URL", "https://api.groq.com/openai/v1"),
            timeout: Duration::from_secs(num("LLM_TIMEOUT_SECS", 90)?),
            fallback_to_local: b("LLM_FALLBACK_TO_LOCAL", true),
            fallback_cloud: b("LLM_FALLBACK_CLOUD", true),
            review: match s("LLM_REVIEW").map(|v| v.to_ascii_lowercase()).as_deref() {
                None | Some("auto") => None,
                Some(v) => Some(parse_bool(v)),
            },
            max_parallel: num("LLM_MAX_PARALLEL", 0)? as usize,
            ollama_keep_alive: s_or("OLLAMA_KEEP_ALIVE", "24h"),
        };

        let chat_id = match s("TELEGRAM_CHAT_ID") {
            None => None,
            Some(v) => Some(v.parse::<i64>().map_err(|e| ConfigError::Invalid {
                key: "TELEGRAM_CHAT_ID",
                message: format!("{v}: {e}"),
            })?),
        };
        let telegram = TelegramConfig {
            enabled: b("TELEGRAM_ENABLED", false),
            bot_token: s("TELEGRAM_BOT_TOKEN"),
            chat_id,
            api_base: s_or("TELEGRAM_API_BASE", "https://api.telegram.org"),
            poll_timeout_secs: num("TELEGRAM_POLL_TIMEOUT_SECS", 50)?,
        };

        let auto_apply_threshold = num("JOB_AUTO_APPLY_THRESHOLD", 80)?.min(100) as u8;
        let review_threshold = num("JOB_REVIEW_THRESHOLD", 60)?.min(100) as u8;
        if review_threshold > auto_apply_threshold {
            return Err(ConfigError::Invalid {
                key: "JOB_REVIEW_THRESHOLD",
                message: "must be <= JOB_AUTO_APPLY_THRESHOLD".into(),
            });
        }
        let jobs = JobConfig {
            enabled: b("JOB_AGENT_ENABLED", false),
            submit_mode: SubmitMode::parse(&s_or("JOB_SUBMIT_MODE", "review")),
            auto_apply_threshold,
            review_threshold,
            resume_path: s("JOB_RESUME_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| knowledge_dir.join("resume.pdf")),
            worker_url: s_or("BROWSER_WORKER_URL", "http://127.0.0.1:7801"),
            worker_token: s("BROWSER_WORKER_TOKEN").unwrap_or_default(),
            worker_timeout: Duration::from_secs(num("BROWSER_WORKER_TIMEOUT_SECS", 180)?),
            max_attempts: num("JOB_MAX_ATTEMPTS", 3)? as u32,
            digest_time: {
                let raw = s_or("JOB_DIGEST_TIME", "19:00");
                NaiveTime::parse_from_str(&raw, "%H:%M").map_err(|e| ConfigError::Invalid {
                    key: "JOB_DIGEST_TIME",
                    message: format!("{raw}: {e}"),
                })?
            },
            digest_max: num("JOB_DIGEST_MAX", 15)? as usize,
            links_enabled: b("JOB_LINKS_ENABLED", false),
            links_url: s_or("JOB_LINKS_URL", "http://127.0.0.1:7810"),
            links_token: s("JOB_LINKS_TOKEN").unwrap_or_default(),
        };

        let daily_raw = s_or("RESEARCH_DAILY_TIME", "07:30");
        let daily_time =
            NaiveTime::parse_from_str(&daily_raw, "%H:%M").map_err(|e| ConfigError::Invalid {
                key: "RESEARCH_DAILY_TIME",
                message: format!("{daily_raw}: {e} (expected HH:MM)"),
            })?;
        let mut search_terms = list("RESEARCH_SEARCH_TERMS");
        if search_terms.is_empty() {
            search_terms = DEFAULT_SEARCH_TERMS.iter().map(|s| s.to_string()).collect();
        }
        let research = ResearchConfig {
            enabled: b("RESEARCH_ENABLED", false),
            daily_time,
            searxng_url: s("SEARXNG_URL"),
            search_terms,
            rss_feeds: list("RESEARCH_RSS_FEEDS"),
            greenhouse_boards: list("RESEARCH_GREENHOUSE_BOARDS"),
            lever_boards: list("RESEARCH_LEVER_BOARDS"),
            ashby_boards: list("RESEARCH_ASHBY_BOARDS"),
            career_pages: list("RESEARCH_CAREER_PAGES"),
            max_results_per_query: num("RESEARCH_MAX_RESULTS_PER_QUERY", 20)? as usize,
            summary_top_n: num("RESEARCH_SUMMARY_TOP_N", 10)? as usize,
            greenhouse_api_base: s_or(
                "GREENHOUSE_API_BASE",
                "https://boards-api.greenhouse.io/v1/boards",
            ),
            lever_api_base: s_or("LEVER_API_BASE", "https://api.lever.co/v0/postings"),
            ashby_api_base: s_or(
                "ASHBY_API_BASE",
                "https://api.ashbyhq.com/posting-api/job-board",
            ),
            recruiter_enabled: b("RECRUITER_DISCOVERY_ENABLED", false),
            recruiter_linkedin_enabled: b("RECRUITER_LINKEDIN_ENABLED", false),
            recruiter_daily_limit: num("RECRUITER_DAILY_LIMIT", 20)? as u32,
            auto_apply: b("RESEARCH_AUTO_APPLY", false),
            remote_boards: list_or_default(
                s("RESEARCH_REMOTE_BOARDS"),
                crate::research::remote_boards::BOARDS,
            ),
            remote_board_urls: crate::research::remote_boards::BOARDS
                .iter()
                .filter_map(|b| {
                    let key = format!("RESEARCH_{}_URL", b.to_ascii_uppercase());
                    s(&key).map(|v| (b.to_string(), v))
                })
                .collect(),
            max_age_hours: match num("RESEARCH_MAX_AGE_HOURS", 25)? {
                0 => None,
                h => Some(h as u32),
            },
            applicant_country: s("RESEARCH_APPLICANT_COUNTRY"),
            tech_stack: list_or_default(
                s("RESEARCH_TECH_STACK"),
                &["python", "c#", ".net", "java", "genai", "agentic ai"],
            ),
            work_types: list_or_default(s("RESEARCH_WORK_TYPES"), &["contract", "freelance", "consultant"]),
            require_remote: b("RESEARCH_REQUIRE_REMOTE", true),
            check_interval_hours: num("RESEARCH_CHECK_INTERVAL_HOURS", 4)?.clamp(1, 24),
            schedule_enabled: b("RESEARCH_SCHEDULE_ENABLED", true),
        };

        let max_concurrency = num("EH_MAX_CONCURRENCY", 10)?.clamp(1, 256) as usize;

        Ok(Self {
            bind_addr,
            allow_non_loopback,
            api_token: s("EH_API_TOKEN").unwrap_or_default(),
            data_dir,
            database_path,
            knowledge_dir,
            log_format,
            max_concurrency,
            max_body_bytes: num("EH_MAX_BODY_BYTES", 256 * 1024)? as usize,
            max_email_text_bytes: num("EH_MAX_EMAIL_TEXT_BYTES", 64 * 1024)? as usize,
            max_attachment_bytes: num("EH_MAX_ATTACHMENT_BYTES", 25 * 1024 * 1024)?,
            workflow_version: num("EH_WORKFLOW_VERSION", 1)? as u32,
            max_retries: num("EH_MAX_RETRIES", 3)? as u32,
            max_email_age_hours: num("EH_MAX_EMAIL_AGE_HOURS", 72)?,
            llm,
            telegram,
            jobs,
            research,
        })
    }

    /// Whether drafts get an LLM second-opinion review (see `LlmConfig::review`).
    pub fn llm_review_enabled(&self) -> bool {
        self.llm.review.unwrap_or(self.llm.mode != ModelMode::Local)
    }

    /// Telegram is usable only when enabled *and* fully configured.
    pub fn telegram_active(&self) -> bool {
        self.telegram.enabled
            && self.telegram.bot_token.is_some()
            && self.telegram.chat_id.is_some()
    }
}

pub fn parse_bool(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on" | "y"
    )
}

/// Split `a;b;c` (or newline/comma separated when no `;` present) into trimmed items.
/// A list setting: unset → `default`; "none"/"off" → empty.
fn list_or_default(v: Option<String>, default: &[&str]) -> Vec<String> {
    match v {
        None => default.iter().map(|s| s.to_string()).collect(),
        Some(v) if matches!(v.trim().to_ascii_lowercase().as_str(), "none" | "off" | "-") => Vec::new(),
        Some(v) => split_list(&v),
    }
}

pub fn split_list(v: &str) -> Vec<String> {
    let sep: &[char] = if v.contains(';') {
        &[';', '\n']
    } else {
        &[',', '\n']
    };
    v.split(sep)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|k| m.get(k).cloned())
    }

    #[test]
    fn defaults_are_safe() {
        let c = cfg(&[]).unwrap();
        assert_eq!(c.bind_addr.to_string(), "127.0.0.1:7800");
        assert_eq!(c.llm.mode, ModelMode::Local);
        assert!(!c.telegram.enabled);
        assert!(!c.jobs.enabled);
        assert_eq!(c.jobs.submit_mode, SubmitMode::Review);
        assert!(!c.research.enabled);
        assert!(!c.research.auto_apply);
        assert_eq!(c.max_concurrency, 10);
        assert_eq!(c.research.search_terms.len(), DEFAULT_SEARCH_TERMS.len());
        assert!(!c.telegram_active());
    }

    #[test]
    fn rejects_public_bind() {
        assert!(cfg(&[("EH_BIND_ADDR", "0.0.0.0:7800")]).is_err());
        assert!(cfg(&[
            ("EH_BIND_ADDR", "0.0.0.0:7800"),
            ("EH_ALLOW_NON_LOOPBACK", "true")
        ])
        .is_ok());
    }

    #[test]
    fn parses_lists_and_modes() {
        let c = cfg(&[
            ("RESEARCH_SEARCH_TERMS", "Rust Engineer; AI Architect"),
            ("RESEARCH_GREENHOUSE_BOARDS", "acme,globex"),
            ("MODEL_MODE", "remote_gemini"),
            ("JOB_SUBMIT_MODE", "auto"),
        ])
        .unwrap();
        assert_eq!(
            c.research.search_terms,
            vec!["Rust Engineer", "AI Architect"]
        );
        assert_eq!(c.research.greenhouse_boards, vec!["acme", "globex"]);
        assert_eq!(c.llm.mode, ModelMode::RemoteGemini);
        assert_eq!(c.jobs.submit_mode, SubmitMode::Auto);
    }

    #[test]
    fn thresholds_validated() {
        assert!(cfg(&[("JOB_REVIEW_THRESHOLD", "90")]).is_err());
        assert!(cfg(&[("RESEARCH_DAILY_TIME", "7am")]).is_err());
    }

    #[test]
    fn telegram_requires_token_and_chat() {
        let c = cfg(&[("TELEGRAM_ENABLED", "true"), ("TELEGRAM_BOT_TOKEN", "x")]).unwrap();
        assert!(c.telegram.token_problem().is_some());
        let ok = cfg(&[("TELEGRAM_BOT_TOKEN", "123456789:AAdummyabcdefghijklmnopqrstuvwxyz01234")]).unwrap();
        assert_eq!(ok.telegram.token_problem(), None);
        let no_id = cfg(&[("TELEGRAM_BOT_TOKEN", "AAdummyabcdefghijklmnopqrstuvwxyz01234")]).unwrap();
        assert!(no_id.telegram.token_problem().unwrap().contains("prefix"));
        assert!(!c.telegram_active());
        let c = cfg(&[
            ("TELEGRAM_ENABLED", "true"),
            ("TELEGRAM_BOT_TOKEN", "x"),
            ("TELEGRAM_CHAT_ID", "-100123"),
        ])
        .unwrap();
        assert!(c.telegram_active());
    }
}
