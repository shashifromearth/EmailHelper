//! Jobs, applications, human-in-the-loop questions, recruiters and research findings.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{from_json, new_id, now, to_json};
use crate::error::{AppError, AppResult};

// ---------------------------------------------------------------- jobs

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct JobDetails {
    pub title: String,
    pub company: Option<String>,
    pub url: Option<String>,
    pub location: Option<String>,
    /// remote | hybrid | onsite | unknown
    pub remote: Option<String>,
    pub salary: Option<String>,
    pub salary_min: Option<f64>,
    pub salary_max: Option<f64>,
    pub salary_currency: Option<String>,
    pub experience: Option<String>,
    pub experience_years: Option<f64>,
    pub skills: Vec<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobRecord {
    pub id: String,
    pub source: String,
    pub details: JobDetails,
    pub score: Option<i64>,
    pub score_breakdown: Value,
    pub decision: Option<String>,
    pub decision_reasons: Vec<String>,
    pub status: String,
    pub discovered_at: String,
}

/// Stable identity for a job: normalised URL, else company + title.
pub fn fingerprint(d: &JobDetails) -> String {
    let basis = match &d.url {
        Some(u) if !u.is_empty() => {
            let u = u.split(['?', '#']).next().unwrap_or(u);
            u.trim_end_matches('/').to_ascii_lowercase()
        }
        _ => format!(
            "{}|{}",
            d.company.as_deref().unwrap_or("").to_lowercase().trim(),
            d.title.to_lowercase().trim()
        ),
    };
    let mut h = Sha256::new();
    h.update(basis.as_bytes());
    format!("{:x}", h.finalize())[..32].to_string()
}

pub struct JobScore<'a> {
    pub score: i64,
    pub breakdown: &'a Value,
    pub decision: &'a str,
    pub reasons: &'a [String],
}

/// Insert or refresh a job. Returns `(id, is_new)`.
pub fn upsert(
    c: &Connection,
    source: &str,
    source_ref: Option<&str>,
    d: &JobDetails,
    s: &JobScore<'_>,
) -> AppResult<(String, bool)> {
    let fp = fingerprint(d);
    let ts = now();
    if let Some(id) = c
        .query_row("SELECT id FROM jobs WHERE fingerprint=?1", [&fp], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
    {
        c.execute(
            "UPDATE jobs SET score=?2, score_breakdown=?3, decision=?4, decision_reasons=?5,
                description=COALESCE(?6, description), updated_at=?7 WHERE id=?1",
            params![
                id,
                s.score,
                s.breakdown.to_string(),
                s.decision,
                to_json(&s.reasons),
                d.description,
                ts
            ],
        )?;
        return Ok((id, false));
    }
    let id = new_id();
    c.execute(
        "INSERT INTO jobs (id, fingerprint, source, source_ref, company, title, url, location, remote,
            salary, salary_min, salary_max, salary_currency, experience, skills, description, score,
            score_breakdown, decision, decision_reasons, status, discovered_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,'new',?21,?21)",
        params![
            id,
            fp,
            source,
            source_ref,
            d.company,
            d.title,
            d.url,
            d.location,
            d.remote,
            d.salary,
            d.salary_min,
            d.salary_max,
            d.salary_currency,
            d.experience,
            to_json(&d.skills),
            d.description,
            s.score,
            s.breakdown.to_string(),
            s.decision,
            to_json(&s.reasons),
            ts
        ],
    )?;
    Ok((id, true))
}

const JOB_COLS: &str = "id, source, company, title, url, location, remote, salary, salary_min,
    salary_max, salary_currency, experience, skills, description, score, score_breakdown, decision,
    decision_reasons, status, discovered_at";

fn map_job(r: &Row<'_>) -> rusqlite::Result<JobRecord> {
    let breakdown: Option<String> = r.get(15)?;
    Ok(JobRecord {
        id: r.get(0)?,
        source: r.get(1)?,
        details: JobDetails {
            company: r.get(2)?,
            title: r.get(3)?,
            url: r.get(4)?,
            location: r.get(5)?,
            remote: r.get(6)?,
            salary: r.get(7)?,
            salary_min: r.get(8)?,
            salary_max: r.get(9)?,
            salary_currency: r.get(10)?,
            experience: r.get(11)?,
            experience_years: None,
            skills: from_json(r.get(12)?),
            description: r.get(13)?,
        },
        score: r.get(14)?,
        score_breakdown: breakdown
            .and_then(|b| serde_json::from_str(&b).ok())
            .unwrap_or(Value::Null),
        decision: r.get(16)?,
        decision_reasons: from_json(r.get(17)?),
        status: r.get(18)?,
        discovered_at: r.get(19)?,
    })
}

pub fn get_job(c: &Connection, id: &str) -> AppResult<JobRecord> {
    c.query_row(
        &format!("SELECT {JOB_COLS} FROM jobs WHERE id=?1"),
        [id],
        map_job,
    )
    .optional()?
    .ok_or_else(|| AppError::NotFound(format!("job {id}")))
}

pub fn list_jobs(c: &Connection, limit: usize, since: Option<&str>) -> AppResult<Vec<JobRecord>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {JOB_COLS} FROM jobs WHERE (?1 IS NULL OR discovered_at >= ?1)
         ORDER BY score DESC, discovered_at DESC LIMIT ?2"
    ))?;
    let rows = stmt
        .query_map(params![since, limit as i64], map_job)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn set_job_status(c: &Connection, id: &str, status: &str) -> AppResult<()> {
    c.execute(
        "UPDATE jobs SET status=?2, updated_at=?3 WHERE id=?1",
        params![id, status, now()],
    )?;
    Ok(())
}

// ---------------------------------------------------------------- applications

#[derive(Debug, Clone, Serialize)]
pub struct ApplicationRecord {
    pub id: String,
    pub job_id: String,
    pub company: Option<String>,
    pub role: Option<String>,
    pub url: String,
    pub status: String,
    pub current_step: Option<String>,
    pub page_url: Option<String>,
    pub ats: Option<String>,
    pub fields: Value,
    pub answers: serde_json::Map<String, Value>,
    pub resume_uploaded: bool,
    pub cover_letter: Option<String>,
    pub approved: bool,
    pub blocker_kind: Option<String>,
    pub blocker_detail: Option<String>,
    pub attempts: i64,
    pub run_seq: i64,
    pub score: Option<i64>,
    pub confirmation_text: Option<String>,
    pub screenshot_path: Option<String>,
    pub applied_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

const APP_COLS: &str =
    "id, job_id, company, role, url, status, current_step, page_url, ats, fields,
    answers, resume_uploaded, cover_letter, approved, blocker_kind, blocker_detail, attempts,
    run_seq, score, confirmation_text, screenshot_path, applied_at, created_at, updated_at";

fn map_app(r: &Row<'_>) -> rusqlite::Result<ApplicationRecord> {
    let fields: String = r.get(9)?;
    let answers: String = r.get(10)?;
    Ok(ApplicationRecord {
        id: r.get(0)?,
        job_id: r.get(1)?,
        company: r.get(2)?,
        role: r.get(3)?,
        url: r.get(4)?,
        status: r.get(5)?,
        current_step: r.get(6)?,
        page_url: r.get(7)?,
        ats: r.get(8)?,
        fields: serde_json::from_str(&fields).unwrap_or(Value::Array(vec![])),
        answers: serde_json::from_str(&answers).unwrap_or_default(),
        resume_uploaded: r.get::<_, i64>(11)? != 0,
        cover_letter: r.get(12)?,
        approved: r.get::<_, i64>(13)? != 0,
        blocker_kind: r.get(14)?,
        blocker_detail: r.get(15)?,
        attempts: r.get(16)?,
        run_seq: r.get(17)?,
        score: r.get(18)?,
        confirmation_text: r.get(19)?,
        screenshot_path: r.get(20)?,
        applied_at: r.get(21)?,
        created_at: r.get(22)?,
        updated_at: r.get(23)?,
    })
}

/// Create an application for a job (one per job). Returns `(id, created)`.
pub fn create_application(c: &Connection, job: &JobRecord, url: &str) -> AppResult<(String, bool)> {
    if let Some(id) = c
        .query_row(
            "SELECT id FROM applications WHERE job_id=?1",
            [&job.id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok((id, false));
    }
    let id = new_id();
    let ts = now();
    c.execute(
        "INSERT INTO applications (id, job_id, company, role, url, status, current_step, score,
            created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,'pending','created',?6,?7,?7)",
        params![
            id,
            job.id,
            job.details.company,
            job.details.title,
            url,
            job.score,
            ts
        ],
    )?;
    record_status(c, &id, "pending", Some("created"))?;
    Ok((id, true))
}

pub fn get_application(c: &Connection, id: &str) -> AppResult<ApplicationRecord> {
    c.query_row(
        &format!("SELECT {APP_COLS} FROM applications WHERE id=?1"),
        [id],
        map_app,
    )
    .optional()?
    .ok_or_else(|| AppError::NotFound(format!("application {id}")))
}

pub fn list_applications(
    c: &Connection,
    status: Option<&str>,
    limit: usize,
) -> AppResult<Vec<ApplicationRecord>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {APP_COLS} FROM applications WHERE (?1 IS NULL OR status=?1)
         ORDER BY updated_at DESC LIMIT ?2"
    ))?;
    let rows = stmt
        .query_map(params![status, limit as i64], map_app)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn record_status(
    c: &Connection,
    app_id: &str,
    status: &str,
    detail: Option<&str>,
) -> AppResult<()> {
    c.execute(
        "INSERT INTO application_status(application_id, status, detail, ts) VALUES (?1,?2,?3,?4)",
        params![app_id, status, detail, now()],
    )?;
    Ok(())
}

/// Partial update of an application. `None` leaves a column unchanged.
#[derive(Debug, Default)]
pub struct AppUpdate<'a> {
    pub status: Option<&'a str>,
    pub current_step: Option<&'a str>,
    pub page_url: Option<&'a str>,
    pub ats: Option<&'a str>,
    pub fields: Option<&'a Value>,
    pub answers: Option<&'a serde_json::Map<String, Value>>,
    pub resume_uploaded: Option<bool>,
    pub cover_letter: Option<&'a str>,
    pub approved: Option<bool>,
    /// `Some(None)` clears the blocker.
    pub blocker: Option<Option<(&'a str, &'a str)>>,
    pub confirmation_text: Option<&'a str>,
    pub screenshot_path: Option<&'a str>,
    pub applied_at: Option<&'a str>,
    pub bump_attempts: bool,
}

pub fn update_application(c: &Connection, id: &str, u: &AppUpdate<'_>) -> AppResult<()> {
    let (bk, bd, clear_blocker) = match u.blocker {
        Some(Some((k, d))) => (Some(k), Some(d), false),
        Some(None) => (None, None, true),
        None => (None, None, false),
    };
    c.execute(
        "UPDATE applications SET
            status = COALESCE(?2, status),
            current_step = COALESCE(?3, current_step),
            page_url = COALESCE(?4, page_url),
            ats = COALESCE(?5, ats),
            fields = COALESCE(?6, fields),
            answers = COALESCE(?7, answers),
            resume_uploaded = COALESCE(?8, resume_uploaded),
            cover_letter = COALESCE(?9, cover_letter),
            approved = COALESCE(?10, approved),
            blocker_kind = CASE WHEN ?13 THEN NULL ELSE COALESCE(?11, blocker_kind) END,
            blocker_detail = CASE WHEN ?13 THEN NULL ELSE COALESCE(?12, blocker_detail) END,
            confirmation_text = COALESCE(?14, confirmation_text),
            screenshot_path = COALESCE(?15, screenshot_path),
            applied_at = COALESCE(?16, applied_at),
            attempts = attempts + ?17,
            updated_at = ?18
         WHERE id = ?1",
        params![
            id,
            u.status,
            u.current_step,
            u.page_url,
            u.ats,
            u.fields.map(|f| f.to_string()),
            u.answers.map(to_json),
            u.resume_uploaded.map(|b| b as i64),
            u.cover_letter,
            u.approved.map(|b| b as i64),
            bk,
            bd,
            clear_blocker,
            u.confirmation_text,
            u.screenshot_path,
            u.applied_at,
            u.bump_attempts as i64,
            now(),
        ],
    )?;
    if let Some(status) = u.status {
        record_status(c, id, status, u.current_step)?;
    }
    Ok(())
}

/// Increment and return the run sequence (used to build unique queue keys).
pub fn next_run_seq(c: &Connection, id: &str) -> AppResult<i64> {
    c.execute(
        "UPDATE applications SET run_seq = run_seq + 1 WHERE id=?1",
        [id],
    )?;
    Ok(
        c.query_row("SELECT run_seq FROM applications WHERE id=?1", [id], |r| {
            r.get(0)
        })?,
    )
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct PipelineCounts {
    pub applied: i64,
    pub in_progress: i64,
    pub blocked: i64,
    pub failed: i64,
    pub awaiting_approval: i64,
    pub skipped: i64,
}

pub fn pipeline_counts(c: &Connection) -> AppResult<PipelineCounts> {
    Ok(c.query_row(
        "SELECT COALESCE(SUM(status='submitted'),0),
                COALESCE(SUM(status IN ('pending','running','ready')),0),
                COALESCE(SUM(status IN ('awaiting_human','blocked')),0),
                COALESCE(SUM(status='failed'),0),
                COALESCE(SUM(status='awaiting_approval'),0),
                COALESCE(SUM(status='skipped'),0)
         FROM applications",
        [],
        |r| {
            Ok(PipelineCounts {
                applied: r.get(0)?,
                in_progress: r.get(1)?,
                blocked: r.get(2)?,
                failed: r.get(3)?,
                awaiting_approval: r.get(4)?,
                skipped: r.get(5)?,
            })
        },
    )?)
}

// ---------------------------------------------------------------- questions

#[derive(Debug, Clone, Serialize)]
pub struct HumanQuestion {
    pub id: String,
    pub ref_code: String,
    pub application_id: Option<String>,
    pub field_id: Option<String>,
    pub question: String,
    pub reason: String,
    pub options: Vec<String>,
    pub status: String,
    pub telegram_message_id: Option<i64>,
    pub answer: Option<String>,
    pub created_at: String,
}

const Q_COLS: &str = "id, ref_code, application_id, field_id, question, reason, options, status,
    telegram_message_id, answer, created_at";

fn map_q(r: &Row<'_>) -> rusqlite::Result<HumanQuestion> {
    Ok(HumanQuestion {
        id: r.get(0)?,
        ref_code: r.get(1)?,
        application_id: r.get(2)?,
        field_id: r.get(3)?,
        question: r.get(4)?,
        reason: r.get(5)?,
        options: from_json(r.get(6)?),
        status: r.get(7)?,
        telegram_message_id: r.get(8)?,
        answer: r.get(9)?,
        created_at: r.get(10)?,
    })
}

/// Create an open question unless one is already open for the same field.
pub fn create_question(
    c: &Connection,
    application_id: &str,
    field_id: &str,
    question: &str,
    reason: &str,
    options: &[String],
) -> AppResult<(HumanQuestion, bool)> {
    if let Some(q) = c
        .query_row(
            &format!(
                "SELECT {Q_COLS} FROM human_questions
                 WHERE application_id=?1 AND field_id=?2 AND status='open'"
            ),
            params![application_id, field_id],
            map_q,
        )
        .optional()?
    {
        return Ok((q, false));
    }
    let id = new_id();
    let ref_code = format!("Q-{}", id[..4].to_ascii_uppercase());
    c.execute(
        "INSERT INTO human_questions (id, ref_code, application_id, field_id, question, reason,
            options, status, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,'open',?8)",
        params![
            id,
            ref_code,
            application_id,
            field_id,
            question,
            reason,
            to_json(&options),
            now()
        ],
    )?;
    Ok((get_question(c, &id)?, true))
}

pub fn get_question(c: &Connection, id: &str) -> AppResult<HumanQuestion> {
    c.query_row(
        &format!("SELECT {Q_COLS} FROM human_questions WHERE id=?1"),
        [id],
        map_q,
    )
    .optional()?
    .ok_or_else(|| AppError::NotFound(format!("question {id}")))
}

pub fn set_question_telegram_id(c: &Connection, id: &str, msg_id: i64) -> AppResult<()> {
    c.execute(
        "UPDATE human_questions SET telegram_message_id=?2 WHERE id=?1",
        params![id, msg_id],
    )?;
    Ok(())
}

/// Find the open question a Telegram reply refers to.
pub fn find_open_question(
    c: &Connection,
    telegram_reply_to: Option<i64>,
    ref_code: Option<&str>,
) -> AppResult<Option<HumanQuestion>> {
    if let Some(mid) = telegram_reply_to {
        if let Some(q) = c
            .query_row(
                &format!(
                    "SELECT {Q_COLS} FROM human_questions
                     WHERE telegram_message_id=?1 AND status='open'"
                ),
                [mid],
                map_q,
            )
            .optional()?
        {
            return Ok(Some(q));
        }
    }
    if let Some(code) = ref_code {
        return Ok(c
            .query_row(
                &format!(
                    "SELECT {Q_COLS} FROM human_questions
                     WHERE ref_code=?1 COLLATE NOCASE AND status='open'"
                ),
                [code],
                map_q,
            )
            .optional()?);
    }
    Ok(None)
}

pub fn answer_question(c: &Connection, id: &str, answer: &str) -> AppResult<bool> {
    let n = c.execute(
        "UPDATE human_questions SET status='answered', answer=?2, answered_at=?3
         WHERE id=?1 AND status='open'",
        params![id, answer, now()],
    )?;
    Ok(n == 1)
}

pub fn cancel_questions(c: &Connection, application_id: &str) -> AppResult<()> {
    c.execute(
        "UPDATE human_questions SET status='cancelled' WHERE application_id=?1 AND status='open'",
        [application_id],
    )?;
    Ok(())
}

pub fn open_questions(
    c: &Connection,
    application_id: Option<&str>,
) -> AppResult<Vec<HumanQuestion>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {Q_COLS} FROM human_questions
         WHERE status='open' AND (?1 IS NULL OR application_id=?1) ORDER BY created_at"
    ))?;
    let rows = stmt
        .query_map(params![application_id], map_q)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------- answer memory

pub fn question_key(question: &str) -> String {
    question
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn remember_answer(c: &Connection, question: &str, answer: &str) -> AppResult<()> {
    c.execute(
        "INSERT INTO answer_memory(question_key, question, answer, updated_at) VALUES (?1,?2,?3,?4)
         ON CONFLICT(question_key) DO UPDATE SET answer=excluded.answer, updated_at=excluded.updated_at",
        params![question_key(question), question, answer, now()],
    )?;
    Ok(())
}

pub fn recall_answer(c: &Connection, question: &str) -> AppResult<Option<String>> {
    Ok(c.query_row(
        "SELECT answer FROM answer_memory WHERE question_key=?1",
        [question_key(question)],
        |r| r.get(0),
    )
    .optional()?)
}

// ---------------------------------------------------------------- recruiters

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Recruiter {
    #[serde(default)]
    pub id: String,
    pub company: String,
    pub name: String,
    pub title: Option<String>,
    pub linkedin_url: Option<String>,
    pub email: Option<String>,
    pub source: String,
    pub rank_score: i64,
    pub outreach_draft: Option<String>,
    #[serde(default)]
    pub status: String,
}

/// Insert or update a recruiter. Returns `(id, is_new)`.
pub fn upsert_recruiter(c: &Connection, r: &Recruiter) -> AppResult<(String, bool)> {
    let ts = now();
    if let Some(id) = c
        .query_row(
            "SELECT id FROM recruiters WHERE company=?1 COLLATE NOCASE AND name=?2 COLLATE NOCASE",
            params![r.company, r.name],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        c.execute(
            "UPDATE recruiters SET title=COALESCE(?2,title), linkedin_url=COALESCE(?3,linkedin_url),
                email=COALESCE(?4,email), rank_score=MAX(rank_score, ?5),
                outreach_draft=COALESCE(?6,outreach_draft), updated_at=?7 WHERE id=?1",
            params![
                id,
                r.title,
                r.linkedin_url,
                r.email,
                r.rank_score,
                r.outreach_draft,
                ts
            ],
        )?;
        return Ok((id, false));
    }
    let id = new_id();
    c.execute(
        "INSERT INTO recruiters (id, company, name, title, linkedin_url, email, source, rank_score,
            outreach_draft, status, created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'new',?10,?10)",
        params![
            id,
            r.company,
            r.name,
            r.title,
            r.linkedin_url,
            r.email,
            r.source,
            r.rank_score,
            r.outreach_draft,
            ts
        ],
    )?;
    Ok((id, true))
}

pub fn list_recruiters(
    c: &Connection,
    company: Option<&str>,
    limit: usize,
) -> AppResult<Vec<Recruiter>> {
    let mut stmt = c.prepare(
        "SELECT id, company, name, title, linkedin_url, email, source, rank_score, outreach_draft, status
         FROM recruiters WHERE (?1 IS NULL OR company=?1 COLLATE NOCASE)
         ORDER BY rank_score DESC, created_at DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![company, limit as i64], |r| {
            Ok(Recruiter {
                id: r.get(0)?,
                company: r.get(1)?,
                name: r.get(2)?,
                title: r.get(3)?,
                linkedin_url: r.get(4)?,
                email: r.get(5)?,
                source: r.get(6)?,
                rank_score: r.get(7)?,
                outreach_draft: r.get(8)?,
                status: r.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------- interviews & findings

pub fn record_interview(
    c: &Connection,
    email_id: &str,
    company: Option<&str>,
    role: Option<&str>,
    notes: &str,
) -> AppResult<String> {
    let id = new_id();
    c.execute(
        "INSERT INTO interviews (id, email_id, company, role, notes, created_at) VALUES (?1,?2,?3,?4,?5,?6)",
        params![id, email_id, company, role, notes, now()],
    )?;
    Ok(id)
}

pub fn interview_count(c: &Connection) -> AppResult<i64> {
    Ok(c.query_row("SELECT COUNT(*) FROM interviews", [], |r| r.get(0))?)
}

pub fn save_finding(
    c: &Connection,
    query: &str,
    url: &str,
    title: &str,
    snippet: &str,
    source: &str,
) -> AppResult<bool> {
    let n = c.execute(
        "INSERT INTO research_findings (query, url, title, snippet, source, created_at)
         VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(url) DO NOTHING",
        params![query, url, title, snippet, source, now()],
    )?;
    Ok(n == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_ignores_tracking() {
        let a = JobDetails {
            title: "X".into(),
            url: Some("https://jobs.lever.co/acme/123?utm_source=li".into()),
            ..Default::default()
        };
        let b = JobDetails {
            title: "Y".into(),
            url: Some("https://jobs.lever.co/acme/123/".into()),
            ..Default::default()
        };
        assert_eq!(fingerprint(&a), fingerprint(&b));
    }

    #[test]
    fn question_keys_normalise() {
        assert_eq!(
            question_key("  What is your Expected CTC? * "),
            question_key("what is your expected ctc")
        );
    }

    #[tokio::test]
    async fn question_lifecycle() {
        let db = crate::db::Db::open_in_memory().unwrap();
        db.call(|c| {
            let d = JobDetails {
                title: "Staff AI Engineer".into(),
                company: Some("Acme".into()),
                url: Some("https://x/1".into()),
                ..Default::default()
            };
            let (jid, new) = upsert(
                c,
                "test",
                None,
                &d,
                &JobScore {
                    score: 90,
                    breakdown: &Value::Null,
                    decision: "apply",
                    reasons: &[],
                },
            )?;
            assert!(new);
            let job = get_job(c, &jid)?;
            let (aid, _) = create_application(c, &job, "https://x/1")?;
            let (q, created) =
                create_question(c, &aid, "ctc", "Expected CTC?", "unknown_question", &[])?;
            assert!(created);
            let (_, created_again) =
                create_question(c, &aid, "ctc", "Expected CTC?", "unknown_question", &[])?;
            assert!(!created_again);
            set_question_telegram_id(c, &q.id, 777)?;
            let found = find_open_question(c, Some(777), None)?.unwrap();
            assert_eq!(found.id, q.id);
            let by_code = find_open_question(c, None, Some(&q.ref_code.to_lowercase()))?.unwrap();
            assert_eq!(by_code.id, q.id);
            assert!(answer_question(c, &q.id, "35 LPA")?);
            assert!(!answer_question(c, &q.id, "again")?);
            assert!(open_questions(c, Some(&aid))?.is_empty());
            Ok(())
        })
        .await
        .unwrap();
    }
}
