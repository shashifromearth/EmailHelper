//! Client for the job-links service (`job-links/`). With
//! `JOB_LINKS_ENABLED=true` every job link found in mail is queued there for
//! the Chrome extension, instead of the browser worker applying to it.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::db::emails::EmailRecord;
use crate::error::{AppError, AppResult};
use crate::jobs::alerts::AlertJob;
use crate::jobs::detect::{JobLink, JobSignal};
use crate::jobs::{alerts, extract};

const TIMEOUT: Duration = Duration::from_secs(15);
const TOKEN_HEADER: &str = "x-joblinks-token";

/// One link as `POST /v1/links` takes it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct QueuedLink {
    pub url: String,
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mailbox: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email_id: Option<String>,
}

/// Counts from a batch `POST /v1/links`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct QueueSummary {
    pub created: usize,
    pub updated: usize,
    pub failed: usize,
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

    /// Add (or merge into) links on the service.
    pub async fn push(&self, links: &[QueuedLink]) -> AppResult<QueueSummary> {
        if links.is_empty() {
            return Ok(QueueSummary::default());
        }
        let resp = self
            .http
            .post(format!("{}/v1/links", self.base))
            .header(TOKEN_HEADER, &self.token)
            .timeout(TIMEOUT)
            .json(&serde_json::json!({ "links": links }))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::other(format!(
                "job-links service returned {status}: {}",
                body.chars().take(300).collect::<String>()
            )));
        }
        Ok(resp.json().await?)
    }
}

fn person_link(link: &JobLink, a: Option<&AlertJob>) -> QueuedLink {
    QueuedLink {
        url: link.url.clone(),
        source: "email",
        platform: Some(link.ats.clone()),
        title: a.map(|a| a.title.clone()).or_else(|| link.label.clone()),
        company: a.and_then(|a| a.company.clone()),
        location: a.and_then(|a| a.location.clone()),
        ..Default::default()
    }
}

/// Every job link in an email, with what the alert parser (or the link text,
/// or the subject for a single-link mail) says about the job.
pub fn links_for_email(
    e: &EmailRecord,
    signal: &JobSignal,
    listed: &[AlertJob],
    person_id: Option<&str>,
) -> Vec<QueuedLink> {
    let alert_for = |url: &str| listed.iter().find(|a| a.url.as_deref() == Some(url));
    let mut out: Vec<QueuedLink> = signal
        .links
        .iter()
        .map(|l| person_link(l, alert_for(&l.url)))
        .collect();
    // Alert jobs whose link only the parser found.
    for a in listed {
        let Some(url) = a.url.as_deref() else { continue };
        if out.iter().any(|q| q.url == url) {
            continue;
        }
        if let Some(l) = crate::jobs::detect::classify_url(url) {
            if !out.iter().any(|q| q.url == l.url) {
                out.push(person_link(&l, Some(a)));
            }
        }
    }
    if out.len() == 1 && out[0].title.is_none() {
        let (title, company) = extract::split_title(&e.subject);
        out[0].title = Some(title).filter(|t| !t.trim().is_empty());
        if out[0].company.is_none() {
            out[0].company = company;
        }
    }
    for q in &mut out {
        // A job site's own name is not the employer.
        if q.company.is_none() && !alerts::is_login_platform(&signal.source) {
            q.company = signal.company_hint.clone();
        }
        q.person_id = person_id.map(str::to_string);
        q.mailbox = e.account_email.clone();
        q.email_id = Some(e.id.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::detect::SignalKind;
    use wiremock::matchers::{body_partial_json, header, method, path};
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

    #[test]
    fn alert_metadata_is_attached() {
        let li = "https://www.linkedin.com/jobs/view/1/";
        let li2 = "https://www.linkedin.com/jobs/view/2/";
        let listed = vec![
            AlertJob {
                title: "Lead AI Engineer".into(),
                company: Some("Netrolynx AI".into()),
                location: Some("United States".into()),
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
        let s = signal("linkedin", vec![jl(li, "linkedin", Some("ignored label"))], Some("LinkedIn"));
        let out = links_for_email(&email("Lead AI Engineer at Netrolynx AI"), &s, &listed, Some("shashi"));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].title.as_deref(), Some("Lead AI Engineer"));
        assert_eq!(out[0].company.as_deref(), Some("Netrolynx AI"));
        assert_eq!(out[0].location.as_deref(), Some("United States"));
        assert_eq!(out[0].platform.as_deref(), Some("linkedin"));
        assert_eq!(out[1].url, li2, "a job only the parser found is queued too");
        assert_eq!(out[1].company.as_deref(), Some("Beni Health"));
        for q in &out {
            assert_eq!(q.person_id.as_deref(), Some("shashi"));
            assert_eq!(q.mailbox.as_deref(), Some("me@example.com"));
            assert_eq!(q.email_id.as_deref(), Some("email-1"));
            assert_eq!(q.source, "email");
        }
    }

    #[test]
    fn single_link_falls_back_to_label_then_subject() {
        let lever = "https://jobs.lever.co/globex/abc";
        let s = signal("recruiter", vec![jl(lever, "lever", None)], Some("Globex"));
        let out = links_for_email(&email("Staff Rust Engineer at Globex"), &s, &[], None);
        assert_eq!(out[0].title.as_deref(), Some("Staff Rust Engineer"));
        assert_eq!(out[0].company.as_deref(), Some("Globex"));
        assert_eq!(out[0].person_id, None);

        let s = signal("recruiter", vec![jl(lever, "lever", Some("Platform Engineer"))], None);
        let out = links_for_email(&email("Opportunity"), &s, &[], None);
        assert_eq!(out[0].title.as_deref(), Some("Platform Engineer"));
    }

    #[tokio::test]
    async fn push_posts_a_batch_with_the_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/links"))
            .and(header("x-joblinks-token", "secret-token-0123456789"))
            .and(body_partial_json(serde_json::json!({
                "links": [{ "url": "https://jobs.lever.co/acme/1", "source": "email", "platform": "lever", "person_id": "p1" }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "created": 1, "updated": 0, "failed": 0, "results": []
            })))
            .expect(1)
            .mount(&server)
            .await;
        let c = LinkQueueClient::new(reqwest::Client::new(), &format!("{}/", server.uri()), "secret-token-0123456789");
        let link = QueuedLink {
            url: "https://jobs.lever.co/acme/1".into(),
            source: "email",
            platform: Some("lever".into()),
            person_id: Some("p1".into()),
            ..Default::default()
        };
        let s = c.push(std::slice::from_ref(&link)).await.unwrap();
        assert_eq!(s, QueueSummary { created: 1, updated: 0, failed: 0 });
        assert_eq!(c.push(&[]).await.unwrap(), QueueSummary::default(), "nothing to send");
    }

    #[tokio::test]
    async fn push_reports_service_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/links"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({ "error": "missing or wrong x-joblinks-token" })))
            .mount(&server)
            .await;
        let c = LinkQueueClient::new(reqwest::Client::new(), &server.uri(), "wrong");
        let err = c
            .push(&[QueuedLink {
                url: "https://jobs.lever.co/acme/1".into(),
                source: "email",
                ..Default::default()
            }])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("401"), "{err}");
    }
}
