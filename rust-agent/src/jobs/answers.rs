//! Application form answering: profile mapping → remembered answers → RAG+LLM
//! → human. Sensitive questions are never guessed.

use serde::Deserialize;
use serde_json::Value;

use crate::db::jobs as jdb;
use crate::db::Db;
use crate::error::AppResult;
use crate::jobs::browser::Field;
use crate::knowledge::{KnowledgeBase, Profile};
use crate::llm::{LlmRequest, LlmRouter, LlmTask};
use crate::prompts;

#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Value(Value),
    /// The resume file input.
    ResumeFile,
    /// Cover-letter text field (filled with the generated letter).
    CoverLetter,
    /// Leave empty (optional field we choose not to answer).
    Skip,
    /// A human must answer.
    Ask {
        reason: String,
    },
}

fn label_has(label: &str, words: &[&str]) -> bool {
    words.iter().any(|w| label.contains(w))
}

fn s(v: &str) -> Option<Answer> {
    (!v.trim().is_empty()).then(|| Answer::Value(Value::String(v.trim().to_string())))
}

/// Pick the option that best matches `want` (case-insensitive, prefix/contains).
pub fn match_option(options: &[String], want: &str) -> Option<String> {
    let w = want.trim().to_lowercase();
    if w.is_empty() {
        return None;
    }
    options
        .iter()
        .find(|o| o.trim().to_lowercase() == w)
        .or_else(|| options.iter().find(|o| o.to_lowercase().starts_with(&w)))
        .or_else(|| options.iter().find(|o| o.to_lowercase().contains(&w)))
        .or_else(|| {
            options
                .iter()
                .find(|o| !o.trim().is_empty() && w.contains(&o.trim().to_lowercase()))
        })
        .cloned()
}

fn yes_no(f: &Field, yes: bool) -> Answer {
    let want = if yes { "yes" } else { "no" };
    if f.options.is_empty() {
        return Answer::Value(Value::String(if yes { "Yes" } else { "No" }.into()));
    }
    match match_option(&f.options, want) {
        Some(o) => Answer::Value(Value::String(o)),
        None => Answer::Ask {
            reason: "no matching yes/no option".into(),
        },
    }
}

fn choose(f: &Field, value: &str) -> Option<Answer> {
    if value.trim().is_empty() {
        return None;
    }
    if f.options.is_empty() {
        return s(value);
    }
    match_option(&f.options, value).map(|o| Answer::Value(Value::String(o)))
}

const EEO: &[&str] = &[
    "gender",
    "race",
    "ethnicity",
    "veteran",
    "disability",
    "sexual orientation",
    "pronoun",
    "hispanic",
    "latino",
];

/// Deterministic mapping from field label to profile data.
pub fn from_profile(f: &Field, p: &Profile) -> Option<Answer> {
    let l = f.label.to_lowercase();
    let id = f.id.to_lowercase();
    let both = format!("{l} {id}");
    let ident = &p.identity;

    if f.kind == "file" {
        if label_has(&both, &["resume", "cv", "curriculum"]) {
            return Some(Answer::ResumeFile);
        }
        if label_has(&both, &["cover"]) {
            // We provide cover letters as text; an optional file slot is skipped.
            return Some(if f.required {
                Answer::Ask {
                    reason: "cover letter file upload required".into(),
                }
            } else {
                Answer::Skip
            });
        }
        return Some(if f.required {
            Answer::Ask {
                reason: format!("unknown file upload \"{}\"", f.label),
            }
        } else {
            Answer::Skip
        });
    }
    if label_has(&both, &["cover letter", "cover_letter", "coverletter"]) {
        return Some(Answer::CoverLetter);
    }
    if label_has(&l, EEO) {
        if let Some(o) = f.options.iter().find(|o| {
            let o = o.to_lowercase();
            o.contains("decline")
                || o.contains("prefer not")
                || o.contains("don't wish")
                || o.contains("do not wish")
                || o.contains("not to answer")
        }) {
            return Some(Answer::Value(Value::String(o.clone())));
        }
        return Some(if f.required {
            Answer::Ask {
                reason: "self-identification question".into(),
            }
        } else {
            Answer::Skip
        });
    }
    // Order matters: more specific labels first.
    if label_has(
        &both,
        &["first name", "first_name", "firstname", "given name"],
    ) {
        return s(&ident.first_name);
    }
    if label_has(
        &both,
        &[
            "last name",
            "last_name",
            "lastname",
            "surname",
            "family name",
        ],
    ) {
        return s(&ident.last_name);
    }
    if label_has(&both, &["full name", "full_name"])
        || l.trim_end_matches('*').trim() == "name"
        || id == "name"
    {
        return s(&ident.full_name);
    }
    if f.kind == "email" || label_has(&both, &["email"]) {
        return s(&ident.email);
    }
    if f.kind == "tel" || label_has(&both, &["phone", "mobile", "contact number"]) {
        return s(&ident.phone);
    }
    if label_has(&both, &["linkedin"]) {
        return s(&ident.linkedin_url);
    }
    if label_has(&both, &["github"]) {
        return s(&ident.github_url);
    }
    if label_has(
        &both,
        &["portfolio", "website", "personal site", "other url", "blog"],
    ) {
        return s(&ident.portfolio_url).or((!f.required).then_some(Answer::Skip));
    }
    if label_has(
        &l,
        &[
            "current company",
            "current employer",
            "organization",
            "company name",
        ],
    ) || id == "org"
    {
        return s(&ident.current_company);
    }
    if label_has(
        &l,
        &[
            "current title",
            "current role",
            "current designation",
            "job title",
        ],
    ) {
        return s(&ident.current_title);
    }
    if label_has(&l, &["notice period"]) {
        return choose(f, &ident.notice_period);
    }
    if label_has(
        &l,
        &["years of experience", "total experience", "how many years"],
    ) && p.resume.total_experience_years > 0.0
    {
        let y = p.resume.total_experience_years;
        let v = if y.fract() == 0.0 {
            format!("{}", y as i64)
        } else {
            format!("{y}")
        };
        if f.options.is_empty() {
            return s(&v);
        }
        // Prefer bounded ranges ("8-12 years") over open ones ("12+ years").
        let bounded = f
            .options
            .iter()
            .find(|o| !o.contains('+') && option_contains_years(o, y));
        let open = f
            .options
            .iter()
            .find(|o| o.contains('+') && option_contains_years(o, y));
        return bounded
            .or(open)
            .map(|o| Answer::Value(Value::String(o.clone())))
            .or_else(|| choose(f, &v));
    }
    if label_has(
        &l,
        &[
            "expected salary",
            "expected ctc",
            "salary expectation",
            "desired salary",
            "expected compensation",
            "compensation expectation",
        ],
    ) {
        return choose(f, &p.salary.expected_text);
    }
    if label_has(
        &l,
        &["current salary", "current ctc", "current compensation"],
    ) {
        return choose(f, &p.salary.current_text);
    }
    if label_has(&l, &["sponsorship", "sponsor"]) {
        return sponsorship_answer(f, p);
    }
    if label_has(
        &l,
        &[
            "authorized to work",
            "authorised to work",
            "legally authorized",
            "work authorization",
            "work authorisation",
            "eligible to work",
            "right to work",
        ],
    ) {
        return work_authorization_answer(f, p);
    }
    if label_has(&l, &["relocate", "relocation"]) {
        return ident.willing_to_relocate.map(|b| yes_no(f, b));
    }
    if label_has(
        &l,
        &[
            "city",
            "location",
            "where are you based",
            "current location",
        ],
    ) {
        let loc = if ident.location.is_empty() {
            &ident.city
        } else {
            &ident.location
        };
        return choose(f, loc);
    }
    if label_has(&l, &["country"]) {
        return choose(f, &ident.country);
    }
    if label_has(&l, &["school", "university", "college", "institution"]) {
        return p
            .highest_education()
            .and_then(|e| choose(f, &e.institution));
    }
    if label_has(&l, &["degree", "qualification", "education"]) {
        return p.highest_education().and_then(|e| choose(f, &e.degree));
    }
    if label_has(&l, &["how did you hear", "where did you hear", "source"]) {
        let d = if p.preferences.how_did_you_hear_default.is_empty() {
            "LinkedIn"
        } else {
            &p.preferences.how_did_you_hear_default
        };
        return choose(f, d).or((!f.required).then_some(Answer::Skip));
    }
    if f.kind == "checkbox"
        && label_has(
            &l,
            &[
                "privacy",
                "consent",
                "terms",
                "agree",
                "acknowledge",
                "gdpr",
            ],
        )
    {
        return Some(if p.preferences.auto_accept_application_consent {
            Answer::Value(Value::Bool(true))
        } else if f.required {
            Answer::Ask {
                reason: "consent required".into(),
            }
        } else {
            Answer::Skip
        });
    }
    if label_has(&l, &["skills"]) && f.kind != "checkbox" {
        let skills = p.skills.core.join(", ");
        return s(&skills);
    }
    None
}

/// Countries a question asks about. "us" is only trusted next to "work in"
/// style phrasing, because as a pronoun it is far more common.
fn countries_asked(label: &str) -> Vec<&'static str> {
    let mut c = crate::knowledge::profile::countries_in(label);
    let l = label.to_lowercase();
    let us_phrase = [
        "in the us",
        "in us",
        "the us?",
        "the us ",
        "(us)",
        "us-based",
        "us based",
    ]
    .iter()
    .any(|p| l.contains(p));
    if us_phrase && !c.contains(&"United States") {
        c.push("United States");
    }
    c
}

/// "Are you authorized to work in X?" Yes only for countries the user is
/// authorized in, No for others, and a human decides when no country is named.
fn work_authorization_answer(f: &Field, p: &Profile) -> Option<Answer> {
    let authorized = p.authorized_countries();
    if authorized.is_empty() {
        return None;
    }
    let is_yes_no = f.options.is_empty()
        || (f.options.len() <= 3 && f.options.iter().any(|o| o.eq_ignore_ascii_case("yes")));
    if !is_yes_no {
        return choose(f, &p.identity.work_authorization);
    }
    let asked = countries_asked(&f.label);
    if asked.is_empty() {
        // Depends on where the job is; ask rather than guess.
        return None;
    }
    Some(yes_no(f, asked.iter().all(|c| authorized.contains(c))))
}

/// "Will you require sponsorship (in X)?" No inside authorized countries;
/// for other countries it follows `requires_sponsorship`.
fn sponsorship_answer(f: &Field, p: &Profile) -> Option<Answer> {
    let authorized = p.authorized_countries();
    let asked = countries_asked(&f.label);
    if !asked.is_empty() && !authorized.is_empty() && asked.iter().all(|c| authorized.contains(c)) {
        return Some(yes_no(f, false));
    }
    let needs = p
        .identity
        .requires_sponsorship
        .or(p.preferences.visa_sponsorship_required.then_some(true));
    match needs {
        Some(false) => Some(yes_no(f, false)),
        Some(true) if !asked.is_empty() => Some(yes_no(f, true)),
        // Needs sponsorship somewhere, but the question doesn't say where.
        _ => None,
    }
}

fn option_contains_years(opt: &str, years: f64) -> bool {
    let nums: Vec<f64> = opt
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .filter_map(|t| t.parse().ok())
        .collect();
    match nums.as_slice() {
        [a, b] => years >= *a && years <= *b,
        [a] if opt.contains('+') => years >= *a,
        _ => false,
    }
}

/// Legal agreements (arbitration, waivers) and employer AI-use policy
/// statements: always the person's own decision, for every application.
pub fn is_legal_agreement(label: &str, options: &[String]) -> bool {
    let text = format!("{} {}", label, options.join(" ")).to_lowercase();
    [
        "arbitrat",
        "waive",
        "waiver",
        "class action",
        "ai policy",
        "use of ai",
        "ai-generated",
        "ai generated",
        "without ai",
        "ai assistance",
        "ai tools",
        "generative ai policy",
        // attestations: statements the person makes about themselves
        "i confirm",
        "i certify",
        "i attest",
        "i have read",
        "i hereby",
    ]
    .iter()
    .any(|w| text.contains(w))
}

/// Sponsorship questions that name no country: answer for the job's country
/// (from the `Location:` line of the job context).
fn sponsorship_for_job(f: &Field, p: &Profile, job_context: &str) -> Option<Answer> {
    if !countries_asked(&f.label).is_empty() {
        return None; // the question names a country: the normal rule applies
    }
    let loc = job_context
        .lines()
        .find_map(|l| l.strip_prefix("Location:"))
        .unwrap_or("");
    let countries = crate::knowledge::profile::countries_in(loc);
    if countries.is_empty() {
        return None;
    }
    let authorized = p.authorized_countries();
    if countries.iter().all(|c| authorized.contains(c)) {
        return Some(yes_no(f, false));
    }
    let needs = p
        .identity
        .requires_sponsorship
        .or(p.preferences.visa_sponsorship_required.then_some(true))?;
    Some(yes_no(f, needs))
}

/// "Are you authorized to work in the country where the job is located?":
/// answered for the job's country (the `Location:` line).
fn authorization_for_job(f: &Field, p: &Profile, job_context: &str) -> Option<Answer> {
    if !countries_asked(&f.label).is_empty() {
        return None;
    }
    let authorized = p.authorized_countries();
    if authorized.is_empty() {
        return None;
    }
    let loc = job_context
        .lines()
        .find_map(|l| l.strip_prefix("Location:"))
        .unwrap_or("");
    let countries = crate::knowledge::profile::countries_in(loc);
    if countries.is_empty() {
        return None;
    }
    Some(yes_no(f, countries.iter().all(|c| authorized.contains(c))))
}

/// Questions that must never be guessed by the model.
pub fn is_sensitive(label: &str) -> bool {
    let l = label.to_lowercase();
    label_has(
        &l,
        &[
            "salary",
            "ctc",
            "compensation",
            "criminal",
            "convicted",
            "felony",
            "background check",
            "drug",
            "visa",
            "sponsor",
            "authorized to work",
            "authorised to work",
            "social security",
            "ssn",
            "aadhaar",
            "pan number",
            "passport",
            "date of birth",
            "bank",
            "medical",
            "disability",
            "signature",
            "password",
        ],
    )
}

#[derive(Deserialize)]
struct LlmAnswer {
    answer: Option<Value>,
    #[serde(default)]
    confidence: Option<f32>,
}

/// Answer one field. `memory` lookups use exact normalised question text.
#[allow(clippy::too_many_arguments)] // explicit dependencies read better than a context bag here
pub async fn answer_field(
    f: &Field,
    profile: &Profile,
    db: &Db,
    kb: &KnowledgeBase,
    llm: &LlmRouter,
    job_context: &str,
    scope: &crate::knowledge::Scope,
    memory_ns: &str,
) -> AppResult<Answer> {
    if is_legal_agreement(&f.label, &f.options) {
        return Ok(if f.required {
            Answer::Ask {
                reason: "legal agreement or employer AI-use policy: your own decision".into(),
            }
        } else {
            Answer::Skip
        });
    }
    if label_has(&f.label.to_lowercase(), &["sponsorship", "sponsor"]) {
        if let Some(a) = sponsorship_for_job(f, profile, job_context) {
            return Ok(a);
        }
    }
    if label_has(
        &f.label.to_lowercase(),
        &["authorized to work", "authorised to work", "legally authorized", "eligible to work"],
    ) {
        if let Some(a) = authorization_for_job(f, profile, job_context) {
            return Ok(a);
        }
    }
    if let Some(a) = from_profile(f, profile) {
        return Ok(a);
    }
    // Remembered answers are per person (`memory_ns` = "<person>:").
    let label = format!("{memory_ns}{}", f.label);
    if let Some(prev) = db.call(move |c| jdb::recall_answer(c, &label)).await? {
        if let Some(a) = choose(f, &prev) {
            return Ok(a);
        }
    }
    if is_sensitive(&f.label) {
        return Ok(if f.required {
            Answer::Ask {
                reason: "sensitive question".into(),
            }
        } else {
            Answer::Skip
        });
    }
    if f.kind == "checkbox" && f.options.len() <= 1 {
        return Ok(if f.required {
            Answer::Ask {
                reason: format!("unknown checkbox \"{}\"", f.label),
            }
        } else {
            Answer::Skip
        });
    }
    let snippets = kb
        .search_scoped(&f.label, 5, scope)
        .await
        .unwrap_or_default();
    let mut kb_text = profile.summary_text();
    for sn in snippets {
        kb_text.push_str(&format!(
            "\n[{}] {}",
            sn.path,
            sn.content.chars().take(600).collect::<String>()
        ));
    }
    let job = prompts::sanitize_untrusted(job_context, 2500).text;
    let question = prompts::sanitize_untrusted(&f.label, 400).text;
    let options = if f.options.is_empty() {
        String::new()
    } else {
        format!(
            "\nAllowed options (answer with one exactly): {}",
            f.options.join(" | ")
        )
    };
    let req = LlmRequest::new(
        LlmTask::FormAnswer,
        format!(
            "You fill job application forms for the candidate using ONLY the CANDIDATE data. {} \
If the data does not clearly answer the question, return {{\"answer\": null, \"confidence\": 0}}. \
Return JSON: {{\"answer\": string|null, \"confidence\": number 0..1}}. Keep free-text answers under 120 words, first person.",
            prompts::DATA_ONLY_RULE
        ),
        format!(
            "CANDIDATE:\n{kb_text}\n\nJOB:\n{}\n\nQuestion: {question}{options}",
            prompts::untrusted_block("JOB", &job)
        ),
    )
    .json()
    .max_tokens(300);
    let ans = match llm.complete_json::<LlmAnswer>(&req).await {
        Ok((v, _)) => v,
        Err(e) => {
            tracing::warn!(error = %e, field = %f.id, "LLM form answer failed");
            return Ok(if f.required {
                Answer::Ask {
                    reason: "unknown_question".into(),
                }
            } else {
                Answer::Skip
            });
        }
    };
    let conf = ans.confidence.unwrap_or(0.0);
    let text = match ans.answer {
        Some(Value::String(s)) if !s.trim().is_empty() => s,
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => {
            if b {
                "Yes".into()
            } else {
                "No".into()
            }
        }
        _ => String::new(),
    };
    if conf >= 0.7 && !text.is_empty() {
        if let Some(a) = choose(f, &text) {
            return Ok(a);
        }
    }
    Ok(if f.required {
        Answer::Ask {
            reason: "unknown_question".into(),
        }
    } else {
        Answer::Skip
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::profile::*;

    fn field(id: &str, label: &str, kind: &str, required: bool, options: &[&str]) -> Field {
        Field {
            id: id.into(),
            label: label.into(),
            kind: kind.into(),
            required,
            options: options.iter().map(|s| s.to_string()).collect(),
            accept: None,
        }
    }

    fn profile() -> Profile {
        Profile {
            identity: Identity {
                full_name: "Asha Rao".into(),
                first_name: "Asha".into(),
                last_name: "Rao".into(),
                email: "asha@example.com".into(),
                phone: "+91 98765 43210".into(),
                linkedin_url: "https://linkedin.com/in/asha".into(),
                location: "Bengaluru, India".into(),
                country: "India".into(),
                work_authorization: "Yes - Indian citizen".into(),
                requires_sponsorship: Some(false),
                notice_period: "30 days".into(),
                willing_to_relocate: Some(false),
                ..Default::default()
            },
            resume: Resume {
                total_experience_years: 12.0,
                ..Default::default()
            },
            salary: SalaryPreferences {
                expected_text: "60 LPA".into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn maps_identity_fields() {
        let p = profile();
        assert_eq!(
            from_profile(&field("first_name", "First Name *", "text", true, &[]), &p),
            Some(Answer::Value("Asha".into()))
        );
        assert_eq!(
            from_profile(&field("email", "Email", "email", true, &[]), &p),
            Some(Answer::Value("asha@example.com".into()))
        );
        assert_eq!(
            from_profile(&field("resume", "Resume/CV", "file", true, &[]), &p),
            Some(Answer::ResumeFile)
        );
        assert_eq!(
            from_profile(&field("cl", "Cover Letter", "textarea", false, &[]), &p),
            Some(Answer::CoverLetter)
        );
        assert_eq!(
            from_profile(&field("q1", "LinkedIn Profile", "url", false, &[]), &p),
            Some(Answer::Value("https://linkedin.com/in/asha".into()))
        );
    }

    #[test]
    fn maps_choice_questions() {
        let p = profile();
        assert_eq!(
            from_profile(
                &field(
                    "q2",
                    "Are you legally authorized to work in India?",
                    "select",
                    true,
                    &["Yes", "No"]
                ),
                &p
            ),
            Some(Answer::Value("Yes".into()))
        );
        assert_eq!(
            from_profile(
                &field(
                    "q3",
                    "Will you now or in the future require sponsorship?",
                    "radio",
                    true,
                    &["Yes", "No"]
                ),
                &p
            ),
            Some(Answer::Value("No".into()))
        );
        assert_eq!(
            from_profile(
                &field(
                    "q4",
                    "Years of experience",
                    "select",
                    true,
                    &["0-2 years", "3-7 years", "8-12 years", "12+ years"]
                ),
                &p
            ),
            Some(Answer::Value("8-12 years".into()))
        );
        assert_eq!(
            from_profile(
                &field(
                    "q5",
                    "Notice period",
                    "select",
                    true,
                    &["Immediate", "15 days", "30 days", "60 days"]
                ),
                &p
            ),
            Some(Answer::Value("30 days".into()))
        );
        assert_eq!(
            from_profile(
                &field(
                    "g",
                    "Gender",
                    "select",
                    false,
                    &["Male", "Female", "Decline to self-identify"]
                ),
                &p
            ),
            Some(Answer::Value("Decline to self-identify".into()))
        );
        assert_eq!(
            from_profile(
                &field("ctc", "What is your expected CTC?", "text", true, &[]),
                &p
            ),
            Some(Answer::Value("60 LPA".into()))
        );
    }

    #[test]
    fn work_authorization_is_country_aware() {
        let mut p = profile();
        p.identity.work_authorization = "India".into();
        p.identity.requires_sponsorship = Some(true);
        let yn = ["Yes", "No"];
        let ask = |label: &str| from_profile(&field("q", label, "select", true, &yn), &p);
        assert_eq!(
            ask("Are you legally authorized to work in India?"),
            Some(Answer::Value("Yes".into()))
        );
        assert_eq!(
            ask("Are you legally authorized to work in the United States?"),
            Some(Answer::Value("No".into()))
        );
        assert_eq!(
            ask("Are you authorized to work in the US?"),
            Some(Answer::Value("No".into()))
        );
        assert_eq!(
            ask("Are you legally authorized to work in the country where this job is located?"),
            None
        );
        assert_eq!(
            ask("Will you require visa sponsorship to work in India?"),
            Some(Answer::Value("No".into()))
        );
        assert_eq!(
            ask("Will you require visa sponsorship to work in the UK?"),
            Some(Answer::Value("Yes".into()))
        );
        assert_eq!(
            ask("Will you now or in the future require sponsorship?"),
            None
        );
    }

    #[test]
    fn consent_and_unknowns_escalate() {
        let p = profile();
        assert!(matches!(
            from_profile(
                &field("c", "I agree to the privacy policy", "checkbox", true, &[]),
                &p
            ),
            Some(Answer::Ask { .. })
        ));
        assert_eq!(
            from_profile(&field("x", "Favourite colour?", "text", true, &[]), &p),
            None
        );
        assert!(is_sensitive("Have you ever been convicted of a felony?"));
        assert!(!is_sensitive("Why do you want to join Acme?"));
    }

    #[test]
    fn option_matching() {
        let o: Vec<String> = vec!["Yes, I am".into(), "No".into()];
        assert_eq!(match_option(&o, "yes"), Some("Yes, I am".into()));
        assert_eq!(match_option(&o, "no"), Some("No".into()));
        assert_eq!(match_option(&o, "maybe"), None);
    }

    #[test]
    fn legal_agreements_and_job_country_sponsorship() {
        assert!(is_legal_agreement("Agreement to Arbitrate", &[]));
        assert!(is_legal_agreement("Please confirm", &["I understand and agree to the terms of the Agreement to Arbitrate".into()]));
        assert!(is_legal_agreement("AI Policy for Application", &[]));
        assert!(!is_legal_agreement("Why do you want to join Acme?", &[]));
        assert!(is_legal_agreement("I confirm I have read the above.", &[]));

        let mut p = Profile::default();
        p.identity.requires_sponsorship = Some(true);
        p.identity.work_authorization = "Indian citizen, authorized to work in India".into();
        p.identity.authorized_countries = vec!["India".into()];
        let yn = ["Yes", "No"];
        let q = field("q", "Do you require visa sponsorship?", "select", true, &yn);
        let us = "Engineer at Acme\nLocation: San Francisco, CA | Seattle, WA\n";
        let india = "Engineer at Acme\nLocation: Bengaluru\n";
        assert!(matches!(sponsorship_for_job(&q, &p, us), Some(Answer::Value(v)) if v == "Yes"));
        assert!(matches!(sponsorship_for_job(&q, &p, india), Some(Answer::Value(v)) if v == "No"));
        assert!(sponsorship_for_job(&q, &p, "Engineer\nLocation: Remote\n").is_none());
        let auth = field("a", "Are you authorized to work in the country where the job is located?", "radio", true, &yn);
        assert!(matches!(authorization_for_job(&auth, &p, us), Some(Answer::Value(v)) if v == "No"));
        assert!(matches!(authorization_for_job(&auth, &p, india), Some(Answer::Value(v)) if v == "Yes"));
    }
}
