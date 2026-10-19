//! Daily Telegram digest of good matches the agent can't apply to itself:
//! jobs that are only on LinkedIn/Naukri or on an employer portal that needs
//! an account. Each person gets their own list with direct links.

use std::sync::Arc;

use chrono::Local;
use rusqlite::{params, Connection};

use crate::app::App;
use crate::error::AppResult;
use crate::telegram::Event;

/// Digest entries older than this are dropped unsent.
const EXPIRE_DAYS: i64 = 7;

#[derive(Debug, Clone, PartialEq)]
pub struct DigestJob {
    pub id: String,
    pub person_id: Option<String>,
    pub title: String,
    pub company: Option<String>,
    pub location: Option<String>,
    pub url: Option<String>,
    pub source: String,
    pub score: i64,
}

/// Unsent digest jobs, best first.
pub fn pending(c: &Connection) -> AppResult<Vec<DigestJob>> {
    let mut stmt = c.prepare(
        "SELECT id, person_id, title, company, location, url, source, COALESCE(score, 0) FROM jobs
         WHERE status='digest' AND digest_sent_at IS NULL AND COALESCE(decision,'') != 'skip'
         ORDER BY score DESC, discovered_at DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(DigestJob {
                id: r.get(0)?,
                person_id: r.get(1)?,
                title: r.get(2)?,
                company: r.get(3)?,
                location: r.get(4)?,
                url: r.get(5)?,
                source: r.get(6)?,
                score: r.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn mark_sent(c: &Connection, ids: &[String]) -> AppResult<()> {
    let ts = crate::db::now();
    for id in ids {
        c.execute("UPDATE jobs SET digest_sent_at=?2 WHERE id=?1", params![id, ts])?;
    }
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(EXPIRE_DAYS))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    c.execute(
        "UPDATE jobs SET digest_sent_at='expired' WHERE status='digest' AND digest_sent_at IS NULL AND discovered_at < ?1",
        params![cutoff],
    )?;
    Ok(())
}

/// The digest message for one person's jobs.
pub fn format(jobs: &[DigestJob]) -> String {
    let mut s = format!(
        "📋 Jobs to apply to yourself ({})\nThese good matches are only on LinkedIn/Naukri or an employer portal that needs an account, so the agent can't apply for you.\n",
        jobs.len()
    );
    for (i, j) in jobs.iter().enumerate() {
        let place = j.location.as_deref().map(|l| format!(" · {l}")).unwrap_or_default();
        s.push_str(&format!(
            "\n{}. {} — {}{} · {}/100\n   {}\n",
            i + 1,
            j.title,
            j.company.as_deref().unwrap_or("company not named"),
            place,
            j.score,
            j.url.clone().unwrap_or_else(|| format!("(in your {} alert email)", j.source)),
        ));
    }
    s
}

/// Send each person their digest; returns the number of jobs sent.
pub async fn send(app: &Arc<App>) -> AppResult<usize> {
    let all = app.db.call(|c| pending(c)).await?;
    let max = app.cfg.jobs.digest_max.max(1);
    let mut by_person: std::collections::BTreeMap<Option<String>, Vec<DigestJob>> = Default::default();
    for j in all {
        by_person.entry(j.person_id.clone()).or_default().push(j);
    }
    let mut sent = 0usize;
    for (pid, mut jobs) in by_person {
        jobs.truncate(max);
        let person = crate::jobs::person_by_id(app, pid.as_deref());
        let chat = person.as_ref().and_then(|p| p.telegram_chat_id);
        let text = match &person {
            Some(p) => format!("[{}]\n{}", p.name, format(&jobs)),
            None => format(&jobs),
        };
        app.notifier.notify_person(chat, Event::JobsDiscovered, &text).await;
        let ids: Vec<String> = jobs.iter().map(|j| j.id.clone()).collect();
        sent += ids.len();
        app.db.call(move |c| mark_sent(c, &ids)).await?;
    }
    tracing::info!(sent, "job digest sent");
    Ok(sent)
}

pub fn spawn_scheduler(app: Arc<App>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let wait = crate::research::discovery::until_next(app.cfg.jobs.digest_time, Local::now());
            tokio::time::sleep(wait).await;
            if let Err(e) = send(&app).await {
                tracing::error!(error = %e, "job digest failed");
            }
            tokio::time::sleep(std::time::Duration::from_secs(61)).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_lists_links_and_placeholders() {
        let jobs = vec![
            DigestJob {
                id: "1".into(),
                person_id: None,
                title: "Lead AI Engineer".into(),
                company: Some("Netrolynx AI".into()),
                location: Some("Bengaluru".into()),
                url: Some("https://www.linkedin.com/jobs/view/4480916162/".into()),
                source: "linkedin".into(),
                score: 74,
            },
            DigestJob {
                id: "2".into(),
                person_id: None,
                title: "Principal Platform Engineer".into(),
                company: None,
                location: None,
                url: None,
                source: "naukri".into(),
                score: 66,
            },
        ];
        let t = format(&jobs);
        assert!(t.contains("1. Lead AI Engineer — Netrolynx AI · Bengaluru · 74/100\n   https://www.linkedin.com/jobs/view/4480916162/"));
        assert!(t.contains("2. Principal Platform Engineer — company not named · 66/100\n   (in your naukri alert email)"));
    }
}
