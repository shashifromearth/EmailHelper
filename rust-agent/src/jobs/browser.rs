//! Client for the Playwright browser worker (documents/API.md §2).

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::jobs::extract::ExtractedPage;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Blocker {
    pub kind: String,
    #[serde(default)]
    pub detail: String,
}

impl Blocker {
    /// Blockers only a human can clear in the browser (vs. answerable by text).
    pub fn needs_browser_action(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "captcha" | "otp" | "mfa" | "login_required" | "access_denied"
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Field {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub accept: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InspectResult {
    #[serde(default)]
    pub final_url: String,
    #[serde(default)]
    pub ats: String,
    #[serde(default)]
    pub fields: Vec<Field>,
    #[serde(default)]
    pub blocker: Option<Blocker>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FillResult {
    pub status: String,
    #[serde(default)]
    pub final_url: String,
    #[serde(default)]
    pub missing_required: Vec<String>,
    #[serde(default)]
    pub blocker: Option<Blocker>,
    #[serde(default)]
    pub confirmation_text: Option<String>,
    #[serde(default)]
    pub screenshot_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Person {
    pub name: String,
    #[serde(default)]
    pub headline: String,
    #[serde(default)]
    pub profile_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PeopleResult {
    #[serde(default)]
    pub people: Vec<Person>,
    #[serde(default)]
    pub blocker: Option<Blocker>,
}

#[derive(Clone)]
pub struct BrowserClient {
    http: reqwest::Client,
    base: String,
    token: String,
    timeout: Duration,
}

impl BrowserClient {
    pub fn new(http: reqwest::Client, base: &str, token: &str, timeout: Duration) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            timeout,
        }
    }

    async fn post<T: for<'de> Deserialize<'de>>(&self, path: &str, body: Value) -> AppResult<T> {
        let resp = self
            .http
            .post(format!("{}{}", self.base, path))
            .header("X-Worker-Token", &self.token)
            .timeout(self.timeout)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(AppError::other(format!(
                "browser worker {path} returned {status}: {}",
                text.chars().take(300).collect::<String>()
            )));
        }
        Ok(resp.json().await?)
    }

    pub async fn health(&self) -> bool {
        match self
            .http
            .get(format!("{}/health", self.base))
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }

    pub async fn extract(&self, url: &str) -> AppResult<ExtractedPage> {
        self.post("/v1/extract", json!({ "url": url })).await
    }

    pub async fn inspect(&self, session_key: &str, url: &str) -> AppResult<InspectResult> {
        self.post(
            "/v1/apply/inspect",
            json!({ "session_key": session_key, "url": url }),
        )
        .await
    }

    pub async fn fill(
        &self,
        session_key: &str,
        url: &str,
        answers: &serde_json::Map<String, Value>,
        files: &BTreeMap<String, String>,
        submit: bool,
    ) -> AppResult<FillResult> {
        self.post(
            "/v1/apply/fill",
            json!({
                "session_key": session_key,
                "url": url,
                "answers": answers,
                "files": files,
                "submit": submit,
            }),
        )
        .await
    }

    pub async fn linkedin_people(
        &self,
        company: &str,
        roles: &[&str],
        limit: usize,
    ) -> AppResult<PeopleResult> {
        self.post(
            "/v1/linkedin/people",
            json!({ "company": company, "roles": roles, "limit": limit }),
        )
        .await
    }
}
