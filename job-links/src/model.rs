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

/// One queued job link.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkRecord {
    pub id: String,
    /// Canonical URL (dedupe key together with `person_id`).
    pub url: String,
    pub original_url: String,
    pub source: Source,
    pub platform: Platform,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub company: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub person_id: Option<String>,
    #[serde(default)]
    pub mailbox: Option<String>,
    #[serde(default)]
    pub email_id: Option<String>,
    pub status: Status,
    #[serde(default)]
    pub status_reason: Option<String>,
    #[serde(default)]
    pub claimed_by: Option<String>,
    #[serde(default)]
    pub claimed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub lease_expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// How many times the link has been claimed.
    #[serde(default)]
    pub attempts: u32,
}

/// `POST /v1/links` item.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NewLink {
    pub url: String,
    #[serde(default)]
    pub source: Option<Source>,
    #[serde(default)]
    pub platform: Option<Platform>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub company: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub person_id: Option<String>,
    #[serde(default)]
    pub mailbox: Option<String>,
    #[serde(default)]
    pub email_id: Option<String>,
}

/// `PATCH /v1/links/{id}`: metadata edits and manual status changes. An
/// empty string clears a field.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct LinkPatch {
    pub title: Option<String>,
    pub company: Option<String>,
    pub location: Option<String>,
    pub mailbox: Option<String>,
    pub email_id: Option<String>,
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
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    /// `asc` (oldest first) or `desc` (default, newest first).
    pub order: Option<String>,
}
