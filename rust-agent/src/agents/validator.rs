//! Agent 5: validation. Deterministic checks catch the common failure modes
//! (empty, placeholders, leaked prompts, invented contact details or prices,
//! risky promises, prompt injection); an optional LLM review adds a second
//! opinion. Anything suspicious becomes REVIEW_REQUIRED, never a hard failure.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::llm::{LlmRequest, LlmRouter, LlmTask};
use crate::prompts;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Verdict {
    Approved,
    ReviewRequired,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Approved => "APPROVED",
            Self::ReviewRequired => "REVIEW_REQUIRED",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Validation {
    pub verdict: Verdict,
    pub notes: Vec<String>,
    pub confidence: f32,
}

pub struct ValidationInput<'a> {
    pub draft: &'a str,
    /// Trusted text the draft may draw facts from (knowledge, FACTS, the
    /// person's own details).
    pub grounding: &'a str,
    /// Third-party text (incoming email + thread). Contact details, links and
    /// amounts found only here are flagged, never accepted.
    pub incoming: &'a str,
    /// The sender's address; replying to it is always fine.
    pub sender: &'a str,
    pub classification_confidence: f32,
    pub injection_flags: &'a [String],
}

/// Commitments a draft must not make on the person's behalf unless a trusted
/// source says so. `true` marks phrases followed by a date, which are fine
/// when that date is in the trusted sources (e.g. a joining date in FACTS).
const COMMITMENTS: &[(&str, bool)] = &[
    ("i accept the offer", false),
    ("i accept your offer", false),
    ("i confirm i will join", false),
    ("i will join on", true),
    ("we will deliver by", false),
    ("i agree to the terms", false),
    ("i am available to start on", true),
];

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("valid regex"))
}

fn emails_in(s: &str) -> HashSet<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}")
        .find_iter(s)
        .map(|m| m.as_str().to_ascii_lowercase())
        .collect()
}

fn urls_in(s: &str) -> HashSet<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r#"(?i)\bhttps?://[^\s<>"')]+"#)
        .find_iter(s)
        .map(|m| m.as_str().trim_end_matches(['.', ',']).to_ascii_lowercase())
        .collect()
}

fn money_in(s: &str) -> HashSet<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)(?:[$€£₹]\s?\d[\d,]*(?:\.\d+)?|\b\d[\d,]*(?:\.\d+)?\s?(?:usd|eur|gbp|inr|lpa|dollars|euros|rupees)\b)")
        .find_iter(s)
        .map(|m| {
            m.as_str()
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '.')
                .collect::<String>()
        })
        .filter(|d| !d.is_empty())
        .collect()
}

fn phones_in(s: &str) -> HashSet<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    // Spaces and tabs only: a number never continues on the next line
    // ("+91 98…" followed by a "3." list number).
    re(&R, r"\+?\d[\d \t().-]{8,}\d")
        .find_iter(s)
        .map(|m| {
            m.as_str()
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect::<String>()
        })
        .filter(|d| d.len() >= 9)
        .collect()
}

/// Byte offsets where `phrase` occurs as whole words in `text`.
fn phrase_at(text: &str, phrase: &str) -> Vec<usize> {
    text.match_indices(phrase)
        .map(|(i, _)| i)
        .filter(|&i| {
            let before = text[..i].chars().next_back();
            let after = text[i + phrase.len()..].chars().next();
            !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
        })
        .collect()
}

/// The date after a dated commitment ("… start on 1 December 2026") is backed
/// when a trusted source contains it.
fn date_backed(tail: &str, grounding_lower: &str) -> bool {
    let words: Vec<&str> = tail.split_whitespace().take(4).collect();
    (1..=words.len()).any(|n| {
        let joined = words[..n].join(" ");
        let d = joined.trim_end_matches(|c: char| !c.is_alphanumeric());
        d.len() >= 6 && d.chars().any(|c| c.is_ascii_digit()) && !phrase_at(grounding_lower, d).is_empty()
    })
}

fn commitment_notes(draft_lower: &str, grounding_lower: &str) -> Vec<String> {
    let text = draft_lower.replace('\u{2019}', "'").replace("i'm ", "i am ");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut notes = Vec::new();
    for (phrase, dated) in COMMITMENTS {
        if !phrase_at(grounding_lower, phrase).is_empty() {
            continue;
        }
        let unbacked = phrase_at(&text, phrase)
            .into_iter()
            .any(|at| !*dated || !date_backed(&text[at + phrase.len()..], grounding_lower));
        if unbacked {
            notes.push(format!("policy: commitment \"{phrase}\" not backed by knowledge base or facts"));
        }
    }
    notes
}

/// True when a digit run isn't part of a longer number ("+91 …", a card).
fn isolated(text: &str, start: usize, end: usize) -> bool {
    let joins = |c: &[char]| match c {
        [d, ..] if d.is_ascii_digit() || *d == '+' => true,
        [s, d, ..] => (*s == ' ' || *s == '-') && d.is_ascii_digit(),
        _ => false,
    };
    let before: Vec<char> = text[..start].chars().rev().take(2).collect();
    let after: Vec<char> = text[end..].chars().take(2).collect();
    !joins(&before) && !joins(&after)
}

fn luhn(digits: &str) -> bool {
    let sum: u32 = digits
        .chars()
        .rev()
        .filter_map(|c| c.to_digit(10))
        .enumerate()
        .map(|(i, d)| match (i % 2 == 1, d * 2) {
            (true, x) if x > 9 => x - 9,
            (true, x) => x,
            _ => d,
        })
        .sum();
    sum.is_multiple_of(10)
}

fn iban_valid(compact: &str) -> bool {
    if !(15..=34).contains(&compact.len()) {
        return false;
    }
    let (head, tail) = compact.split_at(4);
    let mut rem: u32 = 0;
    for c in tail.chars().chain(head.chars()) {
        let Some(v) = c.to_digit(36) else {
            return false;
        };
        rem = if v >= 10 { (rem * 100 + v) % 97 } else { (rem * 10 + v) % 97 };
    }
    rem == 1
}

/// Government IDs and financial identifiers in a final draft, by kind (never
/// the values). Values in `approved` (the person's private facts, filled in
/// from placeholders) are skipped.
pub fn pii_kinds(text: &str, approved: &[&str]) -> Vec<&'static str> {
    fn norm(s: &str) -> String {
        s.chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase()
    }
    let approved: Vec<String> = approved.iter().map(|a| norm(a)).filter(|a| !a.is_empty()).collect();
    let ok = |v: &str| approved.contains(&norm(v));
    static PAN: OnceLock<Regex> = OnceLock::new();
    static AADHAAR: OnceLock<Regex> = OnceLock::new();
    static SSN: OnceLock<Regex> = OnceLock::new();
    static CARD: OnceLock<Regex> = OnceLock::new();
    static IBAN: OnceLock<Regex> = OnceLock::new();
    static DOB: OnceLock<Regex> = OnceLock::new();
    let mut kinds = Vec::new();
    let mut add = |k: &'static str| {
        if !kinds.contains(&k) {
            kinds.push(k);
        }
    };
    if re(&PAN, r"\b[A-Z]{5}[0-9]{4}[A-Z]\b").find_iter(text).any(|m| !ok(m.as_str())) {
        add("PAN");
    }
    if re(&AADHAAR, r"\b[2-9]\d{3}[ -]?\d{4}[ -]?\d{4}\b")
        .find_iter(text)
        .any(|m| isolated(text, m.start(), m.end()) && !ok(m.as_str()))
    {
        add("Aadhaar number");
    }
    if re(&SSN, r"\b\d{3}-\d{2}-\d{4}\b")
        .find_iter(text)
        .any(|m| isolated(text, m.start(), m.end()) && !ok(m.as_str()))
    {
        add("SSN");
    }
    if re(&CARD, r"\b\d(?:[ -]?\d){12,18}\b").find_iter(text).any(|m| {
        isolated(text, m.start(), m.end()) && luhn(&norm(m.as_str())) && !ok(m.as_str())
    }) {
        add("card number");
    }
    if re(&IBAN, r"\b[A-Z]{2}\d{2}(?: ?[A-Z0-9]{4}){2,7}(?: ?[A-Z0-9]{1,3})?\b")
        .find_iter(text)
        .any(|m| iban_valid(&m.as_str().replace(' ', "")) && !ok(m.as_str()))
    {
        add("IBAN");
    }
    const MONTH: &str = "(?:jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\\.?";
    let dob = re(
        &DOB,
        &format!(
            r"(?i)(?:\bdob\b|\bd\.o\.b\b\.?|\bdate of birth\b|\bbirth ?date\b|\bborn on\b)[^\d\n.;]{{0,25}}?(\d{{1,2}}[-/. ]\d{{1,2}}[-/. ]\d{{2,4}}|\d{{4}}[-/.]\d{{1,2}}[-/.]\d{{1,2}}|\d{{1,2}}(?:st|nd|rd|th)?[ -]{MONTH},?[ -]\d{{2,4}}|{MONTH} \d{{1,2}}(?:st|nd|rd|th)?,? \d{{4}})"
        ),
    );
    if dob.captures_iter(text).any(|c| c.get(1).is_some_and(|d| !ok(d.as_str()))) {
        add("date of birth");
    }
    kinds
}

/// Deterministic validation rules.
pub fn rule_checks(i: &ValidationInput<'_>) -> Vec<String> {
    let mut notes = Vec::new();
    let draft = i.draft.trim();
    let lower = draft.to_lowercase();
    let word_count = draft.split_whitespace().count();

    if draft.is_empty() {
        notes.push("empty response".into());
        return notes;
    }
    if word_count < 8 {
        notes.push(format!("response too short ({word_count} words)"));
    }
    if word_count > 600 {
        notes.push(format!("response too long ({word_count} words)"));
    }
    static PLACEHOLDER: OnceLock<Regex> = OnceLock::new();
    if re(&PLACEHOLDER, r"(?i)\[[^\]\n]{0,40}\b(your|insert|recipient|company|organi[sz]ation|name|date|time|title|address|phone|email|position|role|link|url)\b[^\]\n]{0,40}\]|\{\{[^}]*\}\}|<insert|lorem ipsum|xx/xx")
        .is_match(draft)
    {
        notes.push("contains unfilled placeholder".into());
    }
    for leak in [
        "untrusted",
        "system prompt",
        "as an ai language model",
        "<<<",
        ">>>",
        "i cannot help with",
    ] {
        if lower.contains(leak) {
            notes.push(format!("possible prompt leakage or refusal (\"{leak}\")"));
            break;
        }
    }
    static PROMISE: OnceLock<Regex> = OnceLock::new();
    let grounding_lower = i.grounding.to_lowercase();
    for m in re(&PROMISE, r"(?i)\b(full refund|refund (you|your)|we will refund|guarantee[ds]?|discount of|% (off|discount)|free of charge|legally binding|we accept liability|waive)\b")
        .find_iter(draft)
    {
        if !grounding_lower.contains(&m.as_str().to_lowercase()) {
            notes.push(format!("policy: commitment \"{}\" not backed by knowledge base", m.as_str()));
        }
    }
    notes.extend(commitment_notes(&lower, &grounding_lower));
    // Only trusted text grounds contact details and amounts. One that appears
    // only in the incoming email (e.g. a phishing "pay here" link) is echoed,
    // not grounded, so it goes to review. Replying to the sender is fine.
    let sender = i.sender.trim().to_ascii_lowercase();
    let mut echoed: Vec<String> = Vec::new();
    let (ground_emails, in_emails) = (emails_in(i.grounding), emails_in(i.incoming));
    for e in emails_in(draft).difference(&ground_emails) {
        if *e == sender {
            continue;
        }
        if in_emails.contains(e) {
            echoed.push(format!("email {e}"));
        } else {
            notes.push(format!(
                "hallucination risk: email address {e} not in sources"
            ));
        }
    }
    let known = |set: &HashSet<String>, u: &str| {
        set.iter()
            .any(|g| g.starts_with(u) || u.starts_with(g.as_str()))
    };
    let (ground_urls, in_urls) = (urls_in(i.grounding), urls_in(i.incoming));
    for u in urls_in(draft) {
        if known(&ground_urls, u.as_str()) {
            continue;
        }
        if known(&in_urls, u.as_str()) {
            echoed.push(format!("link {u}"));
        } else {
            notes.push(format!("hallucination risk: link {u} not in sources"));
        }
    }
    let (ground_money, in_money) = (money_in(i.grounding), money_in(i.incoming));
    for m in money_in(draft).difference(&ground_money) {
        if in_money.contains(m) {
            echoed.push(format!("amount {m}"));
        } else {
            notes.push(format!("hallucination risk: amount {m} not in sources"));
        }
    }
    let (ground_phones, in_phones) = (phones_in(i.grounding), phones_in(i.incoming));
    for p in phones_in(draft).difference(&ground_phones) {
        if in_phones.contains(p) {
            echoed.push(format!("phone {p}"));
        } else {
            notes.push(format!(
                "hallucination risk: phone number {p} not in sources"
            ));
        }
    }
    if !echoed.is_empty() {
        echoed.sort();
        notes.push(format!(
            "repeats contact details from the incoming email: {}",
            echoed.join(", ")
        ));
    }
    if !i.injection_flags.is_empty() {
        notes.push(format!(
            "incoming email contains possible prompt injection ({})",
            i.injection_flags.join(", ")
        ));
    }
    if i.classification_confidence < 0.5 {
        notes.push(format!(
            "low classification confidence ({:.2})",
            i.classification_confidence
        ));
    }
    notes
}

#[derive(Deserialize)]
struct LlmVerdict {
    verdict: String,
    #[serde(default)]
    issues: Vec<String>,
    #[serde(default)]
    confidence: Option<f32>,
}

/// Validate a draft. Rule checks always run; `use_llm` adds a second-opinion
/// LLM review (off by default for local models, where it costs a full call).
pub async fn validate(llm: &LlmRouter, i: &ValidationInput<'_>, use_llm: bool) -> Validation {
    let mut notes = rule_checks(i);
    if i.draft.trim().is_empty() {
        return Validation {
            verdict: Verdict::ReviewRequired,
            notes,
            confidence: 0.0,
        };
    }
    if !use_llm {
        let verdict = if notes.is_empty() {
            Verdict::Approved
        } else {
            Verdict::ReviewRequired
        };
        return Validation {
            verdict,
            notes,
            confidence: 0.75,
        };
    }
    let source = prompts::sanitize_untrusted(
        &format!("{}\n\nINCOMING EMAIL (third party):\n{}", i.grounding, i.incoming),
        6000,
    )
    .text;
    let draft = prompts::sanitize_untrusted(i.draft, 4000).text;
    let req = LlmRequest::new(
        LlmTask::Validate,
        prompts::validate_system(),
        prompts::validate_prompt(&source, &draft),
    )
    .json()
    .max_tokens(300);
    let confidence = match llm.complete_json::<LlmVerdict>(&req).await {
        Ok((v, _)) => {
            let confidence = v.confidence.unwrap_or(0.7).clamp(0.0, 1.0);
            if v.verdict.to_ascii_uppercase().contains("REVIEW") {
                if v.issues.is_empty() {
                    notes.push("LLM reviewer requested human review".into());
                }
                for issue in v.issues.into_iter().take(5) {
                    notes.push(format!(
                        "reviewer: {}",
                        issue.chars().take(200).collect::<String>()
                    ));
                }
            }
            if confidence < 0.5 {
                notes.push(format!("low reviewer confidence ({confidence:.2})"));
            }
            confidence
        }
        Err(e) => {
            tracing::warn!(error = %e, "LLM validation unavailable; rules only");
            notes.push("LLM review unavailable; rule checks only".into());
            0.5
        }
    };
    let verdict = if notes.is_empty() {
        Verdict::Approved
    } else {
        Verdict::ReviewRequired
    };
    Validation {
        verdict,
        notes,
        confidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(draft: &'a str, grounding: &'a str) -> ValidationInput<'a> {
        ValidationInput {
            draft,
            grounding,
            incoming: "",
            sender: "",
            classification_confidence: 0.9,
            injection_flags: &[],
        }
    }

    #[test]
    fn clean_draft_passes_rules() {
        let n = rule_checks(&input(
            "Hi Jane, thanks for asking. Our standard plan costs $49 per seat per month. Best regards, Asha",
            "Standard plan: $49 per seat per month.",
        ));
        assert!(n.is_empty(), "{n:?}");
    }

    #[test]
    fn flags_empty_and_placeholders() {
        assert_eq!(rule_checks(&input("   ", "x")), vec!["empty response"]);
        let n = rule_checks(&input(
            "Hello [Recipient Name], thank you for your note, we will reply by {{date}} soon.",
            "",
        ));
        assert!(n.iter().any(|x| x.contains("placeholder")));
        let n = rule_checks(&input(
            "Thank you for your interest in [Example Company]. We will be in touch with next steps soon.",
            "",
        ));
        assert!(n.iter().any(|x| x.contains("placeholder")), "{n:?}");
        let ok = rule_checks(&input(
            "Thanks for the note about the [RFC] process; I will review it and reply this week.",
            "[RFC] process",
        ));
        assert!(!ok.iter().any(|x| x.contains("placeholder")), "{ok:?}");
    }

    #[test]
    fn numbers_on_separate_lines_are_not_joined() {
        let n = rule_checks(&input(
            "1. Contact Number: +91 9000012345
3. Email Address: a@b.test
27. Last Working Day: 19-06-2026
28. Joining: Immediately",
            "Phone number: +91 9000012345
Email: a@b.test
Last working day: 19-06-2026",
        ));
        assert!(!n.iter().any(|x| x.contains("phone")), "{n:?}");
    }

    #[test]
    fn flags_invented_facts() {
        let n = rule_checks(&input(
            "Hi, the price is $99 and you can email billing@acme.com or visit https://acme.com/buy. Call +1 415 555 0100.",
            "Our plan is $49.",
        ));
        assert!(n.iter().any(|x| x.contains("amount 99")));
        assert!(n.iter().any(|x| x.contains("billing@acme.com")));
        assert!(n.iter().any(|x| x.contains("https://acme.com/buy")));
        assert!(n.iter().any(|x| x.contains("phone")));
    }

    #[test]
    fn flags_unbacked_promises() {
        let n = rule_checks(&input(
            "We are sorry. We will refund your order and offer a discount of 20% off next time, guaranteed.",
            "Refunds require manager approval.",
        ));
        assert!(n.iter().any(|x| x.starts_with("policy")));
    }

    #[test]
    fn flags_injection_and_low_confidence() {
        let flags = vec!["ignore_instructions".to_string()];
        let n = rule_checks(&ValidationInput {
            draft: "Thanks for the message, I will look into it and get back to you shortly.",
            grounding: "",
            incoming: "",
            sender: "",
            classification_confidence: 0.3,
            injection_flags: &flags,
        });
        assert!(n.iter().any(|x| x.contains("prompt injection")));
        assert!(n
            .iter()
            .any(|x| x.contains("low classification confidence")));
    }

    #[test]
    fn details_only_in_the_incoming_email_are_flagged() {
        let incoming = "From your bank: we guarantee a refund. Verify at https://secure-bank.example.net/login \
or call +1 415 555 0199, or write to help@secure-bank.example.net. Fee: $250.";
        let n = rule_checks(&ValidationInput {
            draft: "Hi, thanks. I will verify at https://secure-bank.example.net/login and call +1 415 555 0199 \
or email help@secure-bank.example.net about the $250 fee; you guarantee it.",
            grounding: "Standard plan: $49 per seat per month.",
            incoming,
            sender: "alerts@bank.example",
            classification_confidence: 0.9,
            injection_flags: &[],
        });
        let echo = n
            .iter()
            .find(|x| x.starts_with("repeats contact details from the incoming email"))
            .unwrap_or_else(|| panic!("{n:?}"));
        for part in ["help@secure-bank.example.net", "secure-bank.example.net/login", "amount 250", "phone 14155550199"] {
            assert!(echo.contains(part), "{echo}");
        }
        assert!(n.iter().any(|x| x.contains("\"guarantee\"")), "email wording is not a source: {n:?}");
        // Replying to the sender's own address, or using the person's own
        // details, is fine.
        let ok = rule_checks(&ValidationInput {
            draft: "Hi Jane, I have noted jane@acme.com and will write back from asha@me.example shortly.",
            grounding: "[my details]\nasha@me.example",
            incoming: "Please reply to jane@acme.com",
            sender: "Jane@Acme.com",
            classification_confidence: 0.9,
            injection_flags: &[],
        });
        assert!(ok.is_empty(), "{ok:?}");
    }

    #[test]
    fn flags_commitments_unless_backed() {
        let n = rule_checks(&input(
            "Thank you. I accept the offer and I agree to the terms. I am available to start on 1 December 2026.",
            "Notice period: 30 days",
        ));
        for p in ["i accept the offer", "i agree to the terms", "i am available to start on"] {
            assert!(n.iter().any(|x| x.contains(p)), "{p}: {n:?}");
        }
        // A start date taken from FACTS is fine; "I'm" counts as "I am".
        let ok = rule_checks(&input(
            "Thank you for the update. I'm available to start on 1 December 2026, as discussed earlier.",
            "FACTS (trusted):\nEarliest joining date: 1 December 2026",
        ));
        assert!(ok.is_empty(), "{ok:?}");
        let n = rule_checks(&input(
            "Thank you for the update. I am available to start on 1 December 2026, as discussed earlier.",
            "FACTS (trusted):\nEarliest joining date: 11 December 2026",
        ));
        assert!(n.iter().any(|x| x.contains("available to start on")), "{n:?}");
        // Whole phrases only.
        let ok = rule_checks(&input(
            "Before I accept the offering terms I need to review them with my team this week.",
            "",
        ));
        assert!(!ok.iter().any(|x| x.starts_with("policy")), "{ok:?}");
    }

    #[test]
    fn pii_scan_finds_ids_and_skips_approved_values() {
        let kinds = |t: &str| pii_kinds(t, &[]);
        assert_eq!(kinds("My PAN is ABCDE1234F."), vec!["PAN"]);
        assert_eq!(kinds("Aadhaar: 2345 6789 0123"), vec!["Aadhaar number"]);
        assert_eq!(kinds("Aadhaar: 234567890123"), vec!["Aadhaar number"]);
        assert_eq!(kinds("SSN 123-45-6789"), vec!["SSN"]);
        assert_eq!(kinds("Card: 4111 1111 1111 1111"), vec!["card number"]);
        assert_eq!(kinds("Card: 4111-1111-1111-1111"), vec!["card number"]);
        assert_eq!(kinds("IBAN GB82 WEST 1234 5698 7654 32 for the transfer"), vec!["IBAN"]);
        assert_eq!(kinds("IBAN GB82WEST12345698765432"), vec!["IBAN"]);
        assert_eq!(kinds("DOB: 02/01/1990"), vec!["date of birth"]);
        assert_eq!(kinds("My date of birth is 2 January 1990."), vec!["date of birth"]);
        assert_eq!(kinds("Date of birth (DD-MM-YYYY): 02-01-1990"), vec!["date of birth"]);
        assert_eq!(kinds("born on March 3, 1991"), vec!["date of birth"]);

        // Ordinary numbers are not IDs.
        for clean in [
            "Call me on +91 98765 43210 or +91 9000012345.",
            "Last working day: 19-06-2026. Expected CTC: 35 LPA.",
            "Card ending 4111 1111 1111 1112 (fails the checksum).",
            "We hire across PAN India; invoice INV-2026-0042.",
            "(Date of birth provided). Last working day: 19-06-2026",
            "IBAN GB00 WEST 1234 5698 7654 32 (bad checksum)",
            "Reference 1234 5678 9012 3456 7890 1234",
        ] {
            assert!(kinds(clean).is_empty(), "{clean}: {:?}", kinds(clean));
        }

        // Values the person approved (filled from placeholders) are allowed;
        // others in the same draft are not.
        let text = "PAN: ABCDE1234F, DOB: 02-01-1990, Aadhaar 2345-6789-0123";
        assert_eq!(pii_kinds(text, &["ABCDE1234F", "02-01-1990"]), vec!["Aadhaar number"]);
        assert!(pii_kinds(text, &["ABCDE1234F", "02-01-1990", "2345 6789 0123"]).is_empty());
    }
}
