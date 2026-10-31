//! Client for the job-links service (`job-links/`), the shared job database
//! the Chrome extension applies from. With `JOB_LINKS_ENABLED=true` the job
//! agent writes every job link found in mail there instead of applying
//! through the browser worker.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::emails::EmailRecord;
use crate::db::jobs::JobDetails;
use crate::error::{AppError, AppResult};
use crate::jobs::alerts::{self, AlertJob};
use crate::jobs::detect::{JobLink, JobSignal};
use crate::jobs::extract;

const TIMEOUT: Duration = Duration::from_secs(15);
const TOKEN_HEADER: &str = "x-joblinks-token";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkSource {
    Email,
    Research,
    #[default]
    Manual,
}

/// One job as `POST /v1/links` takes it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct QueuedLink {
    pub url: String,
    pub source: LinkSource,
    /// Email id, or the board/API name for research.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// remote | hybrid | onsite | unknown
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub employment_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub salary: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mailbox: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub posted_at: Option<DateTime<Utc>>,
}

impl QueuedLink {
    /// A job as the agents describe it (`JobDetails`).
    pub fn from_job(source: LinkSource, url: &str, platform: Option<&str>, d: &JobDetails) -> Self {
        Self {
            url: url.to_string(),
            source,
            platform: platform.map(str::to_string),
            title: Some(d.title.clone()).filter(|t| !t.trim().is_empty()),
            company: d.company.clone(),
            location: d.location.clone(),
            remote: d.remote.clone(),
            salary: d.salary.clone(),
            skills: d.skills.clone(),
            description: d.description.clone(),
            ..Default::default()
        }
    }
}

/// Counts from a batch `POST /v1/links`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct UpsertSummary {
    pub created: usize,
    pub updated: usize,
    pub failed: usize,
}

/// A claimed job (the fields an applier needs).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ClaimedLink {
    pub id: String,
    pub url: String,
    pub platform: String,
    pub title: Option<String>,
    pub company: Option<String>,
    pub location: Option<String>,
    pub remote: Option<String>,
    pub employment_type: Option<String>,
    pub description: Option<String>,
    pub skills: Vec<String>,
    pub score: Option<u8>,
    pub decision: Option<String>,
    pub person_id: Option<String>,
    pub resume_hint: Option<String>,
    pub attempts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Applied,
    Failed,
    Skipped,
}

#[derive(Clone)]
pub struct LinkQueueClient {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl LinkQueueClient {
    pub fn new(http: reqwest::Client, base: &str, token: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    pub fn from_app(app: &crate::app::App) -> Self {
        Self::new(app.http.clone(), &app.cfg.jobs.links_url, &app.cfg.jobs.links_token)
    }

    /// POST `body` to `path`; non-2xx answers become errors.
    async fn post(&self, path: &str, body: serde_json::Value) -> AppResult<reqwest::Response> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .header(TOKEN_HEADER, &self.token)
            .timeout(TIMEOUT)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let text = resp.text().await.unwrap_or_default();
        Err(AppError::other(format!(
            "job-links {path} returned {status}: {}",
            text.chars().take(300).collect::<String>()
        )))
    }

    /// Add jobs, or fill gaps in ones already stored (same URL and person).
    pub async fn upsert(&self, links: &[QueuedLink]) -> AppResult<UpsertSummary> {
        if links.is_empty() {
            return Ok(UpsertSummary::default());
        }
        Ok(self.post("/v1/links", json!({ "links": links })).await?.json().await?)
    }

    /// Claim up to `limit` jobs for `client`, highest priority first.
    pub async fn claim(&self, client: &str, limit: usize) -> AppResult<Vec<ClaimedLink>> {
        #[derive(Deserialize)]
        struct Claimed {
            links: Vec<ClaimedLink>,
        }
        let r: Claimed = self
            .post("/v1/links/claim", json!({ "client": client, "limit": limit }))
            .await?
            .json()
            .await?;
        Ok(r.links)
    }

    /// Record how applying to a claimed job went.
    pub async fn report_result(
        &self,
        id: &str,
        outcome: Outcome,
        reason: Option<&str>,
        client: &str,
    ) -> AppResult<()> {
        self.post(
            &format!("/v1/links/{id}/result"),
            json!({ "status": outcome, "reason": reason, "client": client }),
        )
        .await?;
        Ok(())
    }
}

/// Every job link in an email with what is known about the job: the alert
/// parser's entry, else the link text, else (for a single-link mail) the
/// subject.
pub fn jobs_in_email(e: &EmailRecord, signal: &JobSignal, listed: &[AlertJob]) -> Vec<(JobLink, JobDetails)> {
    let alert_for = |url: &str| listed.iter().find(|a| a.url.as_deref() == Some(url));
    let mut out: Vec<(JobLink, JobDetails)> = signal
        .links
        .iter()
        .map(|l| {
            let d = match alert_for(&l.url) {
                Some(a) => crate::jobs::details_from_alert(a),
                None => JobDetails {
                    title: l.label.clone().unwrap_or_default(),
                    ..Default::default()
                },
            };
            (l.clone(), d)
        })
        .collect();
    // Alert jobs whose link only the parser found.
    for a in listed {
        let Some(l) = a.url.as_deref().and_then(crate::jobs::detect::classify_url) else { continue };
        if !out.iter().any(|(x, _)| x.url == l.url) {
            out.push((l, crate::jobs::details_from_alert(a)));
        }
    }
    if let [(_, d)] = out.as_mut_slice() {
        if d.title.trim().is_empty() {
            let (title, company) = extract::split_title(&e.subject);
            d.title = title;
            if d.company.is_none() {
                d.company = company;
            }
        }
    }
    for (l, d) in &mut out {
        // A job site's own alert names many employers; its hint is only the first.
        if d.company.is_none() && !alerts::is_login_platform(&signal.source) {
            d.company = signal.company_hint.clone();
        }
        d.url = Some(l.url.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::detect::SignalKind;
    use wiremock::matchers::{body_json, body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn email(subject: &str) -> EmailRecord {
        EmailRecord {
            id: "email-1".into(),
            account_id: "a".into(),
            account_name: None,
            account_email: Some("me@example.com".into()),
            account_type: "gmail".into(),
            message_id: "<m>".into(),
            tb_message_id: 1,
            folder: None,
            in_reply_to: None,
            references: vec![],
            from: "x".into(),
            from_email: "x@example.com".into(),
            to: vec![],
            cc: vec![],
            subject: subject.into(),
            received_at: None,
            body_text: String::new(),
            body_html: None,
            attachments: vec![],
            category: None,
            is_job: true,
            status: "received".into(),
            created_at: "now".into(),
        }
    }

    fn signal(source: &str, links: Vec<JobLink>, company: Option<&str>) -> JobSignal {
        JobSignal {
            kind: SignalKind::JobAlert,
            source: source.into(),
            is_alert: true,
            links,
            company_hint: company.map(Into::into),
        }
    }

    fn jl(url: &str, ats: &str, label: Option<&str>) -> JobLink {
        JobLink {
            url: url.into(),
            ats: ats.into(),
            label: label.map(Into::into),
        }
    }

    const TOKEN: &str = "secret-token-0123456789";

    #[test]
    fn alert_details_are_attached() {
        let li = "https://www.linkedin.com/jobs/view/1/";
        let li2 = "https://www.linkedin.com/jobs/view/2/";
        let listed = vec![
            AlertJob {
                title: "Lead AI Engineer".into(),
                company: Some("Netrolynx AI".into()),
                location: Some("United States".into()),
                work_mode: Some("Remote".into()),
                url: Some(li.into()),
                ..Default::default()
            },
            AlertJob {
                title: "AI Engineering Lead".into(),
                company: Some("Beni Health".into()),
                url: Some(li2.into()),
                ..Default::default()
            },
        ];
        let s = signal("linkedin", vec![jl(li, "linkedin", Some("ignored label"))], Some("Netrolynx AI"));
        let out = jobs_in_email(&email("Lead AI Engineer at Netrolynx AI"), &s, &listed);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].1.title, "Lead AI Engineer");
        assert_eq!(out[0].1.company.as_deref(), Some("Netrolynx AI"));
        assert_eq!(out[0].1.remote.as_deref(), Some("remote"));
        assert_eq!(out[0].1.url.as_deref(), Some(li));
        assert_eq!(out[1].0.url, li2, "a job only the parser found is included");
        assert_eq!(out[1].1.company.as_deref(), Some("Beni Health"));
        let q = QueuedLink::from_job(LinkSource::Email, &out[0].0.url, Some("linkedin"), &out[0].1);
        assert_eq!(q.title.as_deref(), Some("Lead AI Engineer"));
        assert_eq!(q.remote.as_deref(), Some("remote"));
    }

    #[test]
    fn single_link_falls_back_to_label_then_subject() {
        let lever = "https://jobs.lever.co/globex/abc";
        let s = signal("recruiter", vec![jl(lever, "lever", None)], Some("Globex"));
        let out = jobs_in_email(&email("Staff Rust Engineer at Globex"), &s, &[]);
        assert_eq!(out[0].1.title, "Staff Rust Engineer");
        assert_eq!(out[0].1.company.as_deref(), Some("Globex"));

        let s = signal("recruiter", vec![jl(lever, "lever", Some("Platform Engineer"))], None);
        let out = jobs_in_email(&email("Opportunity"), &s, &[]);
        assert_eq!(out[0].1.title, "Platform Engineer");
    }

    #[tokio::test]
    async fn upsert_posts_a_batch_with_the_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/links"))
            .and(header("x-joblinks-token", TOKEN))
            .and(body_partial_json(json!({
                "links": [{ "url": "https://jobs.lever.co/acme/1", "source": "research", "source_detail": "remotive", "platform": "lever", "skills": ["Rust"] }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "created": 1, "updated": 0, "failed": 0, "results": []
            })))
            .expect(1)
            .mount(&server)
            .await;
        let c = LinkQueueClient::new(reqwest::Client::new(), &format!("{}/", server.uri()), TOKEN);
        let link = QueuedLink {
            source_detail: Some("remotive".into()),
            ..QueuedLink::from_job(
                LinkSource::Research,
                "https://jobs.lever.co/acme/1",
                Some("lever"),
                &JobDetails {
                    title: "Rust Engineer".into(),
                    skills: vec!["Rust".into()],
                    ..Default::default()
                },
            )
        };
        let s = c.upsert(std::slice::from_ref(&link)).await.unwrap();
        assert_eq!(s, UpsertSummary { created: 1, updated: 0, failed: 0 });
        assert_eq!(c.upsert(&[]).await.unwrap(), UpsertSummary::default(), "nothing to send");
    }

    #[tokio::test]
    async fn claim_and_report_result() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/links/claim"))
            .and(header("x-joblinks-token", TOKEN))
            .and(body_json(json!({ "client": "ext-1", "limit": 2 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "lease_expires_at": "2026-10-31T10:00:00Z",
                "links": [{ "id": "l1", "url": "https://jobs.lever.co/acme/1", "platform": "lever",
                            "status": "claimed", "score": 91, "resume_hint": "python", "attempts": 1 }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/links/l1/result"))
            .and(body_json(json!({ "status": "applied", "reason": "submitted", "client": "ext-1" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "l1" })))
            .expect(1)
            .mount(&server)
            .await;
        let c = LinkQueueClient::new(reqwest::Client::new(), &server.uri(), TOKEN);
        let got = c.claim("ext-1", 2).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].score, Some(91));
        assert_eq!(got[0].resume_hint.as_deref(), Some("python"));
        c.report_result("l1", Outcome::Applied, Some("submitted"), "ext-1").await.unwrap();
    }

    #[tokio::test]
    async fn service_errors_are_reported() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({ "error": "link is claimed by ext-2" })))
            .mount(&server)
            .await;
        let c = LinkQueueClient::new(reqwest::Client::new(), &server.uri(), TOKEN);
        let err = c.report_result("l1", Outcome::Failed, None, "ext-1").await.unwrap_err();
        assert!(err.to_string().contains("409") && err.to_string().contains("ext-2"), "{err}");
    }
}
