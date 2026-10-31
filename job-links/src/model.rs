//! Records and request bodies.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Where a link came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Email,
    Research,
    #[default]
    Manual,
}

/// The site a link belongs to. Unknown names deserialize as `other`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", from = "String")]
pub enum Platform {
    Linkedin,
    Naukri,
    Glassdoor,
    Wellfound,
    Indeed,
    Greenhouse,
    Lever,
    Ashby,
    Workday,
    CareerSite,
    Other,
}

impl Platform {
    pub const ALL: [Platform; 11] = [
        Self::Linkedin,
        Self::Naukri,
        Self::Glassdoor,
        Self::Wellfound,
        Self::Indeed,
        Self::Greenhouse,
        Self::Lever,
        Self::Ashby,
        Self::Workday,
        Self::CareerSite,
        Self::Other,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Linkedin => "linkedin",
            Self::Naukri => "naukri",
            Self::Glassdoor => "glassdoor",
            Self::Wellfound => "wellfound",
            Self::Indeed => "indeed",
            Self::Greenhouse => "greenhouse",
            Self::Lever => "lever",
            Self::Ashby => "ashby",
            Self::Workday => "workday",
            Self::CareerSite => "career_site",
            Self::Other => "other",
        }
    }

    /// Sites whose applications happen inside the person's logged-in account:
    /// the extension only fills the form there, the person submits.
    pub fn is_assisted(&self) -> bool {
        matches!(
            self,
            Self::Linkedin | Self::Naukri | Self::Glassdoor | Self::Wellfound | Self::Indeed
        )
    }
}

impl From<String> for Platform {
    fn from(s: String) -> Self {
        let s = s.trim().to_ascii_lowercase().replace('-', "_");
        Self::ALL
            .into_iter()
            .find(|p| p.as_str() == s)
            .unwrap_or(Self::Other)
    }
}

/// `new` → `claimed` → `applied` | `failed` | `skipped`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    New,
    Claimed,
    Applied,
    Failed,
    Skipped,
}

impl Status {
    pub const ALL: [Status; 5] = [
        Self::New,
        Self::Claimed,
        Self::Applied,
        Self::Failed,
        Self::Skipped,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Claimed => "claimed",
            Self::Applied => "applied",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Applied | Self::Failed | Self::Skipped)
    }
}

/// Work arrangement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", from = "String")]
pub enum Remote {
    Remote,
    Hybrid,
    Onsite,
    #[default]
    Unknown,
}

impl From<String> for Remote {
    fn from(s: String) -> Self {
        match s.trim().to_ascii_lowercase().replace('-', "").as_str() {
            "remote" => Self::Remote,
            "hybrid" => Self::Hybrid,
            "onsite" | "on site" | "office" | "in office" => Self::Onsite,
            _ => Self::Unknown,
        }
    }
}

/// One status change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatusChange {
    pub status: Status,
    #[serde(default)]
    pub reason: Option<String>,
    pub at: DateTime<Utc>,
}

/// One job in the shared job database.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkRecord {
    pub id: String,
    /// Canonical URL (dedupe key together with `person_id`).
    pub url: String,
    pub original_url: String,
    pub source: Source,
    /// Email id for `email`, board/API name (remotive, adzuna, …) for `research`.
    #[serde(default)]
    pub source_detail: Option<String>,
    pub platform: Platform,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub company: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub remote: Remote,
    /// full_time | part_time | contract | freelance | consultant | internship | …
    #[serde(default)]
    pub employment_type: Option<String>,
    #[serde(default)]
    pub salary: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    /// Excerpt, at most ~4 KB.
    #[serde(default)]
    pub description: Option<String>,
    /// Matcher score 0-100 and decision (apply/review/skip).
    #[serde(default)]
    pub score: Option<u8>,
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(default)]
    pub person_id: Option<String>,
    /// Which resume (stack) to upload for this job.
    #[serde(default)]
    pub resume_hint: Option<String>,
    #[serde(default)]
    pub mailbox: Option<String>,
    /// When the job was published, if known.
    #[serde(default)]
    pub posted_at: Option<DateTime<Utc>>,
    pub status: Status,
    #[serde(default)]
    pub status_reason: Option<String>,
    #[serde(default)]
    pub status_history: Vec<StatusChange>,
    #[serde(default)]
    pub claimed_by: Option<String>,
    #[serde(default)]
    pub claimed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub lease_expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    pub discovered_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// How many times the link has been claimed.
    #[serde(default)]
    pub attempts: u32,
}

/// `POST /v1/links` item. Everything but `url` is optional.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NewLink {
    pub url: String,
    pub source: Option<Source>,
    pub source_detail: Option<String>,
    pub platform: Option<Platform>,
    pub title: Option<String>,
    pub company: Option<String>,
    pub location: Option<String>,
    pub remote: Option<Remote>,
    pub employment_type: Option<String>,
    pub salary: Option<String>,
    pub skills: Vec<String>,
    pub description: Option<String>,
    pub score: Option<u8>,
    pub decision: Option<String>,
    pub person_id: Option<String>,
    pub resume_hint: Option<String>,
    pub mailbox: Option<String>,
    pub posted_at: Option<DateTime<Utc>>,
}

/// `PATCH /v1/links/{id}`: metadata edits and manual status changes. An
/// empty string clears a text field.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct LinkPatch {
    pub title: Option<String>,
    pub company: Option<String>,
    pub location: Option<String>,
    pub remote: Option<Remote>,
    pub employment_type: Option<String>,
    pub salary: Option<String>,
    pub description: Option<String>,
    pub score: Option<u8>,
    pub decision: Option<String>,
    pub resume_hint: Option<String>,
    pub mailbox: Option<String>,
    pub status: Option<Status>,
    pub status_reason: Option<String>,
}

/// `POST /v1/links/claim`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ClaimRequest {
    /// Who is claiming (e.g. the extension's install id).
    pub client: String,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub lease_secs: Option<u64>,
    #[serde(default)]
    pub platforms: Option<Vec<Platform>>,
    #[serde(default)]
    pub person: Option<String>,
    /// Skip `new` links posted (or, without a date, first seen) more than
    /// this many hours ago; `0` = no limit. Unset = the service default
    /// (`JOB_LINKS_MAX_AGE_HOURS`). Old links are skipped, not deleted.
    #[serde(default)]
    pub max_age_hours: Option<u64>,
}

/// `POST /v1/links/{id}/result`.
#[derive(Clone, Debug, Deserialize)]
pub struct ResultRequest {
    pub status: Status,
    #[serde(default)]
    pub reason: Option<String>,
    /// When given, must match the client holding the claim.
    #[serde(default)]
    pub client: Option<String>,
}

/// Filters for `GET /v1/links`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ListQuery {
    pub status: Option<Status>,
    pub platform: Option<Platform>,
    pub person: Option<String>,
    pub source: Option<Source>,
    /// Only links updated at or after this time.
    pub since: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    /// `asc` (oldest first) or `desc` (default, newest first).
    pub order: Option<String>,
}
