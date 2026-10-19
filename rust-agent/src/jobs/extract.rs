//! Job detail extraction from a rendered page (browser worker) or plain HTML:
//! schema.org JobPosting JSON-LD first, heuristics second.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use crate::db::jobs::JobDetails;

/// Page content as returned by the browser worker's `/v1/extract`.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct ExtractedPage {
    #[serde(default)]
    pub final_url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub json_ld: Vec<Value>,
    #[serde(default)]
    pub links: Vec<PageLink>,
    #[serde(default)]
    pub blocker: Option<crate::jobs::browser::Blocker>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct PageLink {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub href: String,
}

/// Parse a static HTML document into an [`ExtractedPage`] (no JS execution).
pub fn page_from_html(url: &str, html: &str) -> ExtractedPage {
    let doc = scraper::Html::parse_document(html);
    let sel = |s: &str| scraper::Selector::parse(s).expect("valid selector");
    let title = doc
        .select(&sel("title"))
        .next()
        .map(|t| t.text().collect::<String>().trim().to_string())
        .unwrap_or_default();
    let mut json_ld = Vec::new();
    for s in doc.select(&sel(r#"script[type="application/ld+json"]"#)) {
        let raw = s.text().collect::<String>();
        if let Ok(v) = serde_json::from_str::<Value>(raw.trim()) {
            flatten_ld(v, &mut json_ld);
        }
    }
    let base = url::Url::parse(url).ok();
    let links = doc
        .select(&sel("a[href]"))
        .filter_map(|a| {
            let href = a.value().attr("href")?;
            let abs = match &base {
                Some(b) => b.join(href).ok()?.to_string(),
                None => href.to_string(),
            };
            Some(PageLink {
                text: a.text().collect::<String>().trim().to_string(),
                href: abs,
            })
        })
        .take(200)
        .collect();
    let body_sel = sel("body");
    let text = doc
        .select(&body_sel)
        .next()
        .map(|b| visible_text(&b))
        .unwrap_or_default();
    ExtractedPage {
        final_url: url.to_string(),
        title,
        text: collapse_ws(&text).chars().take(40_000).collect(),
        json_ld,
        links,
        blocker: None,
    }
}

fn visible_text(el: &scraper::ElementRef<'_>) -> String {
    let mut out = String::new();
    for node in el.descendants() {
        if let Some(t) = node.value().as_text() {
            let hidden = node.ancestors().any(|a| {
                a.value()
                    .as_element()
                    .map(|e| matches!(e.name(), "script" | "style" | "noscript" | "template"))
                    .unwrap_or(false)
            });
            if !hidden {
                out.push_str(t);
                out.push(' ');
            }
        }
    }
    out
}

fn flatten_ld(v: Value, out: &mut Vec<Value>) {
    match v {
        Value::Array(a) => a.into_iter().for_each(|x| flatten_ld(x, out)),
        Value::Object(mut m) => {
            if let Some(g) = m.remove("@graph") {
                flatten_ld(g, out);
            }
            if !m.is_empty() {
                out.push(Value::Object(m));
            }
        }
        _ => {}
    }
}

pub fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Convert HTML fragments (job descriptions) to plain text.
pub fn html_to_text(html: &str) -> String {
    if !html.contains('<') && !html.contains("&lt;") {
        return html.to_string();
    }
    // Greenhouse returns HTML-escaped HTML.
    let unescaped = html
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&");
    let frag = scraper::Html::parse_fragment(&unescaped);
    collapse_ws(&frag.root_element().text().collect::<Vec<_>>().join(" "))
}

fn is_job_posting(v: &Value) -> bool {
    match &v["@type"] {
        Value::String(s) => s.eq_ignore_ascii_case("JobPosting"),
        Value::Array(a) => a.iter().any(|t| {
            t.as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("JobPosting"))
        }),
        _ => false,
    }
}

fn str_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn location_of(v: &Value) -> Option<String> {
    let one = |l: &Value| -> Option<String> {
        let a = &l["address"];
        if let Some(s) = str_of(a) {
            return Some(s);
        }
        let parts: Vec<String> = ["addressLocality", "addressRegion", "addressCountry"]
            .iter()
            .filter_map(|k| str_of(&a[*k]).or_else(|| str_of(&a[*k]["name"])))
            .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    };
    match v {
        Value::Array(a) => {
            let locs: Vec<String> = a.iter().filter_map(one).collect();
            (!locs.is_empty()).then(|| locs.join(" / "))
        }
        Value::Object(_) => one(v),
        _ => str_of(v),
    }
}

/// Build [`JobDetails`] from an extracted page.
pub fn parse_job(page: &ExtractedPage, skill_vocab: &[String]) -> JobDetails {
    let mut d = JobDetails {
        url: Some(page.final_url.clone()).filter(|u| !u.is_empty()),
        ..Default::default()
    };
    if let Some(jp) = page.json_ld.iter().find(|v| is_job_posting(v)) {
        d.title = str_of(&jp["title"]).unwrap_or_default();
        d.company =
            str_of(&jp["hiringOrganization"]["name"]).or_else(|| str_of(&jp["hiringOrganization"]));
        d.location = location_of(&jp["jobLocation"]);
        if jp["jobLocationType"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("TELECOMMUTE"))
        {
            d.remote = Some("remote".into());
        }
        let bs = &jp["baseSalary"];
        if bs.is_object() {
            d.salary_currency = str_of(&bs["currency"]);
            let val = &bs["value"];
            d.salary_min = val["minValue"].as_f64().or_else(|| val["value"].as_f64());
            d.salary_max = val["maxValue"].as_f64().or(d.salary_min);
            let unit = str_of(&val["unitText"]).unwrap_or_else(|| "YEAR".into());
            if let (Some(lo), Some(hi)) = (d.salary_min, d.salary_max) {
                d.salary = Some(
                    format!(
                        "{} {}–{} per {}",
                        d.salary_currency.clone().unwrap_or_default(),
                        lo,
                        hi,
                        unit.to_lowercase()
                    )
                    .trim()
                    .to_string(),
                );
            }
        }
        if let Some(desc) = str_of(&jp["description"]) {
            d.description = Some(html_to_text(&desc).chars().take(20_000).collect());
        }
        d.experience = str_of(&jp["experienceRequirements"]).or_else(|| {
            str_of(&jp["experienceRequirements"]["monthsOfExperience"])
                .map(|m| format!("{m} months"))
        });
        if let Some(skills) = str_of(&jp["skills"]) {
            d.skills = skills
                .split([',', ';'])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    if d.title.is_empty() {
        let (t, c) = split_title(&page.title);
        d.title = t;
        if d.company.is_none() {
            d.company = c;
        }
    }
    let text = d.description.clone().unwrap_or_else(|| page.text.clone());
    if d.description.is_none() && !page.text.is_empty() {
        d.description = Some(page.text.chars().take(20_000).collect());
    }
    enrich_from_text(&mut d, &text, skill_vocab);
    d
}

/// "Staff AI Engineer - Acme" / "Staff AI Engineer at Acme" / "Acme - Staff AI Engineer"
pub fn split_title(t: &str) -> (String, Option<String>) {
    let t = t.trim();
    let lower = t.to_lowercase();
    if let Some(i) = lower.find(" at ") {
        let company = t[i + 4..]
            .split(['|', '-', '–'])
            .next()
            .unwrap_or("")
            .trim();
        return (
            t[..i].trim().to_string(),
            Some(company.to_string()).filter(|c| !c.is_empty()),
        );
    }
    for sep in [" | ", " - ", " – ", " — "] {
        if let Some((a, b)) = t.split_once(sep) {
            let job_words = [
                "engineer",
                "architect",
                "developer",
                "manager",
                "scientist",
                "lead",
                "head",
                "director",
            ];
            let a_is_title = job_words.iter().any(|w| a.to_lowercase().contains(w));
            let b = b.split(sep).next().unwrap_or(b);
            return if a_is_title {
                (a.trim().to_string(), Some(b.trim().to_string()))
            } else {
                (b.trim().to_string(), Some(a.trim().to_string()))
            };
        }
    }
    (t.to_string(), None)
}

pub const COMMON_SKILLS: &[&str] = &[
    "Rust",
    "Python",
    "Go",
    "Java",
    "TypeScript",
    "JavaScript",
    "C++",
    "Scala",
    "Kotlin",
    "SQL",
    "PyTorch",
    "TensorFlow",
    "JAX",
    "LLM",
    "LLMs",
    "RAG",
    "LangChain",
    "LlamaIndex",
    "Transformers",
    "Hugging Face",
    "MLOps",
    "Kubernetes",
    "Docker",
    "Terraform",
    "AWS",
    "GCP",
    "Azure",
    "Kafka",
    "Spark",
    "Airflow",
    "PostgreSQL",
    "Redis",
    "Elasticsearch",
    "Vector databases",
    "gRPC",
    "Tokio",
    "Machine Learning",
    "Deep Learning",
    "NLP",
    "Computer Vision",
    "Generative AI",
    "GenAI",
    "Prompt Engineering",
    "Fine-tuning",
    "Distributed Systems",
    "Microservices",
    "React",
];

pub fn enrich_from_text(d: &mut JobDetails, text: &str, skill_vocab: &[String]) {
    let lower = text.to_lowercase();
    if d.remote.is_none() {
        d.remote = Some(remote_mode(d.location.as_deref().unwrap_or(""), &lower).to_string());
    }
    if d.experience_years.is_none() {
        static R: OnceLock<Regex> = OnceLock::new();
        let re = R.get_or_init(|| {
            Regex::new(r"(?i)(\d{1,2})\s*\+?\s*(?:-|to|–)?\s*(?:\d{1,2})?\s*\+?\s*(?:years|yrs)")
                .expect("valid regex")
        });
        if let Some(c) = re.captures(text) {
            d.experience_years = c.get(1).and_then(|m| m.as_str().parse().ok());
            if d.experience.is_none() {
                d.experience = Some(c.get(0).map(|m| m.as_str().to_string()).unwrap_or_default());
            }
        }
    }
    if d.salary.is_none() {
        static R: OnceLock<Regex> = OnceLock::new();
        let re = R.get_or_init(|| {
            Regex::new(r"(?i)((?:₹|inr|rs\.?)\s?[\d,.]+\s?(?:-|to|–)\s?[\d,.]+\s?(?:lpa|lakhs?|cr)?|[\d.]+\s?(?:-|to|–)\s?[\d.]+\s?lpa|[$€£]\s?[\d,]+k?\s?(?:-|to|–)\s?[$€£]?\s?[\d,]+k?)")
                .expect("valid regex")
        });
        if let Some(m) = re.find(text) {
            let s = m.as_str().to_string();
            let (lo, hi, cur) = parse_salary_range(&s);
            d.salary = Some(s);
            d.salary_min = d.salary_min.or(lo);
            d.salary_max = d.salary_max.or(hi);
            d.salary_currency = d.salary_currency.clone().or(cur);
        }
    }
    let mut vocab: Vec<String> = COMMON_SKILLS.iter().map(|s| s.to_string()).collect();
    for s in skill_vocab {
        if !vocab.iter().any(|v| v.eq_ignore_ascii_case(s)) {
            vocab.push(s.clone());
        }
    }
    for s in vocab {
        if d.skills.iter().any(|x| x.eq_ignore_ascii_case(&s)) {
            continue;
        }
        if contains_word(&lower, &s.to_lowercase()) {
            d.skills.push(s);
        }
    }
}

/// Work mode from the location field first (most reliable), then from strong
/// phrases in the description. A bare mention of "remote" in a description
/// ("remote work policy", "remote teams") is not enough.
pub fn remote_mode(location: &str, lower_text: &str) -> &'static str {
    let loc = location.to_lowercase();
    if loc.contains("remote") {
        return if loc.contains("hybrid") {
            "hybrid"
        } else {
            "remote"
        };
    }
    if loc.contains("hybrid") {
        return "hybrid";
    }
    const STRONG_REMOTE: &[&str] = &[
        "fully remote",
        "100% remote",
        "remote-first",
        "remote first",
        "(remote)",
        "remote (",
        "work from home",
        "remote role",
        "remote position",
        "remote opportunity",
        "this role is remote",
        "this is a remote",
        "location: remote",
        "remote-friendly",
        "work remotely",
        "- remote",
        "remote -",
        "remote, ",
    ];
    if STRONG_REMOTE.iter().any(|p| lower_text.contains(p)) || lower_text.starts_with("remote") {
        if lower_text.contains("hybrid") {
            "hybrid"
        } else {
            "remote"
        }
    } else if lower_text.contains("hybrid") {
        "hybrid"
    } else if ["on-site", "onsite", "in office", "in-office"]
        .iter()
        .any(|p| lower_text.contains(p))
    {
        "onsite"
    } else {
        "unknown"
    }
}

/// Whole-word (ASCII boundary) containment so "Go" doesn't match "good".
pub fn contains_word(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = hay.as_bytes();
    let mut start = 0;
    while let Some(pos) = hay[start..].find(needle) {
        let i = start + pos;
        let j = i + needle.len();
        let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        let after_ok = j >= bytes.len() || !bytes[j].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
        start = i + needle.len().max(1);
        if start >= hay.len() {
            break;
        }
        while !hay.is_char_boundary(start) {
            start += 1;
        }
    }
    false
}

/// Parse "₹40 - 60 LPA", "$180k - $220k", "30-45 LPA" into annual numbers.
pub fn parse_salary_range(s: &str) -> (Option<f64>, Option<f64>, Option<String>) {
    let lower = s.to_lowercase();
    let cur = if lower.contains('₹')
        || lower.contains("inr")
        || lower.contains("rs")
        || lower.contains("lpa")
        || lower.contains("lakh")
    {
        Some("INR".to_string())
    } else if lower.contains('$') {
        Some("USD".to_string())
    } else if lower.contains('€') {
        Some("EUR".to_string())
    } else if lower.contains('£') {
        Some("GBP".to_string())
    } else {
        None
    };
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"(\d[\d,]*(?:\.\d+)?)\s*(k)?").expect("valid regex"));
    let mult = if lower.contains("lpa") || lower.contains("lakh") {
        100_000.0
    } else if lower.contains("cr") {
        10_000_000.0
    } else {
        1.0
    };
    let nums: Vec<f64> = re
        .captures_iter(&lower)
        .filter_map(|c| {
            let n: f64 = c.get(1)?.as_str().replace(',', "").parse().ok()?;
            let k = if c.get(2).is_some() { 1000.0 } else { 1.0 };
            Some(n * k * mult)
        })
        .collect();
    (
        nums.first().copied(),
        nums.get(1).copied().or(nums.first().copied()),
        cur,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GH: &str = r#"<html><head><title>Job Application for Staff AI Engineer at Acme</title>
<script type="application/ld+json">{"@context":"https://schema.org","@type":"JobPosting","title":"Staff AI Engineer",
"hiringOrganization":{"@type":"Organization","name":"Acme"},"jobLocationType":"TELECOMMUTE",
"jobLocation":{"@type":"Place","address":{"addressLocality":"Bengaluru","addressCountry":"IN"}},
"baseSalary":{"@type":"MonetaryAmount","currency":"INR","value":{"@type":"QuantitativeValue","minValue":4500000,"maxValue":6500000,"unitText":"YEAR"}},
"description":"&lt;p&gt;We need 8+ years building ML systems with Python, Rust and Kubernetes. Experience with RAG and LLMs.&lt;/p&gt;"}</script>
</head><body><h1>Staff AI Engineer</h1><script>var x=1;</script><a href="/acme/jobs/1/apply">Apply</a></body></html>"#;

    #[test]
    fn parses_json_ld() {
        let p = page_from_html("https://boards.greenhouse.io/acme/jobs/1", GH);
        assert_eq!(p.json_ld.len(), 1);
        assert!(!p.text.contains("var x"));
        assert_eq!(
            p.links[0].href,
            "https://boards.greenhouse.io/acme/jobs/1/apply"
        );
        let d = parse_job(&p, &[]);
        assert_eq!(d.title, "Staff AI Engineer");
        assert_eq!(d.company.as_deref(), Some("Acme"));
        assert_eq!(d.remote.as_deref(), Some("remote"));
        assert_eq!(d.location.as_deref(), Some("Bengaluru, IN"));
        assert_eq!(d.salary_min, Some(4_500_000.0));
        assert_eq!(d.salary_currency.as_deref(), Some("INR"));
        assert_eq!(d.experience_years, Some(8.0));
        for s in ["Python", "Rust", "Kubernetes", "RAG", "LLMs"] {
            assert!(
                d.skills.iter().any(|x| x == s),
                "missing {s}: {:?}",
                d.skills
            );
        }
        assert!(!d.skills.iter().any(|x| x == "Go"));
    }

    #[test]
    fn heuristics_without_json_ld() {
        let p = ExtractedPage {
            final_url: "https://careers.initech.com/jobs/42".into(),
            title: "Lead AI Engineer | Initech".into(),
            text: "Hybrid role in Pune. 5-8 years experience. Compensation ₹40 - 60 LPA. Python, PyTorch.".into(),
            ..Default::default()
        };
        let d = parse_job(&p, &[]);
        assert_eq!(d.title, "Lead AI Engineer");
        assert_eq!(d.company.as_deref(), Some("Initech"));
        assert_eq!(d.remote.as_deref(), Some("hybrid"));
        assert_eq!(d.experience_years, Some(5.0));
        assert_eq!(d.salary_min, Some(4_000_000.0));
        assert_eq!(d.salary_max, Some(6_000_000.0));
    }

    #[test]
    fn salary_parsing() {
        let (lo, hi, c) = parse_salary_range("$180k - $220k");
        assert_eq!(
            (lo, hi, c.as_deref()),
            (Some(180_000.0), Some(220_000.0), Some("USD"))
        );
    }

    #[test]
    fn word_boundaries() {
        assert!(contains_word("we use go and rust", "go"));
        assert!(!contains_word("good engineers", "go"));
        assert!(contains_word("c++ experience", "c++"));
    }

    #[test]
    fn title_splitting() {
        assert_eq!(
            split_title("Acme - Staff AI Engineer"),
            ("Staff AI Engineer".into(), Some("Acme".into()))
        );
        assert_eq!(
            split_title("Rust Engineer at Globex | Careers"),
            ("Rust Engineer".into(), Some("Globex".into()))
        );
    }

    #[test]
    fn remote_mode_prefers_location_and_strong_phrases() {
        assert_eq!(
            remote_mode("Tokyo, Japan", "we support remote teams across time zones"),
            "unknown"
        );
        assert_eq!(remote_mode("Bangalore - Remote", "build systems"), "remote");
        assert_eq!(remote_mode("", "this is a fully remote role"), "remote");
        assert_eq!(remote_mode("", "hybrid, 3 days in office"), "hybrid");
        assert_eq!(
            remote_mode("Berlin", "on-site in our berlin office"),
            "onsite"
        );
    }
}
