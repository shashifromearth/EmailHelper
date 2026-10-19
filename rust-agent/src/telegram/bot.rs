//! Interactive Telegram bot: long-polls `getUpdates`, routes replies to open
//! human-in-the-loop questions and answers a few commands.

use std::sync::Arc;
use std::time::Duration;

use crate::app::App;
use crate::db::{jobs as jdb, settings};
use crate::error::AppResult;
use crate::telegram::client::Message;
use crate::telegram::Event;

const OFFSET_KEY: &str = "telegram_update_offset";

/// Extract a leading `Q-XXXX` reference code and the remaining answer.
pub fn parse_ref(text: &str) -> (Option<String>, String) {
    let t = text.trim();
    let mut parts = t.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("");
    let up = first.trim_end_matches([':', ',']).to_ascii_uppercase();
    if up.len() == 6 && up.starts_with("Q-") && up[2..].chars().all(|c| c.is_ascii_hexdigit()) {
        return (Some(up), parts.next().unwrap_or("").trim().to_string());
    }
    (None, t.to_string())
}

/// Handle one incoming message; returns the bot's reply text.
pub async fn handle_message(app: &Arc<App>, msg: &Message) -> AppResult<Option<String>> {
    let Some(text) = msg.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    if let Some(cmd) = text.strip_prefix('/') {
        let cmd = cmd
            .split(['@', ' '])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        return Ok(Some(command(app, &cmd).await?));
    }
    let (code, answer) = parse_ref(text);
    let reply_to = msg.reply_to_message.as_ref().map(|m| m.message_id);
    let q = {
        let code = code.clone();
        app.db
            .call(move |c| jdb::find_open_question(c, reply_to, code.as_deref()))
            .await?
    };
    let q = match q {
        Some(q) => q,
        None => {
            // A single open question means an un-threaded reply is unambiguous.
            let open = app.db.call(|c| jdb::open_questions(c, None)).await?;
            if open.len() == 1 && code.is_none() {
                open.into_iter().next().unwrap()
            } else if open.is_empty() {
                return Ok(Some("No open questions right now. Try /status.".into()));
            } else {
                return Ok(Some(format!(
                    "I have {} open questions. Reply directly to the question message, or start with its Ref (e.g. \"{} your answer\"). /pending lists them.",
                    open.len(),
                    open[0].ref_code
                )));
            }
        }
    };
    let answer = if code.is_some() {
        answer
    } else {
        text.to_string()
    };
    let reply = crate::jobs::application::handle_human_answer(app, &q, &answer).await?;
    Ok(Some(reply))
}

async fn command(app: &Arc<App>, cmd: &str) -> AppResult<String> {
    Ok(match cmd {
        "start" | "help" => "EmailHelper bot\n/status – system status\n/jobs – top open jobs\n/pending – questions waiting for you\nReply to a question message to answer it; reply \"skip\" to abandon an application.".into(),
        "status" => {
            let (drafts, pipeline, depth) = app
                .db
                .call(|c| {
                    Ok((
                        crate::db::drafts::counts(c)?,
                        jdb::pipeline_counts(c)?,
                        crate::db::queue::depth(c)?,
                    ))
                })
                .await?;
            format!(
                "Status: OK\nLLM: {} ({})\nQueue: {depth}\nDrafts: {} total, {} pending, {} need review\nApplications: {} applied, {} in progress, {} blocked, {} awaiting approval",
                app.llm.provider_name(),
                app.llm.model_name(),
                drafts.total,
                drafts.pending,
                drafts.review_required,
                pipeline.applied,
                pipeline.in_progress,
                pipeline.blocked,
                pipeline.awaiting_approval
            )
        }
        "jobs" => {
            let jobs = app.db.call(|c| jdb::list_jobs(c, 10, None)).await?;
            if jobs.is_empty() {
                "No jobs tracked yet.".into()
            } else {
                jobs.iter()
                    .enumerate()
                    .map(|(i, j)| format!("{}. [{}] {} — {} ({})", i + 1, j.score.unwrap_or(0), j.details.title, j.details.company.as_deref().unwrap_or("-"), j.status))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        "pending" => {
            let open = app.db.call(|c| jdb::open_questions(c, None)).await?;
            if open.is_empty() {
                "No open questions.".into()
            } else {
                open.iter()
                    .map(|q| format!("{} – {} ({})", q.ref_code, q.question, q.reason))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        _ => "Unknown command. /help".into(),
    })
}

/// Long-polling loop. Backs off exponentially on errors; `getUpdates` with a
/// server-side timeout is event-driven, not a busy poll.
pub fn spawn(app: Arc<App>) -> Option<tokio::task::JoinHandle<()>> {
    let client = app.notifier.client()?.clone();
    Some(tokio::spawn(async move {
        let mut offset: i64 = app
            .db
            .call(|c| settings::get(c, OFFSET_KEY))
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut backoff = Duration::from_secs(1);
        let timeout = app.cfg.telegram.poll_timeout_secs;
        tracing::info!("telegram bot listening");
        loop {
            match client.get_updates(offset, timeout).await {
                Ok(updates) => {
                    backoff = Duration::from_secs(1);
                    for u in updates {
                        offset = offset.max(u.update_id + 1);
                        let Some(msg) = u.message else { continue };
                        // One bot serves everyone: the default chat plus any
                        // person's own chat from people.json.
                        let allowed = msg.chat.id == client.chat_id
                            || app
                                .knowledge
                                .people()
                                .persons
                                .iter()
                                .any(|p| p.telegram_chat_id == Some(msg.chat.id));
                        if !allowed {
                            tracing::warn!(
                                chat_id = msg.chat.id,
                                "ignoring message from unauthorised chat"
                            );
                            continue;
                        }
                        app.metrics
                            .telegram_requests_total
                            .with_label_values(&["in"])
                            .inc();
                        match handle_message(&app, &msg).await {
                            Ok(Some(reply)) => {
                                app.notifier
                                    .send_in(
                                        Some(msg.chat.id),
                                        Event::Reply,
                                        &reply,
                                        Some(msg.message_id),
                                    )
                                    .await;
                            }
                            Ok(None) => {}
                            Err(e) => {
                                tracing::error!(error = %e, "failed to handle telegram message");
                                app.notifier
                                    .send_in(
                                        Some(msg.chat.id),
                                        Event::Error,
                                        "Sorry, that failed. Check the agent logs.",
                                        Some(msg.message_id),
                                    )
                                    .await;
                            }
                        }
                    }
                    let o = offset.to_string();
                    let _ = app.db.call(move |c| settings::set(c, OFFSET_KEY, &o)).await;
                }
                Err(e) => {
                    app.metrics.failure("telegram");
                    tracing::warn!(error = %e, retry_in_secs = backoff.as_secs(), "telegram getUpdates failed");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(300));
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reference_codes() {
        assert_eq!(
            parse_ref("Q-7F3A 35 LPA"),
            (Some("Q-7F3A".into()), "35 LPA".into())
        );
        assert_eq!(
            parse_ref("q-7f3a: yes"),
            (Some("Q-7F3A".into()), "yes".into())
        );
        assert_eq!(parse_ref("35 LPA"), (None, "35 LPA".into()));
        assert_eq!(parse_ref("Q-ZZZZ hi"), (None, "Q-ZZZZ hi".into()));
    }
}
