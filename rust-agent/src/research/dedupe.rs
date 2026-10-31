//! The same job turns up on several sources (the employer's board, Adzuna,
//! Jooble, LinkedIn via search). Two postings are one job when they share a
//! canonical URL, or the same company and title.

use std::collections::HashSet;

use crate::research::sources::RawJob;

/// Lower case, letters/digits (and `#`, `+`, `.` for C#, C++, .NET) only,
/// without bracketed notes such as "(Remote)" or "[Contract]".
pub fn norm_title(t: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in t.to_lowercase().chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            c if c.is_alphanumeric() || matches!(c, '#' | '+' | '.') => out.push(c),
            _ => out.push(' '),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Company name without punctuation or legal suffixes ("Acme Pvt. Ltd." → "acme").
pub fn norm_company(c: &str) -> String {
    const SUFFIXES: &[&str] = &[
        "inc", "llc", "ltd", "limited", "pvt", "private", "gmbh", "corp", "corporation", "co", "plc", "ag", "bv", "sa",
        "llp", "pte",
    ];
    c.to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|w| !w.is_empty() && !SUFFIXES.contains(w))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The link to store and hand on: tracking parameters and fragment removed,
/// LinkedIn reduced to `/jobs/view/<id>/`, letter case kept.
pub fn canonical_link(u: &str) -> String {
    if let Some(site) = crate::research::search_apis::platform(u) {
        return crate::research::search_apis::posting_url(site, u);
    }
    if let Some(l) = crate::jobs::detect::classify_url(u) {
        return l.url;
    }
    let Ok(mut parsed) = url::Url::parse(u.trim()) else {
        return u.trim().to_string();
    };
    let kept: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(k, _)| {
            let k = k.to_ascii_lowercase();
            !(k.starts_with("utm_") || matches!(k.as_str(), "ref" | "src" | "source" | "trk" | "trackingid" | "refid" | "gclid" | "fbclid"))
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        parsed.set_query(None);
    } else {
        parsed.query_pairs_mut().clear().extend_pairs(kept);
    }
    parsed.set_fragment(None);
    parsed.to_string()
}

/// URL without scheme, `www.`, tracking parameters or trailing slash.
pub fn norm_url(u: &str) -> String {
    let u = canonical_link(u);
    let c = crate::db::jobs::canonical_url(&u);
    let c = c.split_once("://").map(|(_, rest)| rest.to_string()).unwrap_or(c);
    c.strip_prefix("www.").map(str::to_string).unwrap_or(c)
}

/// Keys identifying a job: its URL, and company + title when the company is known.
pub fn keys(j: &RawJob) -> Vec<String> {
    let mut k = Vec::new();
    if let Some(u) = j.details.url.as_deref().filter(|u| !u.trim().is_empty()) {
        k.push(format!("url:{}", norm_url(u)));
    }
    let company = j.details.company.as_deref().map(norm_company).unwrap_or_default();
    let title = norm_title(&j.details.title);
    if !company.is_empty() && !title.is_empty() {
        k.push(format!("job:{company}|{title}"));
    }
    k
}

/// Remembers the jobs seen so far in one run.
#[derive(Default)]
pub struct Seen(HashSet<String>);

impl Seen {
    /// True the first time a job is seen (none of its keys known yet).
    pub fn first(&mut self, j: &RawJob) -> bool {
        let keys = keys(j);
        let dup = keys.iter().any(|k| self.0.contains(k));
        self.0.extend(keys);
        !dup
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::jobs::JobDetails;

    fn raw(title: &str, company: Option<&str>, url: &str) -> RawJob {
        RawJob::new(
            "x",
            None,
            JobDetails {
                title: title.into(),
                company: company.map(str::to_string),
                url: Some(url.into()),
                ..Default::default()
            },
        )
    }

    #[test]
    fn same_job_on_several_sources() {
        let mut seen = Seen::default();
        assert!(seen.first(&raw("Senior Python Engineer", Some("Acme Pvt. Ltd."), "https://boards.greenhouse.io/acme/jobs/1")));
        assert!(!seen.first(&raw("Senior Python Engineer (Remote)", Some("ACME"), "https://www.adzuna.in/land/ad/9")), "company + title");
        assert!(!seen.first(&raw("Other", None, "http://boards.greenhouse.io/acme/jobs/1/?utm_source=x")), "same URL");
        assert!(seen.first(&raw("Senior Python Engineer", None, "https://example.com/jobs/2")), "no company: URL only");
        assert!(seen.first(&raw("Senior C# Engineer", Some("Acme"), "https://example.com/jobs/3")));
    }

    #[test]
    fn normalisation() {
        assert_eq!(norm_title("Sr. .NET / C# Developer [Contract]"), "sr. .net c# developer");
        assert_eq!(norm_company("Initech GmbH"), "initech");
        assert_eq!(norm_url("https://www.linkedin.com/jobs/view/123456/?trk=a"), "linkedin.com/jobs/view/123456");
        assert_eq!(
            canonical_link("https://www.Adzuna.in/land/ad/4711?se=AbC&utm_source=x#top"),
            "https://www.adzuna.in/land/ad/4711?se=AbC",
            "case of the path and query kept"
        );
        assert_eq!(
            canonical_link("https://in.linkedin.com/jobs/view/python-architect-at-acme-4012345678?trk=x"),
            "https://www.linkedin.com/jobs/view/4012345678/"
        );
    }
}
