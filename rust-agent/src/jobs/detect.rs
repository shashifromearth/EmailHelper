//! Job-opportunity detection in email: alert emails, recruiter outreach,
//! interview invitations, referrals and direct ATS links.

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::db::emails::EmailRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    JobAlert,
    RecruiterOutreach,
    InterviewInvite,
    Referral,
    DirectLink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobLink {
    pub url: String,
    /// greenhouse | lever | ashby | workday | linkedin | naukri | glassdoor | wellfound | foundit | indeed | career_site
    pub ats: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSignal {
    pub kind: SignalKind,
    pub source: String,
    /// Automated alert emails get no reply draft.
    pub is_alert: bool,
    pub links: Vec<JobLink>,
    pub company_hint: Option<String>,
}

const MAX_LINKS: usize = 15;

fn url_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?i)https?://[^\s<>"'()\]\[]+"#).expect("valid regex"))
}

/// Classify a URL as a job posting link and normalise it, or return `None`.
pub fn classify_url(raw: &str) -> Option<JobLink> {
    let trimmed = raw.trim_end_matches(['.', ',', ';', ')', '>', '*']);
    let mut u = url::Url::parse(trimmed).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    let path = u.path().to_ascii_lowercase();

    // LinkedIn alert links look like /comm/jobs/view/123?trackingId=...
    if host.ends_with("linkedin.com") {
        let re = {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| Regex::new(r"/jobs/view/(\d+)").expect("valid regex"))
        };
        let id = re.captures(&path)?.get(1)?.as_str().to_string();
        return Some(JobLink {
            url: format!("https://www.linkedin.com/jobs/view/{id}/"),
            ats: "linkedin".into(),
        });
    }

    // Strip tracking parameters everywhere else.
    let kept: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| {
            let k = k.to_ascii_lowercase();
            !(k.starts_with("utm_")
                || k == "gh_src"
                || k == "source"
                || k == "ref"
                || k == "trk"
                || k == "trackingid"
                || k == "refid"
                || k == "lipi"
                || k == "src")
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        u.set_query(None);
    } else {
        u.query_pairs_mut().clear().extend_pairs(kept);
    }
    u.set_fragment(None);

    let ats = if host.contains("greenhouse.io") || host == "grnh.se" {
        (path.contains("/jobs/") || host == "grnh.se" || path.contains("embed/job_app"))
            .then_some("greenhouse")
    } else if host == "jobs.lever.co" || host == "jobs.eu.lever.co" {
        (path.split('/').filter(|s| !s.is_empty()).count() >= 2).then_some("lever")
    } else if host == "jobs.ashbyhq.com" {
        (path.split('/').filter(|s| !s.is_empty()).count() >= 2).then_some("ashby")
    } else if host.ends_with("myworkdayjobs.com") {
        path.contains("/job/").then_some("workday")
    } else if host.ends_with("naukri.com") {
        (path.contains("job-listings") || path.contains("/job/")).then_some("naukri")
    } else if host.ends_with("glassdoor.com") || host.ends_with("glassdoor.co.in") {
        (path.contains("job-listing") || path.contains("/job/")).then_some("glassdoor")
    } else if host.ends_with("wellfound.com") || host.ends_with("angel.co") {
        path.contains("/jobs/").then_some("wellfound")
    } else if host.ends_with("foundit.in") || host.ends_with("monsterindia.com") {
        (path.contains("job")).then_some("foundit")
    } else if host.ends_with("indeed.com") || host.ends_with("indeed.co.in") {
        (path.contains("viewjob") || path.contains("/rc/clk")).then_some("indeed")
    } else {
        let careerish = host.starts_with("careers.") || host.starts_with("jobs.");
        let pathish = [
            "/careers/",
            "/jobs/",
            "/job/",
            "/positions/",
            "/openings/",
            "/vacancies/",
        ]
        .iter()
        .any(|p| path.contains(p));
        // Avoid unsubscribe / settings links that live under /jobs/.
        let noise = [
            "unsubscribe",
            "preferences",
            "settings",
            "alerts",
            "search",
            "manage",
        ]
        .iter()
        .any(|n| path.contains(n));
        ((careerish || pathish) && !noise && path.len() > 1).then_some("career_site")
    }?;
    Some(JobLink {
        url: u.to_string(),
        ats: ats.into(),
    })
}

/// Extract normalised, de-duplicated job links from text and HTML.
pub fn extract_links(text: &str, html: Option<&str>) -> Vec<JobLink> {
    let mut out: Vec<JobLink> = Vec::new();
    let mut push = |s: &str| {
        let s = s.replace("&amp;", "&");
        if let Some(l) = classify_url(&s) {
            if !out.iter().any(|x| x.url == l.url) && out.len() < MAX_LINKS {
                out.push(l);
            }
        }
    };
    for m in url_re().find_iter(text) {
        push(m.as_str());
    }
    if let Some(h) = html {
        for m in url_re().find_iter(h) {
            push(m.as_str());
        }
    }
    out
}

fn sender_source(from_email: &str) -> Option<&'static str> {
    let d = from_email
        .rsplit('@')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    [
        ("linkedin.com", "linkedin"),
        ("naukri.com", "naukri"),
        ("glassdoor.com", "glassdoor"),
        ("glassdoor.co.in", "glassdoor"),
        ("wellfound.com", "wellfound"),
        ("angel.co", "wellfound"),
        ("foundit.in", "foundit"),
        ("monsterindia.com", "foundit"),
        ("indeed.com", "indeed"),
        ("greenhouse.io", "greenhouse"),
        ("lever.co", "lever"),
        ("ashbyhq.com", "ashby"),
        ("instahyre.com", "instahyre"),
        ("hirist.com", "hirist"),
    ]
    .iter()
    .find(|(dom, _)| d == *dom || d.ends_with(&format!(".{dom}")))
    .map(|(_, s)| *s)
}

/// Detect a job opportunity. Returns `None` for ordinary mail.
pub fn detect_job_email(e: &EmailRecord) -> Option<JobSignal> {
    let subject = e.subject.to_lowercase();
    let body = e.body_text.to_lowercase();
    let text = format!("{subject}\n{body}");
    let has = |ws: &[&str]| ws.iter().any(|w| text.contains(w));

    let links = extract_links(&e.body_text, e.body_html.as_deref());
    let source = sender_source(&e.from_email);

    let alert_phrases = [
        "job alert",
        "jobs you may be interested",
        "recommended jobs",
        "recommended for you",
        "new jobs for",
        "jobs matching",
        "jobs based on your",
        "is hiring",
        "apply now",
        "easy apply",
        "new job matches",
        "top job picks",
        "jobs similar to",
    ];
    let interview = [
        "interview invitation",
        "invite you to interview",
        "schedule an interview",
        "schedule a call",
        "next round",
        "interview slot",
        "technical interview",
    ];
    let referral = ["referral", "refer you", "referred you", "i'd like to refer"];
    let outreach = [
        "job opportunity",
        "exciting opportunity",
        "open position",
        "open role",
        "your profile",
        "came across your profile",
        "we are hiring",
        "we're hiring",
        "would you be interested",
        "role at",
        "position at",
        "opportunity at",
        "recruiter",
        "talent acquisition",
        "hiring for",
        "looking for a",
        "job description",
    ];

    let hiring_context = has(&[
        "interview",
        "role",
        "position",
        "application",
        "candidate",
        "candidacy",
    ]);
    let kind = if has(&interview) && hiring_context {
        Some(SignalKind::InterviewInvite)
    } else if source.is_some() && (has(&alert_phrases) || !links.is_empty()) {
        Some(SignalKind::JobAlert)
    } else if has(&referral) && (has(&outreach) || !links.is_empty()) {
        Some(SignalKind::Referral)
    } else if has(&outreach)
        && (outreach.iter().filter(|w| text.contains(*w)).count() >= 2 || !links.is_empty())
    {
        Some(SignalKind::RecruiterOutreach)
    } else if links.iter().any(|l| l.ats != "career_site") {
        Some(SignalKind::DirectLink)
    } else {
        None
    }?;

    let is_alert = kind == SignalKind::JobAlert
        || crate::agents::classifier::is_automated_sender(&e.from_email);
    let source = source
        .map(str::to_string)
        .or_else(|| links.first().map(|l| l.ats.clone()))
        .unwrap_or_else(|| "recruiter".into());
    Some(JobSignal {
        kind,
        source,
        is_alert,
        company_hint: company_from_text(&e.subject).or_else(|| company_from_domain(&e.from_email)),
        links,
    })
}

/// "Senior Engineer at Acme" / "Acme is hiring" → "Acme".
pub fn company_from_text(s: &str) -> Option<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| {
        Regex::new(r"(?:\bat|@)\s+([A-Z][\w&.\- ]{1,40}?)(?:\s*[-|,:!(·]|\s+(?:is|in|and|for|has|with)\b|$)|^([A-Z][\w&.\- ]{1,40}?)\s+is hiring")
            .expect("valid regex")
    });
    let c = re.captures(s.trim())?;
    let name = c.get(1).or_else(|| c.get(2))?.as_str().trim().to_string();
    (!name.is_empty()).then_some(name)
}

const FREE_MAIL: &[&str] = &[
    "gmail.com",
    "yahoo.com",
    "outlook.com",
    "hotmail.com",
    "live.com",
    "icloud.com",
    "proton.me",
    "protonmail.com",
    "zoho.com",
    "aol.com",
    "rediffmail.com",
    "yahoo.co.in",
];

/// Company guess from a corporate sender domain (`jane@acme.io` → "Acme").
pub fn company_from_domain(from_email: &str) -> Option<String> {
    let d = from_email.rsplit('@').next()?.to_ascii_lowercase();
    if FREE_MAIL.contains(&d.as_str()) || sender_source(from_email).is_some() {
        return None;
    }
    let parts: Vec<&str> = d.split('.').collect();
    let name = if parts.len() >= 3 && ["co", "com", "ac"].contains(&parts[parts.len() - 2]) {
        parts[parts.len() - 3]
    } else if parts.len() >= 2 {
        parts[parts.len() - 2]
    } else {
        return None;
    };
    let mut c = name.chars();
    let first = c.next()?.to_uppercase().collect::<String>();
    Some(first + c.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email(from: &str, subject: &str, body: &str) -> EmailRecord {
        EmailRecord {
            id: "e".into(),
            account_id: "a".into(),
            account_name: None,
            account_email: None,
            account_type: "gmail".into(),
            message_id: "<m>".into(),
            tb_message_id: 1,
            folder: None,
            in_reply_to: None,
            references: vec![],
            from: from.into(),
            from_email: crate::db::emails::email_address(from),
            to: vec![],
            cc: vec![],
            subject: subject.into(),
            received_at: None,
            body_text: body.into(),
            body_html: None,
            attachments: vec![],
            category: None,
            is_job: false,
            status: "received".into(),
            created_at: "now".into(),
        }
    }

    #[test]
    fn normalises_links() {
        let l = classify_url(
            "https://www.linkedin.com/comm/jobs/view/3912345678/?trackingId=abc&refId=x",
        )
        .unwrap();
        assert_eq!(l.url, "https://www.linkedin.com/jobs/view/3912345678/");
        let g =
            classify_url("https://boards.greenhouse.io/acme/jobs/4012345?gh_src=li&utm_source=x")
                .unwrap();
        assert_eq!(g.ats, "greenhouse");
        assert_eq!(g.url, "https://boards.greenhouse.io/acme/jobs/4012345");
        assert_eq!(
            classify_url("https://jobs.lever.co/acme/1b2c-3d")
                .unwrap()
                .ats,
            "lever"
        );
        assert_eq!(
            classify_url("https://jobs.ashbyhq.com/acme/9f8e")
                .unwrap()
                .ats,
            "ashby"
        );
        assert_eq!(
            classify_url("https://acme.com/careers/staff-ai-engineer")
                .unwrap()
                .ats,
            "career_site"
        );
        assert!(classify_url("https://www.linkedin.com/feed/").is_none());
        assert!(classify_url("https://acme.com/jobs/alerts/unsubscribe").is_none());
        assert!(classify_url("https://jobs.lever.co/acme").is_none());
    }

    #[test]
    fn detects_linkedin_alert() {
        let e = email(
            "LinkedIn Job Alerts <jobalerts-noreply@linkedin.com>",
            "Staff AI Engineer at Acme and 9 more jobs",
            "Your job alert for AI Architect\nStaff AI Engineer\nAcme · Bengaluru (Remote)\nhttps://www.linkedin.com/comm/jobs/view/3912345678/?trackingId=abc\nView job",
        );
        let s = detect_job_email(&e).unwrap();
        assert_eq!(s.kind, SignalKind::JobAlert);
        assert_eq!(s.source, "linkedin");
        assert!(s.is_alert);
        assert_eq!(s.links.len(), 1);
        assert_eq!(s.company_hint.as_deref(), Some("Acme"));
    }

    #[test]
    fn detects_recruiter_outreach() {
        let e = email(
            "Priya Sharma <priya@globex.io>",
            "Opportunity: Lead AI Engineer",
            "Hi, I came across your profile and we are hiring a Lead AI Engineer. Would you be interested? JD: https://jobs.lever.co/globex/abc-123",
        );
        let s = detect_job_email(&e).unwrap();
        assert_eq!(s.kind, SignalKind::RecruiterOutreach);
        assert!(!s.is_alert);
        assert_eq!(s.company_hint.as_deref(), Some("Globex"));
        assert_eq!(s.links[0].ats, "lever");
    }

    #[test]
    fn detects_interview() {
        let e = email(
            "hr@initech.com",
            "Interview invitation - Rust Engineer",
            "We'd like to schedule an interview with you.",
        );
        assert_eq!(
            detect_job_email(&e).unwrap().kind,
            SignalKind::InterviewInvite
        );
    }

    #[test]
    fn ignores_normal_mail() {
        let e = email(
            "bob@acme.com",
            "Lunch?",
            "Want to grab lunch tomorrow at noon?",
        );
        assert!(detect_job_email(&e).is_none());
        let e = email(
            "support@vendor.com",
            "Your ticket",
            "We are looking into your ticket about the role permissions.",
        );
        assert!(detect_job_email(&e).is_none());
    }

    #[test]
    fn company_guesses() {
        assert_eq!(
            company_from_text("Acme is hiring a Rust Engineer").as_deref(),
            Some("Acme")
        );
        assert_eq!(
            company_from_text("Staff Engineer at Initech - Remote").as_deref(),
            Some("Initech")
        );
        assert_eq!(
            company_from_domain("x@mail.acme.co.uk").as_deref(),
            Some("Acme")
        );
        assert_eq!(company_from_domain("x@gmail.com"), None);
    }
}
