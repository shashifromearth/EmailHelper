//! Resume matching and the application decision engine.
//!
//! Score (0–100) = skills 35 + role/title 20 + experience 15 + salary 15 +
//! location/remote 15. Hard rules (blacklist, salary floor, relocation, visa,
//! no core-skill overlap) force a skip regardless of score.

use serde::Serialize;
use serde_json::{json, Value};

use crate::db::jobs::JobDetails;
use crate::jobs::extract::contains_word;
use crate::knowledge::Profile;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Apply,
    Review,
    Skip,
}

impl Decision {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Review => "review",
            Self::Skip => "skip",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchResult {
    pub score: u8,
    pub breakdown: Value,
    pub matched_skills: Vec<String>,
    pub missing_skills: Vec<String>,
    /// Hard-rule violations (any → skip).
    pub skip_reasons: Vec<String>,
    /// Soft notes shown to the user.
    pub notes: Vec<String>,
    pub decision: Decision,
}

impl MatchResult {
    pub fn reasons(&self) -> Vec<String> {
        let mut r = self.skip_reasons.clone();
        r.extend(self.notes.iter().cloned());
        r
    }
}

/// Title words that put a role outside an engineering/architecture career.
const OFF_TARGET: &[&str] = &[
    "support", "sales", "account executive", "account manager", "marketing", "recruiter", "talent acquisition",
    "customer success", "product specialist", "designer", "salesforce", "sap", "hcm", "accountant", "legal",
    "counsel", "nurse", "teacher", "tutor", "business development", "pre-sales", "presales",
];
/// Title words for entry-level roles.
const JUNIOR: &[&str] = &["intern", "internship", "trainee", "junior", "jr", "graduate", "fresher", "entry level", "entry-level", "apprentice"];
/// Languages a posting may require.
const LANGUAGES: &[&str] = &[
    "korean", "japanese", "mandarin", "chinese", "cantonese", "german", "french", "spanish", "portuguese",
    "italian", "dutch", "arabic", "hebrew", "russian", "polish", "turkish", "vietnamese", "thai", "bahasa",
    "indonesian", "swedish", "danish", "norwegian", "finnish", "czech",
];
/// Phrases saying the employer sponsors work visas.
const SPONSORS: &[&str] = &[
    "visa sponsorship available", "visa sponsorship is available", "will sponsor", "we sponsor", "sponsorship provided",
    "provide visa sponsorship", "offer visa sponsorship", "sponsor work visas", "sponsorship and relocation",
    "sponsor your visa", "visa and relocation support", "relocation assistance", "relocation support", "relocation package",
];

/// A language the posting requires that isn't one of `known`.
fn required_language(text: &str, known: &[String]) -> Option<&'static str> {
    LANGUAGES.iter().copied().find(|l| {
        !known.iter().any(|k| k.eq_ignore_ascii_case(l))
            && [
                format!("{l} speaking"),
                format!("{l}-speaking"),
                format!("{l} speaker"),
                format!("fluent in {l}"),
                format!("fluency in {l}"),
                format!("{l} fluency"),
                format!("{l} (native"),
                format!("native {l}"),
                format!("business-level {l}"),
                format!("{l} language skills"),
                format!("proficiency in {l}"),
            ]
            .iter()
            .any(|p| text.contains(p.as_str()))
    })
}

fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| {
            t.len() > 1 && !["and", "the", "of", "for", "sr", "jr", "ii", "iii"].contains(t)
        })
        .map(str::to_string)
        .collect()
}

pub fn score(
    job: &JobDetails,
    p: &Profile,
    auto_threshold: u8,
    review_threshold: u8,
) -> MatchResult {
    let mut skip = Vec::new();
    let mut notes = Vec::new();
    let text = format!(
        "{} {} {}",
        job.title,
        job.skills.join(" "),
        job.description.as_deref().unwrap_or("")
    )
    .to_lowercase();

    // ---- skills (35)
    let core: Vec<&String> = p.skills.core.iter().collect();
    let all = p.all_skills();
    let matched: Vec<String> = all
        .iter()
        .filter(|s| contains_word(&text, &s.to_lowercase()))
        .cloned()
        .collect();
    let core_hits = core
        .iter()
        .filter(|s| contains_word(&text, &s.to_lowercase()))
        .count();
    let missing: Vec<String> = job
        .skills
        .iter()
        .filter(|s| !all.iter().any(|a| a.eq_ignore_ascii_case(s)))
        .cloned()
        .collect();
    let alert_only = job.skills.is_empty()
        && job.description.as_deref().unwrap_or("").trim().is_empty();
    let skills_score = if job.skills.is_empty() && core.is_empty() {
        17.0
    } else if alert_only {
        // Only a title is known (platform alert); don't guess skills from it.
        notes.push("skills unknown (title only)".into());
        22.0
    } else {
        let required = job.skills.len().max(1) as f64;
        let coverage =
            ((job.skills.len() - missing.len().min(job.skills.len())) as f64 / required).min(1.0);
        let core_ratio = if core.is_empty() {
            0.5
        } else {
            (core_hits as f64 / core.len().min(6) as f64).min(1.0)
        };
        if job.skills.is_empty() {
            35.0 * core_ratio
        } else {
            35.0 * (0.6 * coverage + 0.4 * core_ratio)
        }
    };
    if !job.skills.is_empty() && core_hits == 0 && matched.is_empty() {
        skip.push("missing core skills".to_string());
    }

    // ---- role/title (20)
    let title_tokens = tokens(&job.title);
    let title_l = format!(" {} ", job.title.to_lowercase().replace(['-', ',', '/', '(', ')', '|'], " "));
    if let Some(w) = OFF_TARGET.iter().find(|w| title_l.contains(&format!(" {w} "))) {
        skip.push(format!("role outside your target roles ({w})"));
    }
    let years = p.resume.total_experience_years;
    let associate_ic = title_tokens.first().is_some_and(|t| t == "associate")
        && title_tokens.iter().any(|t| ["engineer", "developer", "consultant", "analyst"].contains(&t.as_str()))
        && !title_tokens.iter().any(|t| ["manager", "director", "principal", "architect"].contains(&t.as_str()));
    if years >= 5.0 {
        if let Some(w) = JUNIOR.iter().find(|w| title_l.contains(&format!(" {w} "))) {
            skip.push(format!("too junior for {years} years of experience ({w})"));
        } else if associate_ic {
            skip.push(format!("too junior for {years} years of experience (associate)"));
        }
    }
    let role_score = if p.preferences.target_roles.is_empty() {
        10.0
    } else {
        p.preferences
            .target_roles
            .iter()
            .map(|r| {
                let rt = tokens(r);
                if rt.is_empty() {
                    return 0.0;
                }
                let hit = rt.iter().filter(|t| title_tokens.contains(t)).count();
                let ratio = hit as f64 / rt.len() as f64;
                // The role noun ("Architect", "Engineer") must match, or it's a
                // different job that shares a buzzword.
                let head = rt.last().map(String::as_str).unwrap_or("");
                let head_hit = title_tokens.iter().any(|t| {
                    t == head || (head == "engineer" && t == "developer") || (head == "architect" && t == "architecture")
                });
                if head_hit {
                    ratio
                } else {
                    ratio * 0.4
                }
            })
            .fold(0.0_f64, f64::max)
            * 20.0
    };
    if !p.preferences.target_roles.is_empty() && role_score < 8.0 {
        notes.push("title is far from your target roles".into());
    }

    // ---- experience (15)
    let exp_score = match (job.experience_years, p.resume.total_experience_years) {
        (Some(req), have) if have > 0.0 => {
            if have >= req {
                15.0
            } else if have + 2.0 >= req {
                9.0
            } else {
                notes.push(format!("requires {req} years; profile has {have}"));
                3.0
            }
        }
        _ => 10.0,
    };

    // ---- salary (15)
    let salary_score = match (job.salary_max, p.salary.minimum_annual) {
        (Some(max), Some(min)) => {
            let same_currency = match (&job.salary_currency, p.salary.currency.as_str()) {
                (Some(c), pc) if !pc.is_empty() => c.eq_ignore_ascii_case(pc),
                _ => true,
            };
            if !same_currency {
                notes.push("salary currency differs from preference".into());
                8.0
            } else if max >= min {
                match p.salary.expected_annual {
                    Some(exp) if max >= exp => 15.0,
                    _ => 12.0,
                }
            } else {
                skip.push(format!("salary mismatch (max {max} < minimum {min})"));
                0.0
            }
        }
        _ => 9.0,
    };

    // ---- location / remote (15)
    let pref_remote = p.preferences.remote.to_lowercase();
    let job_remote = job.remote.clone().unwrap_or_else(|| "unknown".into());
    let loc = job.location.clone().unwrap_or_default().to_lowercase();
    let preferred_loc = p
        .preferences
        .preferred_locations
        .iter()
        .any(|l| !l.is_empty() && loc.contains(&l.to_lowercase()));
    let loc_score = match (pref_remote.as_str(), job_remote.as_str()) {
        (_, "remote") => 15.0,
        ("remote_only", "onsite") | ("remote_only", "hybrid") => {
            if preferred_loc {
                8.0
            } else if p.identity.willing_to_relocate == Some(false)
                || p.identity.willing_to_relocate.is_none()
            {
                skip.push("relocation required".to_string());
                0.0
            } else {
                4.0
            }
        }
        (_, "onsite") | (_, "hybrid") => {
            if preferred_loc || loc.is_empty() {
                12.0
            } else if p.identity.willing_to_relocate == Some(false) {
                skip.push("relocation required".to_string());
                0.0
            } else {
                6.0
            }
        }
        _ => {
            if preferred_loc {
                12.0
            } else {
                8.0
            }
        }
    };

    // ---- hard rules
    if let Some(company) = &job.company {
        if p.preferences
            .blacklisted_companies
            .iter()
            .any(|b| !b.is_empty() && company.eq_ignore_ascii_case(b.trim()))
        {
            skip.push(format!("blacklisted company ({company})"));
        }
    }
    let needs_visa =
        p.preferences.visa_sponsorship_required || p.identity.requires_sponsorship == Some(true);
    // Visa rules only matter for jobs outside the countries the user can
    // already work in ("must be authorized to work" is fine in India for an
    // Indian citizen).
    let authorized = p.authorized_countries();
    let where_text =
        format!("{} {}", job.location.as_deref().unwrap_or(""), job.title).to_lowercase();
    let in_authorized = crate::knowledge::profile::countries_in(&where_text)
        .iter()
        .any(|c| authorized.contains(c))
        || p.preferences
            .preferred_locations
            .iter()
            .any(|l| !l.is_empty() && where_text.contains(&l.to_lowercase()));
    let location_known = !job.location.as_deref().unwrap_or("").trim().is_empty();
    // Language requirements (English and Hindi assumed).
    let known_langs = vec!["english".to_string(), "hindi".to_string()];
    if let Some(l) = required_language(&format!("{} {}", job.title.to_lowercase(), text), &known_langs) {
        skip.push(format!("requires {l}"));
    }
    // A job in a country the user can't work in, with no word on sponsorship:
    // never applied automatically.
    let job_countries = crate::knowledge::profile::countries_in(&where_text);
    let abroad = needs_visa && !job_countries.is_empty() && !in_authorized;
    let sponsors = SPONSORS.iter().any(|w| text.contains(w));
    if needs_visa
        && location_known
        && !in_authorized
        && [
            "no visa sponsorship",
            "unable to sponsor",
            "not able to sponsor",
            "cannot sponsor",
            "no sponsorship",
            "must be authorized to work",
        ]
        .iter()
        .any(|w| text.contains(w))
    {
        skip.push("visa requirements".to_string());
    }

    let total = (skills_score + role_score + exp_score + salary_score + loc_score)
        .round()
        .clamp(0.0, 100.0) as u8;
    let decision = if !skip.is_empty() || total < review_threshold {
        Decision::Skip
    } else if total >= auto_threshold && !(abroad && !sponsors) {
        Decision::Apply
    } else {
        Decision::Review
    };
    if abroad && !sponsors && decision != Decision::Skip {
        notes.push(format!(
            "in {}; visa sponsorship not mentioned, so it needs your OK",
            job_countries.join(", ")
        ));
    }
    if decision == Decision::Skip && skip.is_empty() {
        notes.push(format!(
            "score {total} below review threshold {review_threshold}"
        ));
    }
    MatchResult {
        score: total,
        breakdown: json!({
            "skills": skills_score.round(),
            "role": role_score.round(),
            "experience": exp_score,
            "salary": salary_score,
            "location": loc_score,
        }),
        matched_skills: matched,
        missing_skills: missing,
        skip_reasons: skip,
        notes,
        decision,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::profile::*;

    fn profile() -> Profile {
        Profile {
            identity: Identity {
                full_name: "Asha Rao".into(),
                willing_to_relocate: Some(false),
                ..Default::default()
            },
            resume: Resume {
                total_experience_years: 12.0,
                ..Default::default()
            },
            skills: Skills {
                core: vec![
                    "Rust".into(),
                    "Python".into(),
                    "LLMs".into(),
                    "RAG".into(),
                    "Kubernetes".into(),
                ],
                secondary: vec!["Go".into()],
            },
            salary: SalaryPreferences {
                currency: "INR".into(),
                minimum_annual: Some(4_000_000.0),
                expected_annual: Some(6_000_000.0),
                ..Default::default()
            },
            preferences: Preferences {
                remote: "remote_only".into(),
                preferred_locations: vec!["Bengaluru".into()],
                target_roles: vec!["Staff AI Engineer".into(), "AI Architect".into()],
                blacklisted_companies: vec!["Evil Corp".into()],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn job() -> JobDetails {
        JobDetails {
            title: "Staff AI Engineer".into(),
            company: Some("Acme".into()),
            remote: Some("remote".into()),
            salary_max: Some(6_500_000.0),
            salary_currency: Some("INR".into()),
            experience_years: Some(8.0),
            skills: vec!["Rust".into(), "Python".into(), "RAG".into(), "LLMs".into()],
            description: Some("Build RAG systems with Rust, Python and Kubernetes".into()),
            ..Default::default()
        }
    }

    #[test]
    fn strong_match_applies() {
        let m = score(&job(), &profile(), 80, 60);
        assert!(m.score >= 80, "{m:?}");
        assert_eq!(m.decision, Decision::Apply);
    }

    #[test]
    fn blacklist_and_salary_skip() {
        let mut j = job();
        j.company = Some("evil corp".into());
        let m = score(&j, &profile(), 80, 60);
        assert_eq!(m.decision, Decision::Skip);
        assert!(m.skip_reasons.iter().any(|r| r.contains("blacklisted")));

        let mut j = job();
        j.salary_max = Some(2_000_000.0);
        let m = score(&j, &profile(), 80, 60);
        assert!(m.skip_reasons.iter().any(|r| r.contains("salary mismatch")));
    }

    #[test]
    fn relocation_and_skills_skip() {
        let mut j = job();
        j.remote = Some("onsite".into());
        j.location = Some("Berlin".into());
        let m = score(&j, &profile(), 80, 60);
        assert!(m.skip_reasons.iter().any(|r| r.contains("relocation")));

        let mut j = job();
        j.title = "Senior Accountant".into();
        j.skills = vec!["SAP".into(), "Excel".into()];
        j.description = Some("Accounting role".into());
        let m = score(&j, &profile(), 80, 60);
        assert_eq!(m.decision, Decision::Skip);
    }

    #[test]
    fn middling_match_reviews() {
        let mut j = job();
        j.title = "Senior Backend Engineer".into();
        j.skills = vec!["Rust".into(), "Kafka".into(), "Java".into()];
        j.description = Some("Rust and Java services".into());
        j.salary_max = None;
        let m = score(&j, &profile(), 80, 60);
        assert!(m.score < 80, "{m:?}");
    }

    #[test]
    fn off_target_junior_and_language_roles_skip() {
        let p = profile();
        for (title, why) in [
            ("AI Support Engineer - Singapore", "role outside"),
            ("Lead Salesforce Engineer", "role outside"),
            ("Junior AI Engineer", "too junior"),
            ("AI Engineer Intern", "too junior"),
            ("Associate Software Engineer", "too junior"),
        ] {
            let mut j = job();
            j.title = title.into();
            let m = score(&j, &p, 80, 60);
            assert_eq!(m.decision, Decision::Skip, "{title}: {m:?}");
            assert!(m.skip_reasons.iter().any(|r| r.contains(why)), "{title}: {m:?}");
        }
        let mut j = job();
        j.description = Some("Rust Python RAG Kubernetes. Korean speaking is required.".into());
        let m = score(&j, &p, 80, 60);
        assert!(m.skip_reasons.iter().any(|r| r.contains("korean")), "{m:?}");
        // "Associate Manager" is not an entry-level role.
        let mut j = job();
        j.title = "Associate Director, AI Engineering".into();
        assert!(!score(&j, &p, 80, 60).skip_reasons.iter().any(|r| r.contains("junior")));
    }

    #[test]
    fn abroad_without_sponsorship_needs_review() {
        let mut p = profile();
        p.identity.requires_sponsorship = Some(true);
        p.identity.willing_to_relocate = Some(true);
        let mut j = job();
        j.location = Some("Tokyo, Japan".into());
        let m = score(&j, &p, 80, 60);
        assert_eq!(m.decision, Decision::Review, "{m:?}");
        assert!(m.notes.iter().any(|n| n.contains("sponsorship")));
        j.description = Some("Rust Python RAG Kubernetes. Visa sponsorship available.".into());
        assert_eq!(score(&j, &p, 80, 60).decision, Decision::Apply);
        j.location = Some("Bengaluru".into());
        j.description = Some("Rust Python RAG Kubernetes".into());
        assert_eq!(score(&j, &p, 80, 60).decision, Decision::Apply, "India needs no sponsorship");
    }

    #[test]
    fn alert_only_jobs_are_neutral_on_skills() {
        let j = JobDetails {
            title: "Lead AI Engineer".into(),
            company: Some("Acme".into()),
            location: Some("Bengaluru".into()),
            ..Default::default()
        };
        let m = score(&j, &profile(), 80, 60);
        assert!(m.notes.iter().any(|n| n.contains("skills unknown")));
        assert_ne!(m.decision, Decision::Skip, "{m:?}");
    }

    #[test]
    fn visa_rule() {
        let mut p = profile();
        p.preferences.visa_sponsorship_required = true;
        p.identity.work_authorization = "India".into();
        let mut j = job();
        j.location = Some("London, UK".into());
        j.description = Some("Rust Python RAG. We are unable to sponsor visas.".into());
        let m = score(&j, &p, 80, 60);
        assert!(m.skip_reasons.iter().any(|r| r.contains("visa")));
        // Same wording for a job in India is not a problem for an Indian citizen.
        j.location = Some("Bengaluru, India".into());
        j.description =
            Some("Rust Python RAG. Candidates must be authorized to work in India.".into());
        let m = score(&j, &p, 80, 60);
        assert!(!m.skip_reasons.iter().any(|r| r.contains("visa")), "{m:?}");
    }
}
