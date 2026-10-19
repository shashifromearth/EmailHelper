//! Answering what an email actually asks.
//!
//! 1. Detect which facts the email asks for (CTC, notice period, DOB, PAN …).
//! 2. Look each one up for the receiving person (files + answers they gave
//!    earlier via Telegram/dashboard).
//! 3. Build prompt instructions:
//!    - known, non-sensitive → the value itself;
//!    - sensitive + "on_request" → a placeholder like `{{PAN}}` that is filled
//!      in locally *after* generation, so the value never reaches any LLM;
//!    - "secure_channel" / "never" → say it will be shared securely / decline;
//!    - unknown → say it will be confirmed shortly, and ask the person.

use std::collections::BTreeMap;

use crate::knowledge::people::{Person, SharePolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FactSpec {
    pub key: &'static str,
    pub label: &'static str,
    /// Lower-case phrases that indicate the email asks for this fact.
    pub patterns: &'static [&'static str],
    /// Stored in private.json; value never goes into a prompt.
    pub private: bool,
}

pub const FACTS: &[FactSpec] = &[
    FactSpec {
        key: "expected_ctc",
        label: "Expected CTC",
        private: false,
        patterns: &[
            "expected ctc",
            "expected salary",
            "salary expectation",
            "expected compensation",
            "expected package",
            "ectc",
        ],
    },
    FactSpec {
        key: "current_ctc",
        label: "Current CTC",
        private: false,
        patterns: &[
            "current ctc",
            "current salary",
            "current compensation",
            "current package",
            "present ctc",
            "cctc",
            "fixed ctc",
            "variable ctc",
        ],
    },
    FactSpec {
        key: "notice_period",
        label: "Notice period",
        private: false,
        patterns: &[
            "notice period",
            "how soon can you join",
            "earliest joining",
            "joining time",
            "when can you join",
            "availability to join",
            "early joiner",
            "immediate joiner",
        ],
    },
    FactSpec {
        key: "last_working_day",
        label: "Last working day",
        private: false,
        patterns: &[
            "last working day",
            "last working date",
            "lwd",
            "relieving date",
            "last day at",
        ],
    },
    FactSpec {
        key: "total_experience",
        label: "Total experience",
        private: false,
        patterns: &[
            "total experience",
            "years of experience",
            "total exp",
            "overall experience",
            "relevant experience",
        ],
    },
    FactSpec {
        key: "current_company",
        label: "Current company",
        private: false,
        patterns: &[
            "current company",
            "current employer",
            "current organization",
            "current organisation",
        ],
    },
    FactSpec {
        key: "current_location",
        label: "Current location",
        private: false,
        patterns: &[
            "current location",
            "current city",
            "where are you based",
            "based out of",
        ],
    },
    FactSpec {
        key: "phone",
        label: "Phone number",
        private: false,
        patterns: &[
            "contact number",
            "phone number",
            "mobile number",
            "whatsapp number",
        ],
    },
    FactSpec {
        key: "resume",
        label: "Updated resume",
        private: false,
        patterns: &[
            "updated resume",
            "updated cv",
            "share your resume",
            "send your resume",
            "latest resume",
            "attach your resume",
            "share your cv",
            "send your cv",
            "share resume",
            "send resume",
        ],
    },
    FactSpec {
        key: "date_of_birth",
        label: "Date of birth",
        private: true,
        patterns: &["date of birth", "dob", "birth date", "d.o.b"],
    },
    FactSpec {
        key: "pan",
        label: "PAN",
        private: true,
        patterns: &[
            "pan card",
            "pan number",
            "pan no",
            "pan details",
            "pan:",
            "pan -",
            "pan num",
        ],
    },
    FactSpec {
        key: "aadhaar",
        label: "Aadhaar",
        private: true,
        patterns: &["aadhaar", "aadhar", "uidai"],
    },
    FactSpec {
        key: "address",
        label: "Address",
        private: true,
        patterns: &[
            "current address",
            "residential address",
            "permanent address",
            "home address",
            "postal address",
        ],
    },
    FactSpec {
        key: "passport",
        label: "Passport",
        private: true,
        patterns: &["passport number", "passport no", "passport details"],
    },
];

/// Facts the email asks for, in catalogue order.
pub fn asked(subject: &str, body: &str) -> Vec<&'static FactSpec> {
    let text = format!("{subject}\n{body}").to_lowercase();
    // "PAN India" means "anywhere in India", not the tax ID.
    let text = text.replace("pan india", "anywhere-in-india");
    FACTS
        .iter()
        .filter(|f| {
            f.patterns.iter().any(|p| {
                if p.len() <= 5 && p.chars().all(|c| c.is_ascii_alphanumeric()) {
                    crate::jobs::extract::contains_word(&text, p)
                } else {
                    text.contains(p)
                }
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub enum FactAnswer {
    Value(String),
    /// Placeholder for a private value, filled after generation.
    Token {
        token: String,
        value: String,
    },
    SecureChannel,
    Decline,
    Unknown,
}

#[derive(Debug, Clone, Default)]
pub struct FactPlan {
    pub answers: Vec<(&'static FactSpec, FactAnswer)>,
    /// Resume the person should attach, when the email asks for one.
    pub attach_resume: Option<String>,
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Value of a non-private fact from the person's files.
fn file_value(p: &Person, key: &str) -> Option<String> {
    let prof = &p.profile;
    match key {
        "expected_ctc" => non_empty(&prof.salary.expected_text),
        "current_ctc" => non_empty(&prof.salary.current_text),
        "notice_period" => non_empty(&prof.salary.notice_period)
            .or_else(|| non_empty(&prof.identity.notice_period)),
        "last_working_day" => non_empty(&prof.salary.last_working_day),
        "total_experience" => {
            let y = prof.resume.total_experience_years;
            (y > 0.0).then(|| {
                if y.fract() == 0.0 {
                    format!("{}+ years", y as i64)
                } else {
                    format!("{y} years")
                }
            })
        }
        "current_company" => {
            let c = prof.identity.current_company.trim();
            let t = prof.identity.current_title.trim();
            match (c.is_empty(), t.is_empty()) {
                (false, false) => Some(format!("{t} at {c}")),
                (false, true) => Some(c.to_string()),
                _ => None,
            }
        }
        "current_location" => {
            non_empty(&prof.identity.location).or_else(|| non_empty(&prof.identity.city))
        }
        "phone" => non_empty(&prof.identity.phone),
        _ => None,
    }
}

/// Decide how each asked fact will be handled for this person.
/// `stored` holds answers the person gave earlier (person_facts table).
pub fn plan(
    person: &Person,
    stored: &BTreeMap<String, String>,
    asked: &[&'static FactSpec],
    resume_file: Option<&str>,
) -> FactPlan {
    let mut out = FactPlan::default();
    for f in asked {
        if f.key == "resume" {
            out.attach_resume = resume_file.map(str::to_string);
            continue;
        }
        let answer = if f.private {
            let value = person
                .private
                .values
                .get(f.key)
                .cloned()
                .or_else(|| stored.get(f.key).cloned());
            match (person.private.policy(f.key), value) {
                (SharePolicy::Never, _) => FactAnswer::Decline,
                (SharePolicy::SecureChannel, _) => FactAnswer::SecureChannel,
                (SharePolicy::OnRequest, Some(v)) => FactAnswer::Token {
                    token: format!("{{{{{}}}}}", f.key.to_ascii_uppercase()),
                    value: v,
                },
                (SharePolicy::OnRequest, None) => FactAnswer::Unknown,
            }
        } else {
            match stored
                .get(f.key)
                .cloned()
                .or_else(|| file_value(person, f.key))
            {
                Some(v) => FactAnswer::Value(v),
                None => FactAnswer::Unknown,
            }
        };
        out.answers.push((f, answer));
    }
    out
}

impl FactPlan {
    pub fn is_empty(&self) -> bool {
        self.answers.is_empty() && self.attach_resume.is_none()
    }

    /// Prompt section listing exactly how to answer each question.
    pub fn prompt_section(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut s = String::from(
            "The email asks for the following. Answer EVERY item directly, using these facts exactly:\n",
        );
        for (f, a) in &self.answers {
            let line = match a {
                FactAnswer::Value(v) => format!("- {}: {}", f.label, v),
                FactAnswer::Token { token, .. } => format!(
                    "- {}: write exactly {} (it is filled in automatically)",
                    f.label, token
                ),
                FactAnswer::SecureChannel => format!(
                    "- {}: say you will share it over a secure channel, not by email",
                    f.label
                ),
                FactAnswer::Decline => {
                    format!("- {}: politely say you don't share it by email", f.label)
                }
                FactAnswer::Unknown => format!(
                    "- {}: not known yet; say you will confirm it shortly",
                    f.label
                ),
            };
            s.push_str(&line);
            s.push('\n');
        }
        if self.attach_resume.is_some() {
            s.push_str("- Updated resume: say the updated resume is attached\n");
        }
        s
    }

    /// Plain facts the draft may contain (for the hallucination check).
    /// Private values are deliberately excluded.
    pub fn grounding(&self) -> String {
        self.answers
            .iter()
            .filter_map(|(f, a)| match a {
                FactAnswer::Value(v) => Some(format!("{}: {}", f.label, v)),
                FactAnswer::Token { token, .. } => Some(format!("{}: {}", f.label, token)),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn unknown(&self) -> Vec<&'static FactSpec> {
        self.answers
            .iter()
            .filter(|(_, a)| matches!(a, FactAnswer::Unknown))
            .map(|(f, _)| *f)
            .collect()
    }

    /// Replace our own placeholders with neutral words so the validator
    /// doesn't flag them, without exposing the private values to it.
    pub fn mask_tokens(&self, draft: &str) -> String {
        let mut out = draft.to_string();
        for (f, a) in &self.answers {
            if let FactAnswer::Token { token, .. } = a {
                out = out.replace(token.as_str(), &format!("({} provided)", f.label));
            }
        }
        out
    }

    /// Insert private values into the final draft. Returns the draft and the
    /// labels of the private facts it now contains.
    pub fn fill_tokens(&self, draft: &str) -> (String, Vec<String>) {
        let mut out = draft.to_string();
        let mut used = Vec::new();
        for (f, a) in &self.answers {
            if let FactAnswer::Token { token, value } = a {
                if out.contains(token.as_str()) {
                    out = out.replace(token.as_str(), value);
                    used.push(f.label.to_string());
                }
            }
        }
        (out, used)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::people::PrivateFacts;
    use std::sync::Arc;

    fn person() -> Person {
        let mut prof = crate::knowledge::Profile::default();
        prof.identity.full_name = "Shashi Kumar".into();
        prof.identity.current_company = "McKinsey & Company".into();
        prof.identity.current_title = "Lead Backend Engineer".into();
        prof.salary.expected_text = "As per industry standards".into();
        prof.resume.total_experience_years = 12.0;
        let mut private = PrivateFacts::default();
        private
            .values
            .insert("date_of_birth".into(), "02-01-1990".into());
        private.values.insert("pan".into(), "ABCDE1234F".into());
        private
            .share
            .insert("date_of_birth".into(), SharePolicy::OnRequest);
        Person {
            id: "shashi".into(),
            name: "Shashi Kumar".into(),
            dir: Default::default(),
            rel_dir: "people/shashi".into(),
            mailboxes: vec![],
            job_agent: true,
            telegram_chat_id: None,
            profile: Arc::new(prof),
            private,
            resumes: vec![],
        }
    }

    #[test]
    fn detects_asked_facts() {
        let a = asked(
            "Profile details needed",
            "Please share your current CTC, expected CTC, notice period, last working day, DOB, PAN card number and updated resume. Location: PAN India.",
        );
        let keys: Vec<&str> = a.iter().map(|f| f.key).collect();
        assert_eq!(
            keys,
            vec![
                "expected_ctc",
                "current_ctc",
                "notice_period",
                "last_working_day",
                "resume",
                "date_of_birth",
                "pan"
            ]
        );
        assert!(
            asked("Hello", "We hire across PAN India locations").is_empty(),
            "PAN India is not the tax ID"
        );
        assert!(asked("Hi", "Thanks for the update").is_empty());
    }

    #[test]
    fn plans_values_tokens_and_unknowns() {
        let p = person();
        let mut stored = BTreeMap::new();
        stored.insert("notice_period".into(), "60 days".into());
        let a = asked("x", "current ctc, expected ctc, notice period, last working day, dob, pan card, total experience, current company, send your resume");
        let plan = plan(&p, &stored, &a, Some("python.docx"));
        let section = plan.prompt_section();
        assert!(section.contains("Expected CTC: As per industry standards"));
        assert!(
            section.contains("Notice period: 60 days"),
            "stored answer used"
        );
        assert!(section.contains("Total experience: 12+ years"));
        assert!(section.contains("Current company: Lead Backend Engineer at McKinsey & Company"));
        assert!(section.contains("Date of birth: write exactly {{DATE_OF_BIRTH}}"));
        assert!(
            section.contains("PAN: say you will share it over a secure channel"),
            "PAN defaults to secure channel"
        );
        assert!(section.contains("Current CTC: not known yet"));
        assert!(section.contains("updated resume is attached"));
        assert_eq!(plan.attach_resume.as_deref(), Some("python.docx"));
        let unknown: Vec<&str> = plan.unknown().iter().map(|f| f.key).collect();
        assert_eq!(unknown, vec!["current_ctc", "last_working_day"]);
        assert!(
            !section.contains("02-01-1990") && !section.contains("ABCDE1234F"),
            "private values never in prompt"
        );
        assert!(!plan.grounding().contains("02-01-1990"));
    }

    #[test]
    fn fills_tokens_after_generation() {
        let p = person();
        let a = asked("x", "Please share your date of birth");
        let plan = plan(&p, &BTreeMap::new(), &a, None);
        let draft = "Hi, my date of birth is {{DATE_OF_BIRTH}}. Thanks";
        let masked = plan.mask_tokens(draft);
        assert!(!masked.contains("{{") && !masked.contains("02-01-1990"));
        let (filled, used) = plan.fill_tokens(draft);
        assert_eq!(filled, "Hi, my date of birth is 02-01-1990. Thanks");
        assert_eq!(used, vec!["Date of birth"]);
    }
}
