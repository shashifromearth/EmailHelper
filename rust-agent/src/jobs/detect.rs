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
    /// Text of the HTML link, when it names the job (not "View job").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
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

/// Query parameters a redirect wrapper keeps its target in.
const TARGET_PARAMS: &[&str] = &[
    "url", "q", "u", "target", "dest", "destination", "redirect", "redirect_url", "redirecturl",
    "redirect_uri", "link", "to", "data",
];

/// Hosts and paths that only forward the click somewhere else.
fn is_wrapper(host: &str, path: &str) -> bool {
    host.ends_with("safelinks.protection.outlook.com")
        || host == "l.facebook.com"
        || host == "dl.naukri.com"
        || host == "my.naukri.com"
        || ((host == "google.com" || host.starts_with("www.google.")) && path == "/url")
        || (host.ends_with("linkedin.com") && path.starts_with("/redir"))
        || ["/redirect", "/redir", "/click", "/track", "/r/"]
            .iter()
            .any(|p| path.starts_with(p))
}

/// A parameter value that is a URL, directly or base64-encoded.
fn url_in(v: &str) -> Option<String> {
    use base64::Engine;
    let v = v.trim();
    if v.starts_with("http://") || v.starts_with("https://") {
        return Some(v.to_string());
    }
    let engines = [
        base64::engine::general_purpose::URL_SAFE_NO_PAD,
        base64::engine::general_purpose::STANDARD_NO_PAD,
    ];
    let trimmed = v.trim_end_matches('=');
    engines
        .iter()
        .filter_map(|e| e.decode(trimmed).ok())
        .filter_map(|b| String::from_utf8(b).ok())
        .find(|s| s.starts_with("http://") || s.starts_with("https://"))
}

/// The URL a redirect/tracking wrapper (SafeLinks, Google, LinkedIn and
/// Naukri redirects) points at, when the target is in the URL itself.
/// Anything else is returned unchanged.
pub fn unwrap_redirect(raw: &str) -> String {
    let mut cur = raw.trim().replace("&amp;", "&");
    for _ in 0..4 {
        let Ok(u) = url::Url::parse(&cur) else { break };
        let host = u.host_str().unwrap_or_default().to_ascii_lowercase();
        if !is_wrapper(&host, &u.path().to_ascii_lowercase()) {
            break;
        }
        let next = u
            .query_pairs()
            .filter(|(k, _)| TARGET_PARAMS.contains(&k.to_ascii_lowercase().as_str()))
            .find_map(|(_, v)| url_in(&v));
        match next {
            Some(n) if n != cur => cur = n,
            _ => break,
        }
    }
    cur
}

/// LinkedIn job id from `/jobs/view/123`, `/comm/jobs/view/slug-123` or
/// `?currentJobId=123`.
fn linkedin_job_id(u: &url::Url) -> Option<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"/jobs/view/(?:[^/?]*-)?(\d+)").expect("valid regex"));
    re.captures(u.path())
        .map(|c| c[1].to_string())
        .or_else(|| {
            u.query_pairs()
                .find(|(k, v)| {
                    k.eq_ignore_ascii_case("currentjobid")
                        && !v.is_empty()
                        && v.chars().all(|c| c.is_ascii_digit())
                })
                .map(|(_, v)| v.into_owned())
        })
}

/// Classify a URL as a job posting link and normalise it, or return `None`.
/// Redirect wrappers are unwrapped first.
pub fn classify_url(raw: &str) -> Option<JobLink> {
    let unwrapped = unwrap_redirect(raw);
    let trimmed = unwrapped.trim_end_matches(['.', ',', ';', ')', '>', '*']);
    let mut u = url::Url::parse(trimmed).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    let path = u.path().to_ascii_lowercase();
    let link = |url: String, ats: &str| JobLink {
        url,
        ats: ats.into(),
        label: None,
    };

    // LinkedIn alert links look like /comm/jobs/view/123?trackingId=...
    if host.ends_with("linkedin.com") {
        let id = linkedin_job_id(&u)?;
        return Some(link(format!("https://www.linkedin.com/jobs/view/{id}/"), "linkedin"));
    }
    // Indeed: the `jk` parameter is the job.
    if host.contains("indeed.") {
        let jk = u.query_pairs().find(|(k, _)| k == "jk" || k == "vjk").map(|(_, v)| v.into_owned());
        if let Some(jk) = jk.filter(|j| !j.is_empty()) {
            return Some(link(format!("https://{host}/viewjob?jk={jk}"), "indeed"));
        }
    }

    // Strip tracking parameters everywhere else (Naukri listings end in the
    // job id, so their query is all tracking).
    let naukri_listing = host.ends_with("naukri.com") && path.contains("job-listings");
    let kept: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| {
            let k = k.to_ascii_lowercase();
            !(naukri_listing
                || k.starts_with("utm_")
                || k == "gh_src"
                || k == "source"
                || k == "ref"
                || k == "trk"
                || k == "trackingid"
                || k == "refid"
                || k == "lipi"
                || k == "src"
                || k == "gclid"
                || k == "fbclid"
                || k == "lever-source"
                || k == "lever-origin")
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
    Some(link(u.to_string(), ats))
}

/// Link text worth keeping as a job title hint.
fn label_of(text: &str) -> Option<String> {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = t.to_lowercase();
    let generic = [
        "view job", "view jobs", "view", "apply", "apply now", "easy apply", "see job", "see jobs",
        "see more jobs", "view & apply", "view and apply", "view opportunity", "click here", "here",
        "learn more", "more", "read more", "details", "job details", "view details", "see all jobs",
    ];
    let looks_like_url = lower.starts_with("http") || lower.starts_with("www.");
    (t.chars().count() >= 3 && t.chars().count() <= 150 && !generic.contains(&lower.as_str()) && !looks_like_url)
        .then_some(t)
}

/// `(href, text)` of every `<a href>` in an HTML body.
fn anchors(html: &str) -> Vec<(String, String)> {
    static SEL: OnceLock<scraper::Selector> = OnceLock::new();
    let sel = SEL.get_or_init(|| scraper::Selector::parse("a[href]").expect("valid selector"));
    let doc = scraper::Html::parse_document(html);
    doc.select(sel)
        .filter_map(|a| {
            let href = a.value().attr("href")?.trim().to_string();
            Some((href, a.text().collect::<String>()))
        })
        .collect()
}

/// Extract normalised, de-duplicated job links from text and HTML: plain
/// URLs in the text, `<a href>` targets (their text becomes the label), and
/// any other URL in the HTML source.
pub fn extract_links(text: &str, html: Option<&str>) -> Vec<JobLink> {
    let mut out: Vec<JobLink> = Vec::new();
    let mut push = |s: &str, label: Option<String>| {
        let Some(mut l) = classify_url(s) else { return };
        match out.iter().position(|x| x.url == l.url) {
            Some(i) => {
                if out[i].label.is_none() {
                    out[i].label = label;
                }
            }
            None if out.len() < MAX_LINKS => {
                l.label = label;
                out.push(l);
            }
            None => {}
        }
    };
    for m in url_re().find_iter(text) {
        push(m.as_str(), None);
    }
    if let Some(h) = html {
        for (href, text) in anchors(h) {
            push(&href, label_of(&text));
        }
        for m in url_re().find_iter(h) {
            push(m.as_str(), None);
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
    // Automated mailers that say what they are, whoever sends them (a
    // company's careers@ address isn't a known job site).
    let mass_alert = [
        "job alert",
        "this job is a match",
        "we invite you to apply",
        "invite you to apply",
        "jobs you may be interested",
        "recommended jobs",
        "new job matches",
        "top job picks",
    ];
    let kind = if has(&interview) && hiring_context {
        Some(SignalKind::InterviewInvite)
    } else if (source.is_some() && (has(&alert_phrases) || !links.is_empty())) || has(&mass_alert) {
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
        assert_eq!(
            classify_url("https://in.linkedin.com/jobs/view/staff-ai-engineer-at-acme-3912345678?trk=x")
                .unwrap()
                .url,
            "https://www.linkedin.com/jobs/view/3912345678/"
        );
        assert_eq!(
            classify_url("https://www.linkedin.com/jobs/collections/recommended/?currentJobId=3912345678")
                .unwrap()
                .url,
            "https://www.linkedin.com/jobs/view/3912345678/"
        );
        let indeed = classify_url("https://in.indeed.com/rc/clk?jk=abc123&from=ja").unwrap();
        assert_eq!((indeed.url.as_str(), indeed.ats.as_str()), ("https://in.indeed.com/viewjob?jk=abc123", "indeed"));
        assert_eq!(
            classify_url("https://www.naukri.com/job-listings-ai-engineer-acme-5-to-10-years-161026503618?src=ja&xp=1")
                .unwrap()
                .url,
            "https://www.naukri.com/job-listings-ai-engineer-acme-5-to-10-years-161026503618"
        );
    }

    fn enc(s: &str) -> String {
        url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
    }

    #[test]
    fn unwraps_redirect_wrappers() {
        let lever = "https://jobs.lever.co/acme/1b2c-3d";
        let safelink = format!(
            "https://eur01.safelinks.protection.outlook.com/?url={}&data=05%7C01%7C&reserved=0",
            enc(lever)
        );
        assert_eq!(unwrap_redirect(&safelink), lever);
        let google = format!("https://www.google.com/url?q={}&sa=D", enc(&safelink));
        assert_eq!(unwrap_redirect(&google), lever, "nested wrappers");
        let li = format!("https://www.linkedin.com/redir/redirect?url={}&urlhash=x", enc(lever));
        assert_eq!(classify_url(&li).unwrap().ats, "lever");
        let naukri = "https://www.naukri.com/job-listings-ai-engineer-acme-161026503618";
        let my = format!("https://my.naukri.com/AL/redirect?id=7&url={}", enc(naukri));
        assert_eq!(classify_url(&my).unwrap().url, naukri);
        use base64::Engine;
        let dl = format!(
            "https://dl.naukri.com/?data={}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(naukri)
        );
        assert_eq!(classify_url(&dl).unwrap().ats, "naukri");
        // Opaque wrappers and normal pages are left alone.
        assert!(classify_url("https://dl.naukri.com?data=x").is_none());
        let page = "https://acme.com/jobs/42?url=https%3A%2F%2Fevil.example";
        assert_eq!(unwrap_redirect(page), page);
    }

    #[test]
    fn extracts_html_anchors_with_labels() {
        let lever = "https://jobs.lever.co/acme/1b2c-3d";
        let html = format!(
            r#"<html><body>
            <a href="https://www.linkedin.com/comm/jobs/view/3912345678/?trackingId=a&amp;refId=b">Staff AI Engineer</a>
            <a href="https://www.linkedin.com/comm/jobs/view/3912345678/?trackingId=c">View job</a>
            <a href="https://eur01.safelinks.protection.outlook.com/?url={}&amp;data=x">  Rust
               Engineer </a>
            <a href="https://www.linkedin.com/comm/jobs/view/4000000001/">View job</a>
            <a href="https://www.linkedin.com/feed/">Home</a>
            <a href="mailto:x@acme.com">mail</a>
            </body></html>"#,
            enc(lever)
        );
        let links = extract_links("", Some(&html));
        assert_eq!(links.len(), 3, "{links:?}");
        assert_eq!(links[0].url, "https://www.linkedin.com/jobs/view/3912345678/");
        assert_eq!(links[0].label.as_deref(), Some("Staff AI Engineer"));
        assert_eq!(links[1].url, lever);
        assert_eq!(links[1].label.as_deref(), Some("Rust Engineer"));
        assert_eq!(links[2].label, None, "generic link text is not a title");
    }

    #[test]
    fn company_job_alerts_are_alerts() {
        let e = email(
            "Qualcomm Careers <careers@qualcomm.com>",
            "Shashi, this job is a match!",
            "Hi Shashi,
Based on your profile, we believe you may be a great match for an existing opportunity at Qualcomm! We invite you to apply below.
This job alert is unrelated to any active opportunities.
Linux Software Engineer, Lead Senior
Bangalore, India",
        );
        let s = detect_job_email(&e).unwrap();
        assert_eq!(s.kind, SignalKind::JobAlert);
        assert!(s.is_alert, "no reply drafted for a mass job alert");
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
