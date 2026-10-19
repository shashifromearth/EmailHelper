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
        key: "full_name",
        label: "Full name",
        private: false,
        patterns: &["full name:", "candidate name:", "full name?"],
    },
    FactSpec {
        key: "email",
        label: "Email address",
        private: false,
        patterns: &["email address:", "email id:", "email:"],
    },
    FactSpec {
        key: "current_designation",
        label: "Current designation",
        private: false,
        patterns: &["current designation", "current title", "designation:"],
    },
    FactSpec {
        key: "linkedin",
        label: "LinkedIn profile",
        private: false,
        patterns: &["linkedin url", "linkedin profile url", "linkedin link", "share your linkedin", "linkedin profile:"],
    },
    FactSpec {
        key: "preferred_location",
        label: "Preferred location",
        private: false,
        patterns: &["preferred location", "preferred job location", "preferred work location", "location preference"],
    },
    FactSpec {
        key: "work_mode",
        label: "Preferred work mode",
        private: false,
        patterns: &["preferred work mode", "work mode preference", "preferred mode of work"],
    },
    FactSpec {
        key: "employment_type",
        label: "Employment preference",
        private: false,
        patterns: &[
            "employment preference",
            "open to full-time",
            "full-time or contract",
            "contract or full-time",
            "permanent or contract",
        ],
    },
    FactSpec {
        key: "joining_date",
        label: "Earliest joining date",
        private: false,
        patterns: &["joining date", "earliest possible joining", "date of joining", "earliest start date"],
    },
    FactSpec {
        key: "reason_for_change",
        label: "Reason for looking for a change",
        private: false,
        patterns: &[
            "reason for looking",
            "reason for change",
            "reason for job change",
            "why are you looking",
            "reason for leaving",
            "reason for switch",
        ],
    },
    FactSpec {
        key: "current_offers",
        label: "Offers in hand",
        private: false,
        patterns: &["holding any offers", "offers in hand", "offer in hand", "any other offers", "holding an offer"],
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
    /// The resume file travels with the draft (Thunderbird attaches it).
    pub resume_attached: bool,
    /// Skills from the person's resume; the draft may claim only these.
    pub skills: Vec<String>,
}

/// Tidy a free-text answer from Telegram/dashboard: "Current ctc : 55 Lpa"
/// or "my notice period is 30 days" becomes "55 LPA" / "30 days".
pub fn clean_answer(key: &str, raw: &str) -> String {
    let original = raw.trim();
    let Some(spec) = FACTS.iter().find(|f| f.key == key) else {
        return original.to_string();
    };
    let mut names: Vec<String> = spec.patterns.iter().map(|p| p.to_string()).collect();
    names.push(spec.label.to_lowercase());
    names.push(key.replace('_', " "));
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    let mut t = original.to_string();
    let lower = t.to_lowercase();
    if let Some((before, after)) = t.split_once(':') {
        let b = before.to_lowercase();
        if names.iter().any(|n| b.contains(n.as_str())) {
            t = after.trim().to_string();
        }
    } else {
        let l = lower.strip_prefix("my ").unwrap_or(&lower);
        let same_bytes = lower.len() == t.len();
        if let Some(n) = names.iter().find(|n| same_bytes && l.starts_with(n.as_str())) {
            let skip = lower.len() - l.len() + n.len();
            let rest = t[skip..].trim_start();
            let rest = rest
                .strip_prefix("is ")
                .or_else(|| rest.strip_prefix("would be "))
                .or_else(|| rest.strip_prefix("- "))
                .or_else(|| rest.strip_prefix("= "))
                .unwrap_or(rest);
            t = rest.trim().to_string();
        }
    }
    let lpa = regex::Regex::new(r"(?i)\blpa\b").expect("valid regex");
    let t = lpa.replace_all(t.trim(), "LPA").to_string();
    if t.is_empty() { original.to_string() } else { t }
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
        "last_working_day" => non_empty(&prof.salary.last_working_day).or_else(|| {
            // Someone available immediately isn't serving a notice period.
            let n = non_empty(&prof.salary.notice_period)
                .or_else(|| non_empty(&prof.identity.notice_period))?
                .to_lowercase();
            (n.contains("immediate") || n.starts_with("0 ") || n == "0" || n.contains("not serving"))
                .then(|| "Not applicable; not serving a notice period (available immediately)".to_string())
        }),
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
        "full_name" => non_empty(&prof.identity.full_name).or_else(|| non_empty(&p.name)),
        "email" => non_empty(&prof.identity.email),
        "current_designation" => non_empty(&prof.identity.current_title),
        "linkedin" => non_empty(&prof.identity.linkedin_url),
        "preferred_location" => {
            let mut locs: Vec<String> = Vec::new();
            for l in &prof.preferences.preferred_locations {
                let l = l.trim();
                // "Bangalore" next to "Bengaluru", or a whole country, adds nothing.
                if l.is_empty() || l.eq_ignore_ascii_case("bangalore") && locs.iter().any(|x| x == "Bengaluru") {
                    continue;
                }
                if !locs.iter().any(|x| x.eq_ignore_ascii_case(l)) {
                    locs.push(l.to_string());
                }
            }
            let relocate = prof.identity.willing_to_relocate == Some(true);
            match (locs.is_empty(), relocate) {
                (true, true) => Some("Open to any location".into()),
                (true, false) => None,
                (false, true) => Some(format!("{}; open to relocation", locs.join(", "))),
                (false, false) => Some(locs.join(", ")),
            }
        }
        "work_mode" => match prof.preferences.remote.trim().to_ascii_lowercase().as_str() {
            "remote_only" | "remote" => Some("Remote".into()),
            "hybrid" => Some("Hybrid".into()),
            "onsite" | "on-site" => Some("On-site".into()),
            "any" => Some("Open to on-site, hybrid or remote".into()),
            _ => None,
        },
        "joining_date" => {
            let n = non_empty(&prof.salary.notice_period).or_else(|| non_empty(&prof.identity.notice_period))?;
            if n.to_lowercase().contains("immediate") {
                Some("Immediately".into())
            } else {
                Some(format!("After my notice period ({n})"))
            }
        }
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
                .or_else(|| stored.get(f.key).map(|v| clean_answer(f.key, v)));
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
                .map(|v| clean_answer(f.key, v))
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
        self.answers.is_empty() && self.attach_resume.is_none() && self.skills.is_empty()
    }

    /// Prompt section listing exactly how to answer each question.
    pub fn prompt_section(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut s = String::new();
        if !self.answers.is_empty() || self.attach_resume.is_some() {
            s.push_str("The email asks for the following. Answer EVERY item directly, using these facts exactly:\n");
        }
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
        if !self.skills.is_empty() {
            s.push_str(&format!(
                "- My skills (from my resume): {}. When the email asks about specific technologies, claim \
experience ONLY with ones in this list; for the others say I haven't worked with them hands-on, or leave \
them out. Never invent experience.\n",
                self.skills.join(", ")
            ));
        }
        if self.attach_resume.is_some() {
            if self.resume_attached {
                s.push_str("- Updated resume: it IS attached to this reply; say so in one short sentence\n");
            } else {
                s.push_str("- Updated resume: it is NOT attached; say you'll send it shortly\n");
            }
        }
        s
    }

    /// Plain facts the draft may contain (for the hallucination check).
    /// Private values are deliberately excluded.
    pub fn grounding(&self) -> String {
        let mut lines: Vec<String> = self
            .answers
            .iter()
            .filter_map(|(f, a)| match a {
                FactAnswer::Value(v) => Some(format!("{}: {}", f.label, v)),
                FactAnswer::Token { token, .. } => Some(format!("{}: {}", f.label, token)),
                _ => None,
            })
            .collect();
        if !self.skills.is_empty() {
            lines.push(format!("Skills (from resume): {}", self.skills.join(", ")));
        }
        if let (Some(f), true) = (&self.attach_resume, self.resume_attached) {
            lines.push(format!("Attached to this reply: {f}"));
        }
        lines.join("\n")
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

    /// Make sure the draft answers every asked item: models sometimes skip
    /// some. Missing answers are added as a short list before the sign-off.
    /// Returns the draft and the labels that were added.
    pub fn ensure_answered(&self, draft: &str) -> (String, Vec<&'static str>) {
        let lower = draft.to_lowercase();
        let mentions = |f: &FactSpec| {
            lower.contains(&f.label.to_lowercase()) || f.patterns.iter().any(|p| lower.contains(p))
        };
        let mut missing: Vec<(&'static str, String)> = Vec::new();
        for (f, a) in &self.answers {
            let line = match a {
                FactAnswer::Value(v) => {
                    // Covered when every significant word of the value is there
                    // ("available immediately" covers "Immediately Available").
                    let words: Vec<String> = v
                        .split(|c: char| !c.is_alphanumeric())
                        .filter(|w| w.len() >= 4 || w.chars().any(|c| c.is_ascii_digit()))
                        .map(str::to_lowercase)
                        .collect();
                    let covered = if words.is_empty() {
                        lower.contains(&v.to_lowercase())
                    } else {
                        words.iter().all(|w| lower.contains(w.as_str()))
                    };
                    (!covered).then(|| v.clone())
                }
                FactAnswer::Token { token, .. } => (!draft.contains(token.as_str())).then(|| token.clone()),
                FactAnswer::SecureChannel => (!mentions(f))
                    .then(|| "I'll share this over a secure channel rather than by email".to_string()),
                FactAnswer::Decline => {
                    (!mentions(f)).then(|| "I'd prefer not to share this over email".to_string())
                }
                FactAnswer::Unknown => (!mentions(f)).then(|| "I'll confirm this shortly".to_string()),
            };
            if let Some(text) = line {
                missing.push((f.label, text));
            }
        }
        if missing.is_empty() {
            return (draft.to_string(), Vec::new());
        }
        let block = format!(
            "For quick reference:\n{}",
            missing
                .iter()
                .map(|(l, t)| format!("- {l}: {t}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let lines: Vec<&str> = draft.trim_end().lines().collect();
        let out = match lines.iter().rposition(|l| crate::agents::drafter::is_signoff(l)) {
            Some(i) => {
                let before = lines[..i].join("\n");
                let after = lines[i..].join("\n");
                format!("{}\n\n{block}\n\n{after}", before.trim_end())
            }
            None => format!("{}\n\n{block}", draft.trim_end()),
        };
        (out, missing.iter().map(|(l, _)| *l).collect())
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
        assert!(section.contains("Updated resume: it is NOT attached"));
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

    #[test]
    fn missing_answers_are_added_before_signoff() {
        let mut plan = FactPlan::default();
        let spec = |k: &str| FACTS.iter().find(|f| f.key == k).unwrap();
        plan.answers.push((spec("expected_ctc"), FactAnswer::Value("As per industry standards".into())));
        plan.answers.push((spec("current_ctc"), FactAnswer::Unknown));
        plan.answers.push((spec("notice_period"), FactAnswer::Value("Immediately Available".into())));
        let draft = "Hi,\n\nI'm based in Bengaluru and available immediately.\nCould you share the compensation range?\n\nBest regards,\nShashi";
        let (out, added) = plan.ensure_answered(draft);
        assert_eq!(added, vec!["Expected CTC", "Current CTC"]);
        assert!(out.contains("For quick reference:\n- Expected CTC: As per industry standards\n- Current CTC: I'll confirm this shortly\n\nBest regards,\nShashi"), "{out}");
        // Already answered → unchanged.
        let ok = "Expected CTC is in line with industry standards; current CTC I'll confirm shortly. Available immediately.\n\nRegards,\nS";
        let (same, none) = plan.ensure_answered(ok);
        assert!(none.is_empty(), "{none:?}");
        assert_eq!(same, ok);
    }

    #[test]
    fn answers_lose_repeated_labels() {
        assert_eq!(clean_answer("current_ctc", "Current ctc : 55 Lpa"), "55 LPA");
        assert_eq!(clean_answer("current_ctc", "55 lpa"), "55 LPA");
        assert_eq!(clean_answer("notice_period", "my notice period is 30 days"), "30 days");
        assert_eq!(clean_answer("expected_ctc", "Expected CTC - 70 LPA"), "70 LPA");
        assert_eq!(clean_answer("notice_period", "Immediate"), "Immediate");
        assert_eq!(clean_answer("current_location", "Bengaluru: Whitefield"), "Bengaluru: Whitefield");
    }

    #[test]
    fn immediate_joiner_has_no_last_working_day_and_skills_are_listed() {
        let mut p = person();
        let prof = Arc::make_mut(&mut p.profile);
        prof.salary.notice_period = String::new();
        prof.identity.notice_period = "Immediately Available".into();
        prof.salary.last_working_day = String::new();
        let asked = asked("x", "notice period? what is your last working day?");
        let mut plan = plan(&p, &BTreeMap::new(), &asked, None);
        assert!(plan.unknown().is_empty(), "nothing to ask: {:?}", plan.unknown());
        plan.skills = vec!["Python".into(), "Django".into()];
        let section = plan.prompt_section();
        assert!(section.contains("Last working day: Not applicable"), "{section}");
        assert!(section.contains("My skills (from my resume): Python, Django"), "{section}");
        assert!(plan.grounding().contains("Skills (from resume): Python, Django"));
    }
}
