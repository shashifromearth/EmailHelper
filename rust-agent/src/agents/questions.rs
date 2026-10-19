//! Recruiter questionnaires: emails that end with a list of items to fill in
//! ("Full Name:", "11. Experience with FastAPI:", "Are you holding offers?").
//! The items are pulled out of the full email (before any truncation) and
//! handed to the drafter as an explicit checklist; afterwards the draft is
//! checked so no item is silently skipped.

use std::sync::OnceLock;

use regex::Regex;

/// Fewer items than this is an ordinary email, not a questionnaire.
pub const MIN_ITEMS: usize = 4;
const MAX_ITEMS: usize = 60;

fn prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(?:[-*•▪◦·]\s*)?(?:\(?(?:\d{1,2}|[a-zA-Z])[.)]\s+)?(?:[-*•▪◦·]\s*)?").expect("valid regex")
    })
}

fn numbered_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\(?\d{1,2}[.)]\s+").expect("valid regex"))
}

const ASK_WORDS: &[&str] = &[
    "describe", "share", "confirm", "briefly", "mention", "provide", "let us know", "are you", "have you",
    "do you", "would you", "can you",
];

/// Lines starting with these are greetings or section headings, not items.
const NOT_START: &[&str] = &[
    "regards", "dear ", "hi ", "hello", "thanks", "thank you", "note:", "subject:", "to apply", "job description",
    "key responsibilities", "responsibilities", "requirements", "required skills", "preferred skills",
];
/// Lines containing these introduce a list rather than ask something.
const NOT_CONTAIN: &[&str] = &["the following", "below"];

/// The items a recruiter asks the candidate to fill in, in order.
pub fn extract(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in body.lines() {
        let line = raw.trim();
        if line.len() < 4 || line.chars().count() > 160 {
            continue;
        }
        let numbered = numbered_re().is_match(line);
        let item = prefix_re().replace(line, "").trim().to_string();
        let lower = item.to_lowercase();
        if item.len() < 4 || NOT_START.iter().any(|w| lower.starts_with(w)) || NOT_CONTAIN.iter().any(|w| lower.contains(w)) {
            continue;
        }
        let blank_field = item.ends_with(':') && !item[..item.len() - 1].contains(':');
        let question = item.ends_with('?');
        let numbered_ask = numbered && ASK_WORDS.iter().any(|w| lower.contains(w));
        if !(blank_field || question || numbered_ask) {
            continue;
        }
        let item = item.trim_end_matches(':').trim().to_string();
        if !out.iter().any(|x| x.eq_ignore_ascii_case(&item)) {
            out.push(item);
        }
        if out.len() >= MAX_ITEMS {
            break;
        }
    }
    if out.len() >= MIN_ITEMS {
        out
    } else {
        Vec::new()
    }
}

/// Prompt section listing every item to answer.
pub fn prompt_section(items: &[String]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "\nQUESTIONNAIRE: the email asks for the items below. Reply with ALL of them as a numbered list, in this \
order, one line each, formatted `N. <item>: <answer>`. Use FACTS, KNOWLEDGE and my skills only. When the \
answer isn't known, write `Will confirm` (or `Happy to discuss on a call` for opinions or reasons). Never \
invent offers, figures, dates or experience.\n",
    );
    for (i, it) in items.iter().enumerate() {
        s.push_str(&format!("{}. {}\n", i + 1, it));
    }
    s
}

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(str::to_string)
        .collect()
}

/// Items the draft doesn't answer. An item counts as answered when the draft
/// has a line containing its first few significant words.
pub fn missing<'a>(items: &'a [String], draft: &str) -> Vec<&'a String> {
    let lines: Vec<Vec<String>> = draft.lines().map(words).collect();
    items
        .iter()
        .filter(|it| {
            let key: Vec<String> = words(it).into_iter().take(3).collect();
            if key.is_empty() {
                return false;
            }
            !lines.iter().any(|l| key.iter().all(|k| l.contains(k)))
        })
        .collect()
}

/// Add still-unanswered items before the sign-off, marked to be confirmed.
pub fn append_missing(draft: &str, missing: &[&String]) -> String {
    if missing.is_empty() {
        return draft.to_string();
    }
    let block = format!(
        "Other details:\n{}",
        missing.iter().map(|m| format!("- {m}: Will confirm")).collect::<Vec<_>>().join("\n")
    );
    let lines: Vec<&str> = draft.trim_end().lines().collect();
    match lines.iter().rposition(|l| crate::agents::drafter::is_signoff(l)) {
        Some(i) => format!(
            "{}\n\n{block}\n\n{}",
            lines[..i].join("\n").trim_end(),
            lines[i..].join("\n")
        ),
        None => format!("{}\n\n{block}", draft.trim_end()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORM: &str = "Dear Candidate,\nPlease share the following details along with your resume:\n\
1. Job Details\nJob Title: Python Developer\nA. Personal and Professional Details\n1. Full Name:\n\
2. Contact Number:\n* Current Location (City):\n9. Total Years of IT Experience:\n\
11. Experience with FastAPI:\n21. Briefly describe your current project, key responsibilities, and the technologies you use.\n\
31. Are you currently holding any offers? If yes, please share the offered CTC.\n\
35. Please confirm your availability for an initial screening discussion.\n6. Application Process\n\
Best regards,\nRecruitment Team";

    #[test]
    fn extracts_form_items_in_order() {
        let items = extract(FORM);
        assert_eq!(
            items,
            vec![
                "Full Name",
                "Contact Number",
                "Current Location (City)",
                "Total Years of IT Experience",
                "Experience with FastAPI",
                "Briefly describe your current project, key responsibilities, and the technologies you use.",
                "Are you currently holding any offers? If yes, please share the offered CTC.",
                "Please confirm your availability for an initial screening discussion.",
            ]
        );
    }

    #[test]
    fn ordinary_emails_are_not_questionnaires() {
        assert!(extract("Hi Shashi,\nAre you open to a call tomorrow?\nThanks,\nPriya").is_empty());
        assert!(extract("Job Title: Python\nLocation: Pune\nExperience: 5+ years").is_empty());
    }

    #[test]
    fn missing_items_are_found_and_appended() {
        let items: Vec<String> = ["Full Name", "Contact Number", "Experience with FastAPI", "Reason for Looking for a Change"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let draft = "Hi,\n1. Full Name: Shashi\n2. Contact Number: +91 1\n3. Experience with FastAPI: 5 years\n\nBest regards,\nShashi";
        let m = missing(&items, draft);
        assert_eq!(m, vec![&items[3]]);
        let out = append_missing(draft, &m);
        assert!(out.contains("Other details:\n- Reason for Looking for a Change: Will confirm\n\nBest regards,"), "{out}");
    }
}
