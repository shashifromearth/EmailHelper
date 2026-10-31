//! Small text helpers shared by the services.

/// Skills recognised in job texts and resumes.
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

/// Whole-word, case-sensitive match (callers lower-case both sides).
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

/// "Jane Doe <Jane@Example.com>" → "jane@example.com".
pub fn email_address(from: &str) -> String {
    let s = from.trim();
    if let (Some(l), Some(r)) = (s.rfind('<'), s.rfind('>')) {
        if l < r {
            return s[l + 1..r].trim().to_ascii_lowercase();
        }
    }
    s.trim_matches(|c| c == '"' || c == '\'')
        .to_ascii_lowercase()
}

/// Build a safe FTS5 MATCH expression from free text: alphanumeric terms of
/// length >= 3, quoted and OR-ed together. Returns `None` if nothing usable.
pub fn fts_query(text: &str, max_terms: usize) -> Option<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "you", "your", "with", "this", "that", "are", "was", "have", "has",
        "but", "not", "from", "our", "can", "will", "would", "could", "please", "thanks", "thank",
        "hello", "regards", "best", "dear", "what", "when", "where", "which", "who", "how",
        "about", "there", "their", "they", "them", "been", "were", "also", "just", "any", "all",
        "out",
    ];
    let mut terms: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        let t = raw.to_lowercase();
        if t.chars().count() < 3 || STOP.contains(&t.as_str()) || terms.contains(&t) {
            continue;
        }
        terms.push(t);
        if terms.len() >= max_terms {
            break;
        }
    }
    if terms.is_empty() {
        return None;
    }
    Some(
        terms
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

/// Senders that must never enter the system (EH_EXCLUDED_SENDERS): exact
/// addresses ("alerts@bank.com") and domains ("hdfcbank.com" or
/// "@hdfcbank.com", which also cover subdomains such as "mail.hdfcbank.com").
#[derive(Debug, Clone, Default)]
pub struct SenderFilter {
    addresses: Vec<String>,
    domains: Vec<String>,
}

impl SenderFilter {
    pub fn new(entries: &[String]) -> Self {
        let mut f = Self::default();
        for e in entries {
            let e = e.trim().to_ascii_lowercase();
            match e.split_once('@') {
                Some(("", d)) if !d.is_empty() => f.domains.push(d.to_string()),
                Some((_, d)) if !d.is_empty() => f.addresses.push(e.clone()),
                None if !e.is_empty() => f.domains.push(e.clone()),
                _ => {}
            }
        }
        f
    }

    pub fn is_empty(&self) -> bool {
        self.addresses.is_empty() && self.domains.is_empty()
    }

    /// Is this From header ("HDFC Bank <alerts@hdfcbank.com>") excluded?
    pub fn excludes(&self, from: &str) -> bool {
        let addr = email_address(from);
        let domain = addr.rsplit_once('@').map(|(_, d)| d).unwrap_or("");
        self.addresses.contains(&addr)
            || (!domain.is_empty()
                && self.domains.iter().any(|d| domain == d || domain.ends_with(&format!(".{d}"))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_and_addresses() {
        assert!(contains_word("senior java developer", "java"));
        assert!(!contains_word("javascript developer", "java"));
        assert_eq!(email_address("Jane <Jane@Example.com>"), "jane@example.com");
    }

    #[test]
    fn sender_filter_matches_addresses_and_domains() {
        let f = SenderFilter::new(&["hdfcbank.com".into(), "@icicibank.com".into(), "Alerts@Paytm.com".into(), " ".into()]);
        assert!(f.excludes("HDFC Bank <InstaAlerts@hdfcbank.com>"));
        assert!(f.excludes("noreply@mail.hdfcbank.com"), "subdomains are covered");
        assert!(f.excludes("x@icicibank.com"));
        assert!(f.excludes("alerts@paytm.com"));
        assert!(!f.excludes("support@paytm.com"), "an exact address excludes only itself");
        assert!(!f.excludes("me@nothdfcbank.com"), "not a subdomain");
        assert!(SenderFilter::new(&[]).is_empty());
    }
}
