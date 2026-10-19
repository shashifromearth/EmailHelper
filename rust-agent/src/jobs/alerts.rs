//! Job alerts from platforms that require a login to apply (LinkedIn,
//! Naukri). Their sites are never opened by the agent: the jobs are read
//! from the alert email itself, then looked up on the employer's own public
//! job board (see `employer.rs`).

use std::sync::OnceLock;

use regex::Regex;

/// One job listed in an alert email.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlertJob {
    pub title: String,
    pub company: Option<String>,
    pub location: Option<String>,
    /// Link to the listing on the platform (for the person, not the agent).
    pub url: Option<String>,
    pub experience: Option<String>,
    pub salary: Option<String>,
    pub work_mode: Option<String>,
    pub skills: Vec<String>,
}

/// Job platforms whose applications happen inside a logged-in account; their
/// terms forbid automated applying, so the agent never browses them.
pub const LOGIN_PLATFORMS: &[&str] = &[
    "linkedin", "naukri", "glassdoor", "indeed", "wellfound", "foundit", "instahyre", "hirist", "iimjobs",
    "monster", "shine",
];

pub fn is_login_platform(source: &str) -> bool {
    LOGIN_PLATFORMS.contains(&source)
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("valid regex"))
}

/// Lines that are badges or chrome, not job data.
fn is_noise(line: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    let l = line.trim();
    l.is_empty()
        || re(
            &R,
            r"(?i)^(top applicant|actively recruiting|easy apply|promoted|premium|fast growing|be an early applicant|early applicant|new|hiring now|apply with resume.*|\d+ (school|company) alumni|\d+ connections?.*|\d+ applicants?|see all jobs|view job.*|your job alert for.*|-{3,}|get app|get the new linkedin.*|manage alerts|messaging|my network|notifications)$",
        )
        .is_match(l)
}

/// `https://www.linkedin.com/comm/jobs/view/123/?tracking…` → canonical URL.
fn linkedin_url(raw: &str) -> Option<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"linkedin\.com/(?:comm/)?jobs/view/(\d+)")
        .captures(raw)
        .map(|c| format!("https://www.linkedin.com/jobs/view/{}/", &c[1]))
}

fn clean(s: &str) -> String {
    s.trim().trim_matches(['\u{a0}', '\t', ' ']).to_string()
}

/// LinkedIn job alert: blocks of title / company / location followed by
/// "View job: <url>", or (as some mail clients render it) "Company · Location"
/// lines under the title.
pub fn parse_linkedin(body: &str) -> Vec<AlertJob> {
    let lines: Vec<String> = body.lines().map(clean).collect();
    let mut out: Vec<AlertJob> = Vec::new();
    if lines.iter().any(|l| l.starts_with("View job:")) {
        let mut block: Vec<String> = Vec::new();
        for l in &lines {
            if let Some(rest) = l.strip_prefix("View job:") {
                let useful: Vec<&String> = block.iter().filter(|x| !is_noise(x)).collect();
                let n = useful.len();
                if n >= 2 {
                    let (title, company, location) = if n >= 3 {
                        (useful[n - 3], Some(useful[n - 2]), Some(useful[n - 1]))
                    } else {
                        (useful[0], Some(useful[1]), None)
                    };
                    out.push(AlertJob {
                        title: title.clone(),
                        company: company.cloned(),
                        location: location.cloned(),
                        url: linkedin_url(rest),
                        ..Default::default()
                    });
                }
                block.clear();
            } else if l.starts_with("---") {
                block.clear();
            } else {
                block.push(l.clone());
            }
        }
        return out;
    }
    // "Company · Location" variant.
    for (i, l) in lines.iter().enumerate() {
        let Some((company, location)) = l.split_once(" · ") else { continue };
        if l.starts_with('©') || company.trim().is_empty() {
            continue;
        }
        let title = lines[..i]
            .iter()
            .rev()
            .map(|x| x.trim())
            .find(|x| !is_noise(x) && !x.eq_ignore_ascii_case(company.trim()));
        if let Some(t) = title {
            out.push(AlertJob {
                title: t.to_string(),
                company: Some(company.trim().to_string()),
                location: Some(location.trim().to_string()),
                ..Default::default()
            });
        }
    }
    out
}

fn is_salary(l: &str) -> bool {
    let x = l.to_lowercase();
    x.contains("disclosed") || x.contains("lacs") || x.contains("lakh") || x.contains('₹') || x.contains("/year") || x.contains("lpa")
}

/// "Hybrid, Bengaluru" / "Hybrid - Bengaluru" → (Some("Hybrid"), "Bengaluru").
fn split_mode(l: &str) -> (Option<String>, String) {
    for m in ["Hybrid", "Remote", "In office", "Work from office", "On-site", "Onsite"] {
        if let Some(rest) = l.strip_prefix(m) {
            let rest = rest.trim_start_matches([',', '-', ' ']).trim();
            if !rest.is_empty() {
                return (Some(m.to_string()), rest.to_string());
            }
        }
    }
    (None, l.to_string())
}

/// "a Banking company", "Foreign MNC", "an IT Services & Consulting Company":
/// placeholders, not employers that can be looked up.
pub fn is_anonymous(company: &str) -> bool {
    let c = company.trim().trim_matches(['\'', '"']).to_lowercase();
    c.is_empty()
        || c.starts_with("a ")
        || c.starts_with("an ")
        || c.starts_with("one of")
        || c.contains("mnc")
        || c.ends_with(" company")
        || c.contains("industry")
        || c.contains("confidential")
        || c.contains("leading ")
}

fn non_empty_after(lines: &[String], from: usize) -> impl Iterator<Item = &String> {
    lines[from..].iter().filter(|l| !l.trim().is_empty())
}

fn is_rating(l: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^\(?\d(\.\d)?\b|^\(\d+ reviews?\)|reviews?\)").is_match(l.trim())
}

/// Naukri alerts: the multi-job digest, the single "invited to apply" mail
/// and the anonymous "early access" roles.
pub fn parse_naukri(body: &str) -> Vec<AlertJob> {
    static LISTING: OnceLock<Regex> = OnceLock::new();
    static EXP: OnceLock<Regex> = OnceLock::new();
    let lines: Vec<String> = body.lines().map(clean).collect();
    let listing = re(&LISTING, r"^\((https?://www\.naukri\.com/(?:jd/)?job-listings-[^)\s]+)\)$");
    let mut out: Vec<AlertJob> = Vec::new();

    // Digest: "Title" / "(job-listings URL)" / company / rating / location / salary.
    for (i, l) in lines.iter().enumerate() {
        let Some(c) = listing.captures(l) else { continue };
        let Some(title) = lines[..i].iter().rev().find(|x| !x.trim().is_empty()) else { continue };
        let after: Vec<&String> = non_empty_after(&lines, i + 1)
            .filter(|x| !is_rating(x))
            .take(3)
            .collect();
        let url = c[1].split('?').next().unwrap_or(&c[1]).to_string();
        out.push(AlertJob {
            title: title.clone(),
            company: after.first().map(|s| s.to_string()).filter(|c| !is_anonymous(c)),
            location: after.get(1).map(|s| s.to_string()),
            salary: after
                .get(2)
                .filter(|s| !s.starts_with("Apply"))
                .map(|s| s.to_string()),
            url: Some(url),
            ..Default::default()
        });
    }
    if !out.is_empty() {
        return out;
    }

    // Single invite: … Title / Location / "5 - 10 years" / salary / mode / skills.
    let exp = re(&EXP, r"(?i)^\d+\s*-\s*\d+\s*(years?|yrs)$");
    if let Some(i) = lines.iter().position(|l| exp.is_match(l)) {
        // Walk up from the experience line: [salary] location title.
        let mut before: Vec<&String> = lines[..i].iter().filter(|x| !x.trim().is_empty()).collect();
        let mut salary_above = None;
        if before.last().is_some_and(|l| is_salary(l)) {
            salary_above = before.pop().cloned();
        }
        let after: Vec<&String> = non_empty_after(&lines, i + 1).take(4).collect();
        if before.len() >= 2 {
            let title = before[before.len() - 2].clone();
            let (mode_prefix, location) = split_mode(&before[before.len() - 1].clone());
            let company = lines
                .iter()
                .find_map(|l| l.strip_prefix("Hiring for "))
                .filter(|c| !c.to_lowercase().contains("industry"))
                .map(|c| c.trim().to_string())
                .or_else(|| {
                    // The line after "you're invited to apply to this job".
                    let k = lines.iter().position(|l| l.contains("invited to apply"))?;
                    non_empty_after(&lines, k + 1)
                        .find(|l| !l.starts_with("Hiring for") && !l.starts_with("Posted by") && !is_rating(l))
                        .filter(|l| **l != title)
                        .cloned()
                })
                .or_else(|| {
                    lines
                        .iter()
                        .find_map(|l| l.strip_prefix("Company:").or_else(|| l.strip_prefix("Company :")))
                        .map(|c| c.trim().to_string())
                })
                .filter(|c| !is_anonymous(c));
            let (salary, mode) = match salary_above {
                Some(sal) => (Some(sal), mode_prefix),
                None => (
                    after.first().map(|s| s.to_string()),
                    mode_prefix.or_else(|| after.get(1).map(|s| s.to_string())),
                ),
            };
            let skills = after
                .iter()
                .skip(2)
                .find(|s| s.contains(',') && !s.starts_with("Apply"))
                .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                .unwrap_or_default();
            out.push(AlertJob {
                title,
                company,
                location: Some(location),
                experience: Some(lines[i].clone()),
                salary,
                work_mode: mode,
                skills,
                url: None,
            });
            return out;
        }
    }

    // Early access: Title / Location / "₹40-60 Lacs P.A." / … / "View opportunity (…)".
    for (i, l) in lines.iter().enumerate() {
        if !l.starts_with("View opportunity") {
            continue;
        }
        let before: Vec<&String> = lines[..i]
            .iter()
            .filter(|x| !x.trim().is_empty())
            .rev()
            .take(6)
            .collect();
        // before (reversed): company hint, "Hiring for one of these", salary, location, title
        if let Some(pos) = before.iter().position(|x| x.starts_with("Hiring for")) {
            let rest: Vec<&&String> = before.iter().skip(pos + 1).collect();
            if rest.len() >= 3 {
                out.push(AlertJob {
                    title: rest[2].to_string(),
                    location: Some(rest[1].to_string()),
                    salary: Some(rest[0].to_string()),
                    company: None, // anonymous ("MNC in Software Product Domain")
                    ..Default::default()
                });
            }
        }
    }
    out
}

/// Jobs listed in an alert email from `source`.
pub fn parse(source: &str, body: &str) -> Vec<AlertJob> {
    let mut jobs = match source {
        "linkedin" => parse_linkedin(body),
        "naukri" => parse_naukri(body),
        _ => Vec::new(),
    };
    jobs.retain(|j| !j.title.trim().is_empty() && j.title.chars().count() <= 150);
    for j in &mut jobs {
        // Page chrome, not employers ("LinkedIn", "Sign Up").
        if j.company.as_deref().is_some_and(|c| {
            let c = c.trim().to_lowercase();
            LOGIN_PLATFORMS.contains(&c.as_str()) || ["sign up", "sign in", "join now", "log in"].contains(&c.as_str())
        }) {
            j.company = None;
        }
    }
    let mut seen = std::collections::HashSet::new();
    jobs.retain(|j| {
        seen.insert(format!(
            "{}|{}",
            j.title.to_lowercase(),
            j.company.as_deref().unwrap_or("").to_lowercase()
        ))
    });
    jobs
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINKEDIN: &str = "Your job alert for Technical Lead in United States            \n\
Lead AI Engineer\nNetrolynx AI\nUnited States\nTop applicant\n\
View job: https://www.linkedin.com/comm/jobs/view/4480916162/?trackingId=abc&refId=x\n\n\
---------------------------------------------------------\n  \n            \n\
AI Engineering Lead\nBeni Health\nUnited States\nTop applicant\n\
View job: https://www.linkedin.com/comm/jobs/view/4479947091/?trackingId=def\n";

    #[test]
    fn linkedin_view_job_blocks() {
        let j = parse("linkedin", LINKEDIN);
        assert_eq!(j.len(), 2);
        assert_eq!(j[0].title, "Lead AI Engineer");
        assert_eq!(j[0].company.as_deref(), Some("Netrolynx AI"));
        assert_eq!(j[0].location.as_deref(), Some("United States"));
        assert_eq!(j[0].url.as_deref(), Some("https://www.linkedin.com/jobs/view/4480916162/"));
        assert_eq!(j[1].title, "AI Engineering Lead");
    }

    #[test]
    fn linkedin_company_dot_location_variant() {
        let body = "LinkedIn \t\nMessaging \tMy Network \tNotifications\nYour job alert for Technical Lead\n\
Accenture services Pvt Ltd \t\nTechnology Architect\n\nAccenture services Pvt Ltd · Gurugram\n\
Lovely Professional University \t\n\n2 school alumni\nAntino \t\nAntino - Lead AI/ML Engineer - Generative AI\n\n\
Antino · Gurugram\n\t\n\nActively recruiting\nSee all jobs\n\
© 2026 LinkedIn Corporation, 1000 West Maude Avenue, Sunnyvale, CA 94085.";
        let j = parse("linkedin", body);
        assert_eq!(j.len(), 2, "{j:?}");
        assert_eq!(j[0].title, "Technology Architect");
        assert_eq!(j[0].company.as_deref(), Some("Accenture services Pvt Ltd"));
        assert_eq!(j[0].location.as_deref(), Some("Gurugram"));
        assert_eq!(j[1].title, "Antino - Lead AI/ML Engineer - Generative AI");
    }

    #[test]
    fn naukri_digest() {
        let body = "Principal Data Engineer\n\nGet App (https://www.naukri.com/durl/nEZb)\n\n\
Seize all job opportunities of this week!\n\nPrincipal Data Engineer\n\
(https://www.naukri.com/jd/job-listings-principal-data-engineer-atlassian-bengaluru-12-to-17-years-161026503618?xp=1&src=ja)\n\n\
Atlassian\n\n3.0 (https://www.ambitionbox.com/reviews/atlassian-reviews?utm=x)\n\nBengaluru\n\nNot disclosed\n\n\
Apply (https://www.naukri.com/jd/job-listings-principal-data-engineer-atlassian-bengaluru-12-to-17-years-161026503618?xp=1)\n\n\
Senior Software Engineer II\n\
(https://www.naukri.com/jd/job-listings-senior-software-engineer-ii-american-express-india-gurugram-10-to-15-years-151026503009?xp=2)\n\n\
AMERICAN EXPRESS\n\n4.1 (https://www.ambitionbox.com/reviews/american-express-reviews)\n\nGurugram\n\nNot disclosed\n";
        let j = parse("naukri", body);
        assert_eq!(j.len(), 2, "{j:?}");
        assert_eq!(j[0].title, "Principal Data Engineer");
        assert_eq!(j[0].company.as_deref(), Some("Atlassian"));
        assert_eq!(j[0].location.as_deref(), Some("Bengaluru"));
        assert_eq!(
            j[0].url.as_deref(),
            Some("https://www.naukri.com/jd/job-listings-principal-data-engineer-atlassian-bengaluru-12-to-17-years-161026503618")
        );
        assert_eq!(j[1].company.as_deref(), Some("AMERICAN EXPRESS"));
        assert_eq!(j[1].location.as_deref(), Some("Gurugram"));
    }

    #[test]
    fn naukri_invite() {
        let body = "Job invite from recruiter. Apply now!\n\nGet app\n\n(https://www.naukri.com/durl/nEZb)\n\n\
shashi kumar, you're invited to apply to this job\n\nHTC Global Services\n\n3.4\n\n(2116 Reviews) (https://www.ambitionbox.com/x)\n\n\
Artificial Intelligence Engineer\n\nBengaluru, Bangalore Rural\n\n5 - 10 years\n\n20.0 lacs - 30.0 lacs\n\nHybrid\n\n\
Python, llm, rag, gen AI, SQL\n\nApply now (http://my.naukri.com/AL/x)\n\nJob description\n";
        let j = parse("naukri", body);
        assert_eq!(j.len(), 1, "{j:?}");
        assert_eq!(j[0].title, "Artificial Intelligence Engineer");
        assert_eq!(j[0].company.as_deref(), Some("HTC Global Services"));
        assert_eq!(j[0].location.as_deref(), Some("Bengaluru, Bangalore Rural"));
        assert_eq!(j[0].experience.as_deref(), Some("5 - 10 years"));
        assert_eq!(j[0].work_mode.as_deref(), Some("Hybrid"));
        assert_eq!(j[0].skills, vec!["Python", "llm", "rag", "gen AI", "SQL"]);

        let hiring_for = body.replace("HTC Global Services\n\n3.4", "Hiring for Target Corporation\n\nPosted by Naukri E Hire");
        assert_eq!(parse("naukri", &hiring_for)[0].company.as_deref(), Some("Target Corporation"));
    }

    #[test]
    fn naukri_toptier_and_concierge_layouts() {
        let toptier = "Hi Shashi,
Recruiter from Aspiroite Consulting sees you as a great fit, apply now
HR Recruiter
has reached out to you
TE Connectivity
4.1
Posted by Aspiroite Consulting.
AI Security Architect (Principal Information Security)
Hybrid, Bengaluru
Not disclosed
10-20 years
Apply now (https://www.naukri.com/toptier/inbox/invites/mail?x=1)
Job description
Company: TE Connectivity
";
        let j = parse("naukri", toptier);
        assert_eq!(j[0].title, "AI Security Architect (Principal Information Security)");
        assert_eq!(j[0].location.as_deref(), Some("Bengaluru"));
        assert_eq!(j[0].work_mode.as_deref(), Some("Hybrid"));
        assert_eq!(j[0].salary.as_deref(), Some("Not disclosed"));
        assert_eq!(j[0].company.as_deref(), Some("TE Connectivity"));

        let concierge = "Hi Shashi Kumar,
I'm Samarth, your career concierge.
Hiring for a Banking company
Posted by Mancer Consulting Services.
Principal Platform Engineer (SDE4)
Hybrid - Bengaluru
₹50L - ₹80L/year
9-13 Yrs
View & apply (https://dl.naukri.com?data=x)
";
        let j = parse("naukri", concierge);
        assert_eq!(j[0].title, "Principal Platform Engineer (SDE4)");
        assert_eq!(j[0].location.as_deref(), Some("Bengaluru"));
        assert_eq!(j[0].company, None, "a Banking company is anonymous");
    }

    #[test]
    fn naukri_early_access_is_anonymous() {
        let body = "Hi Shashi Kumar,\n\nJava Technical Architect\n\nPune\n\n₹44-55 Lacs P.A.\n\nHiring for one of these\n\n\
'MNC in Software Product Domain'\n\nView opportunity (https://dl.naukri.com?data=x)\n";
        let j = parse("naukri", body);
        assert_eq!(j.len(), 1, "{j:?}");
        assert_eq!(j[0].title, "Java Technical Architect");
        assert_eq!(j[0].location.as_deref(), Some("Pune"));
        assert_eq!(j[0].company, None);
    }
}
