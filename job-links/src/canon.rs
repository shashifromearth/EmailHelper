//! URL canonicalisation and platform classification.
//!
//! The canonical URL is the dedupe key: redirect wrappers are unwrapped,
//! tracking parameters dropped and the remaining query sorted, while
//! parameters that identify the job (`gh_jid`, `jk`, `jl`, …) are kept.

use url::Url;

use crate::model::Platform;

/// Query parameters that only track the click, compared lowercased.
const TRACKING: &[&str] = &[
    "gclid", "fbclid", "msclkid", "dclid", "yclid", "mc_cid", "mc_eid", "_hsenc", "_hsmi",
    "hsctatracking", "mkt_tok", "trk", "trkinfo", "trackingid", "refid", "lipi", "midtoken",
    "midsig", "eid", "otptoken", "ref", "referer", "referrer", "src", "source", "gh_src",
    "lever-source", "lever-origin", "xp", "from", "campaign", "cmp",
];

/// Query parameters a redirect wrapper keeps its target in.
const TARGET_PARAMS: &[&str] = &[
    "url", "q", "u", "target", "dest", "destination", "redirect", "redirect_url", "redirecturl",
    "redirect_uri", "link", "to",
];

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CanonError {
    #[error("not a valid URL: {0}")]
    Invalid(String),
    #[error("only http and https links are accepted")]
    Scheme,
}

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

/// The URL a redirect/tracking wrapper points at, when it is in the URL.
fn redirect_target(u: &Url) -> Option<String> {
    let host = u.host_str()?.to_ascii_lowercase();
    if !is_wrapper(&host, &u.path().to_ascii_lowercase()) {
        return None;
    }
    u.query_pairs()
        .filter(|(k, _)| TARGET_PARAMS.contains(&k.to_ascii_lowercase().as_str()))
        .map(|(_, v)| v.trim().to_string())
        .find(|v| v.starts_with("http://") || v.starts_with("https://"))
}

/// Follow redirect wrappers whose target is in the URL (a few levels deep).
pub fn unwrap_redirect(raw: &str) -> String {
    let mut cur = raw.trim().to_string();
    for _ in 0..4 {
        let Ok(u) = Url::parse(&cur) else { break };
        match redirect_target(&u) {
            Some(next) if next != cur => cur = next,
            _ => break,
        }
    }
    cur
}

/// LinkedIn job id from `/jobs/view/123`, `/comm/jobs/view/slug-123` or
/// `?currentJobId=123`.
fn linkedin_job_id(u: &Url) -> Option<String> {
    let segs: Vec<&str> = u.path_segments()?.collect();
    let from_path = segs
        .windows(2)
        .find(|w| w[0] == "view" && segs.contains(&"jobs"))
        .map(|w| w[1])
        .and_then(|s| {
            let tail = s.rsplit('-').next().unwrap_or(s);
            (!tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit())).then(|| tail.to_string())
        });
    from_path.or_else(|| {
        u.query_pairs()
            .find(|(k, v)| k.eq_ignore_ascii_case("currentjobid") && v.chars().all(|c| c.is_ascii_digit()) && !v.is_empty())
            .map(|(_, v)| v.into_owned())
    })
}

/// Canonical form of a job link (see the module docs).
pub fn canonicalize(raw: &str) -> Result<String, CanonError> {
    let unwrapped = unwrap_redirect(raw);
    let mut u = Url::parse(&unwrapped).map_err(|e| CanonError::Invalid(format!("{raw}: {e}")))?;
    if u.scheme() != "http" && u.scheme() != "https" {
        return Err(CanonError::Scheme);
    }
    let host = u.host_str().unwrap_or_default().to_ascii_lowercase();
    if host.is_empty() {
        return Err(CanonError::Invalid(raw.to_string()));
    }
    u.set_fragment(None);
    let _ = u.set_password(None);
    let _ = u.set_username("");

    if host.ends_with("linkedin.com") {
        if let Some(id) = linkedin_job_id(&u) {
            return Ok(format!("https://www.linkedin.com/jobs/view/{id}/"));
        }
    }
    if host.contains("indeed.") {
        if let Some((_, jk)) = u.query_pairs().find(|(k, _)| k == "jk" || k == "vjk") {
            return Ok(format!("https://{host}/viewjob?jk={jk}"));
        }
    }

    let path = u.path().to_ascii_lowercase();
    let mut kept: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| {
            let k = k.to_ascii_lowercase();
            if host.ends_with("naukri.com") && path.contains("job-listings") {
                return false; // the listing slug ends in the job id
            }
            if host.contains("glassdoor.") && path.contains("job-listing") {
                return k == "jl" || k == "joblistingid";
            }
            !(k.starts_with("utm_") || TRACKING.contains(&k.as_str()))
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    kept.sort();
    if kept.is_empty() {
        u.set_query(None);
    } else {
        u.query_pairs_mut().clear().extend_pairs(kept);
    }
    if u.path().len() > 1 && u.path().ends_with('/') {
        let p = u.path().trim_end_matches('/').to_string();
        u.set_path(&p);
    }
    Ok(u.to_string())
}

/// Which site a (canonical) link belongs to.
pub fn classify(url: &str) -> Platform {
    let Ok(u) = Url::parse(url) else {
        return Platform::Other;
    };
    let host = u.host_str().unwrap_or_default().to_ascii_lowercase();
    let path = u.path().to_ascii_lowercase();
    let has_param = |name: &str| u.query_pairs().any(|(k, _)| k.eq_ignore_ascii_case(name));
    let on = |dom: &str| host == dom || host.ends_with(&format!(".{dom}"));
    if on("linkedin.com") {
        Platform::Linkedin
    } else if on("naukri.com") {
        Platform::Naukri
    } else if host.contains("glassdoor.") {
        Platform::Glassdoor
    } else if on("wellfound.com") || on("angel.co") {
        Platform::Wellfound
    } else if host.contains("indeed.") {
        Platform::Indeed
    } else if on("greenhouse.io") || host == "grnh.se" || has_param("gh_jid") {
        Platform::Greenhouse
    } else if on("lever.co") {
        Platform::Lever
    } else if on("ashbyhq.com") || has_param("ashby_jid") {
        Platform::Ashby
    } else if on("myworkdayjobs.com") || on("myworkdaysite.com") || on("workday.com") {
        Platform::Workday
    } else if is_listing_site(&host) {
        Platform::JobBoard
    } else if host.starts_with("careers.")
        || host.starts_with("jobs.")
        || ["/careers", "/career/", "/jobs/", "/job/", "/positions/", "/openings/", "/vacancies/"]
            .iter()
            .any(|p| path.starts_with(p) || path.contains(p))
    {
        Platform::CareerSite
    } else {
        Platform::Other
    }
}

/// Job listing sites (aggregators and remote boards) whose job pages link out
/// to the employer's own application.
pub fn is_listing_site(host: &str) -> bool {
    const DOMAINS: [&str; 18] = [
        "jobicy.com",
        "weworkremotely.com",
        "himalayas.app",
        "remoteok.com",
        "remoteok.io",
        "remotive.com",
        "remotive.io",
        "arc.dev",
        "workatastartup.com",
        "jobgether.com",
        "globalremote.co",
        "remotelyjobs.com",
        "themuse.com",
        "jooble.org",
        "careerjet.com",
        "careerjet.co.in",
        "adzuna.com",
        "adzuna.in",
    ];
    let on = |dom: &str| host == dom || host.ends_with(&format!(".{dom}"));
    // arbeitnow.com / .co.uk / .fr / .ch / .nl …
    let arbeitnow = host == "arbeitnow.com" || host.starts_with("arbeitnow.") || host.contains(".arbeitnow.");
    arbeitnow || DOMAINS.iter().any(|d| on(d)) || host == "news.ycombinator.com" || host == "www.ycombinator.com" || host == "ycombinator.com"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tracking_keeps_identifiers() {
        assert_eq!(
            canonicalize("https://boards.greenhouse.io/acme/jobs/4012345?gh_src=li&utm_source=x#app").unwrap(),
            "https://boards.greenhouse.io/acme/jobs/4012345"
        );
        assert_eq!(
            canonicalize("https://acme.com/careers/?utm_medium=mail&gh_jid=123&ref=x").unwrap(),
            "https://acme.com/careers?gh_jid=123"
        );
        assert_eq!(
            canonicalize("https://acme.com/job?b=2&a=1").unwrap(),
            canonicalize("https://acme.com/job?a=1&b=2").unwrap()
        );
    }

    #[test]
    fn platform_specific_forms() {
        let li = "https://www.linkedin.com/jobs/view/3912345678/";
        assert_eq!(canonicalize("https://www.linkedin.com/comm/jobs/view/3912345678/?trackingId=a&refId=b").unwrap(), li);
        assert_eq!(canonicalize("https://in.linkedin.com/jobs/view/staff-engineer-at-acme-3912345678?trk=x").unwrap(), li);
        assert_eq!(canonicalize("https://www.linkedin.com/jobs/search/?currentJobId=3912345678&keywords=rust").unwrap(), li);
        assert_eq!(
            canonicalize("https://in.indeed.com/rc/clk?jk=abc123&from=ja&tk=x").unwrap(),
            "https://in.indeed.com/viewjob?jk=abc123"
        );
        assert_eq!(
            canonicalize("https://www.naukri.com/job-listings-ai-engineer-acme-bengaluru-5-to-10-years-161026503618?src=jobsearch&xp=1").unwrap(),
            "https://www.naukri.com/job-listings-ai-engineer-acme-bengaluru-5-to-10-years-161026503618"
        );
        assert_eq!(
            canonicalize("https://www.glassdoor.co.in/job-listing/ai-engineer-acme-JV_IC123.htm?jl=1009&pos=101&ao=1").unwrap(),
            "https://www.glassdoor.co.in/job-listing/ai-engineer-acme-JV_IC123.htm?jl=1009"
        );
    }

    #[test]
    fn unwraps_redirects() {
        let target = "https://jobs.lever.co/acme/1b2c-3d?lever-source=mail";
        let safelink = format!(
            "https://eur01.safelinks.protection.outlook.com/?url={}&data=05%7C01&reserved=0",
            url::form_urlencoded::byte_serialize(target.as_bytes()).collect::<String>()
        );
        assert_eq!(canonicalize(&safelink).unwrap(), "https://jobs.lever.co/acme/1b2c-3d");
        let google = format!(
            "https://www.google.com/url?q={}&sa=D",
            url::form_urlencoded::byte_serialize(safelink.as_bytes()).collect::<String>()
        );
        assert_eq!(canonicalize(&google).unwrap(), "https://jobs.lever.co/acme/1b2c-3d");
        // A normal page with a return URL is not a redirect.
        let page = "https://acme.com/jobs/42?url=https%3A%2F%2Fevil.example";
        assert_eq!(unwrap_redirect(page), page);
    }

    #[test]
    fn rejects_non_http() {
        assert_eq!(canonicalize("mailto:jobs@acme.com"), Err(CanonError::Scheme));
        assert!(matches!(canonicalize("not a url"), Err(CanonError::Invalid(_))));
    }

    #[test]
    fn classifies() {
        let c = |u: &str| classify(&canonicalize(u).unwrap());
        assert_eq!(c("https://www.linkedin.com/jobs/view/1/"), Platform::Linkedin);
        assert_eq!(c("https://www.naukri.com/job-listings-x-1"), Platform::Naukri);
        assert_eq!(c("https://www.glassdoor.com/job-listing/x.htm?jl=1"), Platform::Glassdoor);
        assert_eq!(c("https://wellfound.com/jobs/123-ai-engineer"), Platform::Wellfound);
        assert_eq!(c("https://www.indeed.com/viewjob?jk=1"), Platform::Indeed);
        assert_eq!(c("https://job-boards.greenhouse.io/acme/jobs/1"), Platform::Greenhouse);
        assert_eq!(c("https://acme.com/careers?gh_jid=1"), Platform::Greenhouse);
        assert_eq!(c("https://jobs.lever.co/acme/1"), Platform::Lever);
        assert_eq!(c("https://jobs.ashbyhq.com/acme/1"), Platform::Ashby);
        assert_eq!(c("https://acme.wd5.myworkdayjobs.com/en-US/ext/job/x_R1"), Platform::Workday);
        assert_eq!(c("https://careers.acme.com/role/1"), Platform::CareerSite);
        assert_eq!(c("https://acme.com/jobs/staff-engineer"), Platform::CareerSite);
        assert_eq!(c("https://example.com/blog/post"), Platform::Other);
        assert_eq!(c("https://jobicy.com/jobs/152499-backend-engineer-iii"), Platform::JobBoard);
        assert_eq!(c("https://www.arbeitnow.co.uk/jobs/companies/kraken/lead-1"), Platform::JobBoard);
        assert_eq!(c("https://weworkremotely.com/remote-jobs/acme-engineer"), Platform::JobBoard);
        assert_eq!(c("https://remoteok.com/remote-jobs/123"), Platform::JobBoard);
        assert_eq!(c("https://www.ycombinator.com/companies/acme/jobs/1-engineer"), Platform::JobBoard);
    }
}
