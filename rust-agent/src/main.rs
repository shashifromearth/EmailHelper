use std::process::ExitCode;

use emailhelper::app::{App, AppOverrides};
use emailhelper::config::{Config, LogFormat};
use tracing_subscriber::EnvFilter;

const USAGE: &str =
    "emailhelper [serve|migrate|discover-now|reindex|telegram-chats|telegram-test|llm-test|healthcheck|version]

  serve         Run the agent (default)
  migrate       Create/upgrade the SQLite schema and exit
  discover-now  Run one research discovery pass and exit
  reindex       Re-index the knowledge directory and exit
  telegram-chats  List chats that messaged the bot (to fill TELEGRAM_CHAT_ID / people.json)
  telegram-test   Check the bot token and send a test message to TELEGRAM_CHAT_ID
  llm-test        Send a one-line test prompt to every configured model provider
  healthcheck   Exit 0 if the running agent reports healthy (for Docker/systemd)
  version       Print the version

Configuration is read from environment variables and ./.env (or EH_ENV_FILE).";

fn init_tracing(format: LogFormat) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tower_http=warn,hyper=warn,reqwest=warn"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false);
    match format {
        LogFormat::Json => builder
            .json()
            .flatten_event(true)
            .with_current_span(true)
            .init(),
        LogFormat::Pretty => builder.init(),
    }
}

/// Use EH_API_TOKEN, or generate one and persist it under the data directory.
fn ensure_api_token(cfg: &mut Config) -> std::io::Result<()> {
    if !cfg.api_token.is_empty() {
        if cfg.api_token.len() < 16 {
            tracing::warn!("EH_API_TOKEN is shorter than 16 characters; use a long random value");
        }
        return Ok(());
    }
    let path = cfg.data_dir.join("api-token.txt");
    if let Ok(t) = std::fs::read_to_string(&path) {
        let t = t.trim().to_string();
        if !t.is_empty() {
            cfg.api_token = t;
            return Ok(());
        }
    }
    std::fs::create_dir_all(&cfg.data_dir)?;
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    std::fs::write(&path, &token)?;
    tracing::warn!(path = %path.display(), "EH_API_TOKEN not set; generated a token (configure the same value in the Thunderbird extension)");
    cfg.api_token = token;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    tracing::info!("shutdown signal received");
}

async fn healthcheck(cfg: &Config) -> ExitCode {
    let mut addr = cfg.bind_addr;
    if addr.ip().is_unspecified() {
        addr.set_ip(std::net::Ipv4Addr::LOCALHOST.into());
    }
    let url = format!("http://{addr}/readiness");
    match reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let env_file = std::env::var("EH_ENV_FILE").unwrap_or_else(|_| ".env".into());
    let _ = dotenvy::from_filename(&env_file);
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "serve".into());
    if matches!(cmd.as_str(), "-h" | "--help" | "help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if matches!(cmd.as_str(), "version" | "--version" | "-V") {
        println!("emailhelper {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let mut cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("configuration error: {e}");
            return ExitCode::from(2);
        }
    };
    init_tracing(cfg.log_format);
    if cfg.telegram.enabled {
        if let Some(problem) = cfg.telegram.token_problem() {
            tracing::warn!(problem, "Telegram is enabled but the bot token is malformed; notifications will fail");
        }
    }

    match cmd.as_str() {
        "healthcheck" => healthcheck(&cfg).await,
        "telegram-chats" => telegram_chats(&cfg).await,
        "telegram-test" => telegram_test(&cfg).await,
        "llm-test" => llm_test(&cfg).await,
        "migrate" => match emailhelper::db::Db::open(&cfg.database_path) {
            Ok(_) => {
                tracing::info!(path = %cfg.database_path.display(), "database ready");
                ExitCode::SUCCESS
            }
            Err(e) => {
                tracing::error!(error = %e, "migration failed");
                ExitCode::FAILURE
            }
        },
        "reindex" | "discover-now" | "serve" => {
            if let Err(e) = ensure_api_token(&mut cfg) {
                tracing::error!(error = %e, "cannot prepare API token");
                return ExitCode::FAILURE;
            }
            let app = match App::build(cfg, AppOverrides::default()) {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!(error = %e, "startup failed");
                    return ExitCode::FAILURE;
                }
            };
            match cmd.as_str() {
                "reindex" => match app.knowledge.reindex().await {
                    Ok(n) => {
                        tracing::info!(changed = n, "knowledge reindexed");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "reindex failed");
                        ExitCode::FAILURE
                    }
                },
                "discover-now" => {
                    let _ = app.knowledge.reindex().await;
                    match emailhelper::research::discovery::run(&app).await {
                        Ok(r) => {
                            println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
                            ExitCode::SUCCESS
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "discovery failed");
                            ExitCode::FAILURE
                        }
                    }
                }
                _ => serve(app).await,
            }
        }
        other => {
            eprintln!("unknown command: {other}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

async fn serve(app: std::sync::Arc<App>) -> ExitCode {
    let started = std::time::Instant::now();
    let bg = match emailhelper::start_background(&app).await {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "failed to start background workers");
            return ExitCode::FAILURE;
        }
    };
    let listener = match tokio::net::TcpListener::bind(app.cfg.bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, addr = %app.cfg.bind_addr, "cannot bind");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(
        addr = %app.cfg.bind_addr,
        version = env!("CARGO_PKG_VERSION"),
        llm_provider = app.llm.provider_name(),
        llm_model = app.llm.model_name(),
        telegram = app.notifier.enabled(),
        job_agent = app.cfg.jobs.enabled,
        submit_mode = app.cfg.jobs.submit_mode.as_str(),
        research = app.cfg.research.enabled,
        startup_ms = started.elapsed().as_millis() as u64,
        "EmailHelper agent listening"
    );
    let router = emailhelper::api::router(app.clone());
    let res = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await;
    bg.abort();
    match res {
        Ok(()) => {
            tracing::info!("stopped");
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!(error = %e, "server error");
            ExitCode::FAILURE
        }
    }
}

/// List chats that have messaged the bot, so their ids can go into
/// TELEGRAM_CHAT_ID / people.json. Reads the token from the environment; the
/// token itself is never printed.
async fn telegram_test(cfg: &Config) -> ExitCode {
    let Some(token) = cfg.telegram.bot_token.as_deref() else {
        eprintln!("Set TELEGRAM_BOT_TOKEN in .env first.");
        return ExitCode::from(2);
    };
    if let Some(problem) = cfg.telegram.token_problem() {
        eprintln!("{problem}");
        return ExitCode::from(2);
    }
    let chat = cfg.telegram.chat_id.unwrap_or(0);
    let client = emailhelper::telegram::TelegramClient::new(
        reqwest::Client::new(),
        &cfg.telegram.api_base,
        token.trim(),
        chat,
    );
    match client.get_me().await {
        Ok(name) => println!("Bot token OK: @{name}"),
        Err(e) => {
            eprintln!("Bot token rejected: {e}");
            return ExitCode::FAILURE;
        }
    }
    if chat == 0 {
        eprintln!("TELEGRAM_CHAT_ID is not set; send /start to the bot, then run `emailhelper telegram-chats`.");
        return ExitCode::from(2);
    }
    match client
        .send("EmailHelper test: Telegram notifications are working.", None)
        .await
    {
        Ok(_) => {
            println!("Test message sent to chat {chat}.");
            if !cfg.telegram.enabled {
                println!("Note: TELEGRAM_ENABLED is false, so the agent won't use Telegram yet.");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Sending to chat {chat} failed: {e} (has that account sent /start to the bot?)");
            ExitCode::FAILURE
        }
    }
}

async fn llm_test(cfg: &Config) -> ExitCode {
    use emailhelper::config::ModelMode;
    use emailhelper::llm::{LlmRequest, LlmRouter, LlmTask};
    let http = reqwest::Client::builder()
        .timeout(cfg.llm.timeout)
        .build()
        .unwrap_or_default();
    let metrics = std::sync::Arc::new(emailhelper::metrics::Metrics::new());
    println!("MODEL_MODE primary: {:?}", cfg.llm.mode);
    let mut failed = false;
    for (mode, has_key) in [
        (ModelMode::RemoteGroq, cfg.llm.groq_api_key.is_some()),
        (ModelMode::RemoteGemini, cfg.llm.gemini_api_key.is_some()),
        (ModelMode::RemoteOpenAi, cfg.llm.openai_api_key.is_some()),
        (ModelMode::RemoteAnthropic, cfg.llm.anthropic_api_key.is_some()),
        (ModelMode::Local, true),
    ] {
        if !has_key {
            continue;
        }
        let mut c = cfg.llm.clone();
        c.mode = mode;
        c.fallback_cloud = false;
        c.fallback_to_local = false;
        let router = LlmRouter::from_config(&c, http.clone(), metrics.clone());
        let started = std::time::Instant::now();
        let mut req = LlmRequest::new(
            LlmTask::Summarize,
            "You are a test endpoint.",
            "Reply with exactly the word OK.",
        );
        req.max_tokens = 10;
        match router.complete(&req).await {
            Ok(r) => println!(
                "  ok    {}:{}  {:.1}s  -> {:?}",
                r.provider,
                r.model,
                started.elapsed().as_secs_f64(),
                r.text.trim().chars().take(40).collect::<String>()
            ),
            Err(e) => {
                failed = true;
                println!("  FAIL  {}:{}  {e}", router.provider_name(), router.model_name());
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

async fn telegram_chats(cfg: &Config) -> ExitCode {
    let Some(token) = cfg.telegram.bot_token.as_deref() else {
        eprintln!("Set TELEGRAM_BOT_TOKEN in .env first.");
        return ExitCode::from(2);
    };
    let client = emailhelper::telegram::TelegramClient::new(
        reqwest::Client::new(),
        &cfg.telegram.api_base,
        token,
        0,
    );
    // offset 0 + timeout 0: peek at pending updates without consuming them.
    match client.get_updates(0, 0).await {
        Ok(updates) => {
            let mut seen = std::collections::BTreeMap::new();
            for u in updates {
                if let Some(m) = u.message {
                    let c = m.chat;
                    let name = c
                        .title
                        .clone()
                        .or_else(|| {
                            let n = format!(
                                "{} {}",
                                c.first_name.clone().unwrap_or_default(),
                                c.last_name.clone().unwrap_or_default()
                            );
                            (!n.trim().is_empty()).then(|| n.trim().to_string())
                        })
                        .unwrap_or_else(|| "-".into());
                    let user = c
                        .username
                        .clone()
                        .map(|u| format!("@{u}"))
                        .unwrap_or_default();
                    seen.insert(c.id, format!("{name} {user}").trim().to_string());
                }
            }
            if seen.is_empty() {
                println!("No messages yet. Ask each person to open the bot in Telegram and send /start, then run this again.");
                println!("(If the agent is running with Telegram enabled it consumes messages; stop it first.)");
            } else {
                println!("chat_id\tname");
                for (id, name) in seen {
                    println!("{id}\t{name}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Telegram request failed: {e}");
            ExitCode::FAILURE
        }
    }
}
