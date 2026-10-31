//! What the research agent keeps: fresh postings in the person's tech stack,
//! open to applicants in the person's country. Remote jobs anywhere that
//! accepts them; on-site/hybrid jobs in that country, or abroad when the
//! employer offers visa sponsorship / relocation. Remote and
//! contract/freelance/consulting roles rank first (`priority`).

use chrono::{DateTime, Utc};

use crate::jobs::extract::contains_word;
use crate::research::sources::RawJob;

#[derive(Debug, Clone)]
pub struct Criteria {
    /// Postings older than this are dropped (None = no limit).
    pub max_age_hours: Option<u32>,
    /// Applicants live here ("India").
    pub country: String,
    /// Stack terms, e.g. python, c#, .net, java, genai, agentic ai.
    pub stack: Vec<String>,
    /// Role types (any of): contract, freelance, consultant. Empty = any.
    pub work_types: Vec<String>,
    /// The job must be remote / a distributed team.
    pub require_remote: bool,
}

/// Synonyms for a stack term (lower case).
fn stack_words(term: &str) -> Vec<&'static str> {
    match term.trim().to_lowercase().as_str() {
        "python" => vec!["python", "django", "fastapi", "flask"],
        "c#" | "csharp" => vec!["c#", "csharp", ".net", "dotnet", "asp.net"],
        ".net" | "dotnet" => vec![".net", "dotnet", "asp.net", "c#"],
        "java" => vec!["java", "spring boot", "spring", "kotlin", "j2ee", "jvm"],
        "genai" | "gen ai" | "generative ai" => vec![
            "genai", "gen ai", "generative ai", "llm", "llms", "large language model", "rag", "openai", "langchain",
            "ai engineer", "ai architect", "llmops", "prompt engineering", "vector database",
        ],
        "agentic ai" | "agentic" | "ai agents" => vec![
            "agentic", "ai agent", "ai agents", "multi-agent", "langgraph", "crewai", "autogen", "mcp",
            "semantic kernel", "autonomous agents",
        ],
        "llm" => vec!["llm", "llms", "large language model"],
        "rust" => vec!["rust", "rustlang"],
        // "go" alone is an everyday word; only clear uses count.
        "go" | "golang" => vec!["golang", "go developer", "go engineer", "go programmer", "go/golang", "go (golang)"],
        _ => vec![],
    }
}

fn has(text: &str, w: &str) -> bool {
    // Words with symbols (c#, .net, asp.net) are plain substrings.
    if w.chars().all(|c| c.is_alphanumeric() || c == ' ' || c == '-') {
        contains_word(text, w)
    } else {
        text.contains(w)
    }
}

/// How often `w` occurs in `text` as a whole word (symbol words as substrings).
fn count(text: &str, w: &str) -> usize {
    let symbolic = !w.chars().all(|c| c.is_alphanumeric() || c == ' ' || c == '-');
    let mut n = 0;
    let mut from = 0;
    while let Some(i) = text[from..].find(w) {
        let start = from + i;
        let end = start + w.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
        if symbolic || (boundary(before) && boundary(after)) {
            n += 1;
        }
        from = end;
    }
    n
}

/// Which stack terms the job is about: named in the title or tags, or
/// mentioned at least twice in the description (one passing mention, as in
/// "our art team also uses Python", is not enough).
pub fn stack_hits(j: &RawJob, stack: &[String]) -> Vec<String> {
    let head = format!("{} {}", j.details.title, j.details.skills.join(" ")).to_lowercase();
    let desc = j.details.description.as_deref().unwrap_or("").to_lowercase();
    stack
        .iter()
        .filter(|t| {
            let own = t.to_lowercase();
            let mut words: Vec<&str> = stack_words(t);
            if words.is_empty() {
                words = vec![own.as_str()];
            }
            words.iter().any(|w| has(&head, w)) || words.iter().map(|w| count(&desc, w)).sum::<usize>() >= 2
        })
        .cloned()
        .collect()
}

const ANYWHERE: &[&str] = &[
    "worldwide", "anywhere", "global", "international", "all countries", "no location restriction",
    "apac", "asia", "asia-pacific", "asia pacific", "south asia",
];
/// Phrases in a description that limit applicants to other places.
const RESTRICTED: &[&str] = &[
    "us only", "usa only", "u.s. only", "us-only", "us based only", "must be located in the us",
    "must be located in the united states", "must reside in the us", "must reside in the united states",
    "must be based in the us", "must be based in the united states", "authorized to work in the united states",
    "authorized to work in the us", "eligible to work in the us", "eligible to work in the united states",
    "us citizens", "u.s. citizens", "green card", "us work authorization", "within the united states",
    "within the us", "north america only", "canada only", "uk only", "eu only", "europe only",
    "emea only", "latam only", "must be located in europe", "must be based in europe", "right to work in the uk",
    "eligible to work in the eu", "must reside in canada", "located in the americas",
];
/// Citizenship / clearance rules: never open to foreign applicants.
const CITIZENS_ONLY: &[&str] = &[
    "us citizens", "u.s. citizens", "us citizenship", "green card", "security clearance", "citizenship is required",
    "must be a citizen",
];
/// Phrases saying the employer won't sponsor a work visa.
const NO_SPONSORSHIP: &[&str] = &[
    "no visa sponsorship", "not offer visa sponsorship", "not provide visa sponsorship", "unable to sponsor",
    "not able to sponsor", "cannot sponsor", "can't sponsor", "will not sponsor", "won't sponsor", "does not sponsor",
    "do not sponsor", "no sponsorship", "without sponsorship", "sponsorship is not available",
    "sponsorship not available",
];
/// Phrases saying applicants from other countries are welcome: visa
/// sponsorship, relocation help, or an explicit invitation.
pub const OPEN_ABROAD: &[&str] = &[
    "visa sponsorship", "sponsor your visa", "sponsor visas", "sponsor work visa", "will sponsor", "we sponsor",
    "sponsorship available", "sponsorship provided", "blue card", "relocation assistance", "relocation support",
    "relocation package", "relocation bonus", "relocation allowance", "relocation help", "help you relocate",
    "relocation is provided", "relocation provided", "relocation offered", "relocation is offered",
    "relocation is available", "relocation available", "international applicants", "international candidates",
    "applicants from any country", "candidates from any country", "open to applicants worldwide",
    "open to candidates worldwide", "from anywhere in the world",
];

/// Does the posting welcome applicants who need a visa / relocation?
pub fn open_to_foreign_applicants(desc_lower: &str) -> bool {
    !NO_SPONSORSHIP.iter().any(|p| desc_lower.contains(p)) && OPEN_ABROAD.iter().any(|p| desc_lower.contains(p))
}

/// Why a job isn't open to applicants in `country`, if it isn't. Remote
/// jobs must accept that country (or anywhere); on-site / hybrid jobs must be
/// in it, or offer visa sponsorship / relocation.
pub fn eligibility_problem(j: &RawJob, country: &str) -> Option<String> {
    let c = country.to_lowercase();
    let region = j
        .region
        .clone()
        .or_else(|| j.details.location.clone())
        .unwrap_or_default()
        .to_lowercase();
    let desc = j.details.description.as_deref().unwrap_or("").to_lowercase();
    let names_country = |t: &str| {
        contains_word(t, &c)
            || (c == "india" && crate::knowledge::profile::countries_in(t).contains(&"India"))
    };
    if let Some(p) = CITIZENS_ONLY.iter().find(|p| desc.contains(*p)) {
        return Some(format!("description says \"{p}\""));
    }
    let welcomes_foreigners = open_to_foreign_applicants(&desc);
    // What the region says beyond "remote": any named place that isn't the
    // applicant's country (or worldwide / APAC) limits who may apply.
    let named_place: String = region
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| {
            !w.is_empty()
                && !["remote", "fully", "full", "100", "only", "work", "from", "home", "wfh", "hybrid", "onsite", "on", "site", "friendly", "first", "time"]
                    .contains(w)
        })
        .collect::<Vec<_>>()
        .join(" ");
    let region_ok = if named_place.is_empty() {
        None // "Remote" alone: decided by the description
    } else if names_country(&region) || ANYWHERE.iter().any(|w| region.contains(w)) {
        Some(true)
    } else {
        Some(false)
    };
    let place = || j.region.as_deref().or(j.details.location.as_deref()).unwrap_or("?").to_string();
    if region_ok == Some(false) && !welcomes_foreigners {
        return Some(if is_remote(j) {
            format!("only for applicants in {}", place())
        } else {
            format!("on-site in {} without visa sponsorship or relocation", place())
        });
    }
    if (region_ok != Some(true) || !names_country(&desc)) && !welcomes_foreigners {
        if let Some(p) = RESTRICTED.iter().find(|p| desc.contains(*p)) {
            return Some(format!("description says \"{p}\""));
        }
    }
    None
}

/// Remote or a distributed team.
pub fn is_remote(j: &RawJob) -> bool {
    if j.details.remote.as_deref() == Some("remote") {
        return true;
    }
    let place = format!(
        "{} {}",
        j.region.as_deref().unwrap_or(""),
        j.details.location.as_deref().unwrap_or("")
    )
    .to_lowercase();
    if ["remote", "anywhere", "worldwide", "work from home", "wfh"].iter().any(|w| contains_word(&place, w)) {
        return true;
    }
    let desc = j.details.description.as_deref().unwrap_or("").to_lowercase();
    ["fully remote", "100% remote", "remote-first", "remote first", "distributed team", "fully distributed", "work from anywhere"]
        .iter()
        .any(|p| desc.contains(p))
}

/// Is the role one of `types` (contract / freelance / consultant)? Uses the
/// board's employment type, the title and tags, and clear phrases in the
/// description (not a passing "work with consultants").
pub fn engagement_ok(j: &RawJob, types: &[String]) -> bool {
    let label = j.employment_type.as_deref().unwrap_or("").to_lowercase();
    let head = format!("{} {}", j.details.title, j.details.skills.join(" ")).to_lowercase();
    let desc = j.details.description.as_deref().unwrap_or("").to_lowercase();
    let any_in = |t: &str, ws: &[&str]| ws.iter().any(|w| contains_word(t, w));
    types.iter().any(|t| match t.trim().to_lowercase().as_str() {
        "contract" | "contractual" => {
            any_in(&label, &["contract", "contractor", "temporary", "fixed term", "fixed-term"])
                || any_in(&head, &["contract", "contractor", "c2h", "contract-to-hire"])
                || [
                    "contract role", "contract position", "contract basis", "on a contract", "contract-to-hire",
                    "contract to hire", "fixed-term contract", "independent contractor", "1099", "b2b contract",
                ]
                .iter()
                .any(|p| desc.contains(p))
        }
        "freelance" | "freelancer" => {
            any_in(&label, &["freelance", "freelancer"]) || any_in(&head, &["freelance", "freelancer"]) || desc.contains("freelance")
        }
        "consultant" | "consulting" | "software consultant" | "individual consultant" => {
            any_in(&label, &["consultant", "consulting"])
                || any_in(&head, &["consultant", "consulting", "consultancy"])
                || ["independent consultant", "as a consultant", "consulting engagement", "consulting basis"]
                    .iter()
                    .any(|p| desc.contains(p))
        }
        // "remote"/"distributed" are handled by `require_remote`
        "remote" | "distributed" => false,
        other => !other.is_empty() && (contains_word(&label, other) || contains_word(&head, other)),
    })
}

/// Engineering / technical consulting titles (not design, sales, support…):
/// developers, engineers, architects, and engineering leads / managers /
/// directors.
pub fn engineering_title(title: &str) -> bool {
    let t = title.to_lowercase();
    let direct = [
        "engineer", "developer", "dev", "architect", "programmer", "consultant", "scientist", "sre", "devops",
        "full stack", "full-stack", "fullstack", "backend", "back-end", "technical lead", "tech lead", "cto",
        "engineering manager", "director of engineering", "head of engineering", "vp engineering",
        "vp of engineering", "software development manager",
    ]
    .iter()
    .any(|w| contains_word(&t, w));
    // "Lead", "Manager", "Director", "Head" count when the title is technical.
    let leader = ["lead", "manager", "director", "head", "principal", "staff"]
        .iter()
        .any(|w| contains_word(&t, w));
    let technical = [
        "engineering", "software", "technology", "technical", "platform", "development", "ai", "ml", "genai",
        "python", "java", ".net", "c#", "golang", "rust", "cloud", "data",
    ]
    .iter()
    .any(|w| has(&t, w));
    direct || (leader && technical)
}

/// How far up a kept job ranks: `boost` for remote, and again for a
/// preferred role type. Returns the points and what earned them.
pub fn priority(j: &RawJob, preferred_types: &[String], boost: i64) -> (i64, Vec<String>) {
    let mut why = Vec::new();
    if is_remote(j) {
        why.push("remote".to_string());
    }
    if let Some(t) = preferred_types
        .iter()
        .find(|t| engagement_ok(j, std::slice::from_ref(*t)))
    {
        why.push(t.trim().to_lowercase());
    }
    (boost * why.len() as i64, why)
}

/// Keep the job, or say why not.
pub fn check(j: &RawJob, c: &Criteria, now: DateTime<Utc>) -> Result<Vec<String>, String> {
    if !engineering_title(&j.details.title) {
        return Err("not an engineering role".into());
    }
    if let Some(h) = c.max_age_hours {
        match j.posted_at {
            None => return Err("no posting date".into()),
            Some(t) if now.signed_duration_since(t) > chrono::Duration::hours(h as i64) => {
                return Err(format!("posted more than {h} h ago"))
            }
            _ => {}
        }
    }
    if c.require_remote && !is_remote(j) {
        return Err("not remote".into());
    }
    let role_types: Vec<String> = c
        .work_types
        .iter()
        .filter(|t| !matches!(t.trim().to_lowercase().as_str(), "remote" | "distributed"))
        .cloned()
        .collect();
    if !role_types.is_empty() && !engagement_ok(j, &role_types) {
        return Err("not a contract / consultant / freelance role".into());
    }
    if let Some(p) = eligibility_problem(j, &c.country) {
        return Err(p);
    }
    let hits = stack_hits(j, &c.stack);
    if !c.stack.is_empty() && hits.is_empty() {
        return Err("not in your tech stack".into());
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::jobs::JobDetails;

    fn raw(title: &str, region: Option<&str>, desc: &str, kind: Option<&str>, age_h: i64) -> RawJob {
        RawJob {
            source: "remotive".into(),
            source_ref: None,
            details: JobDetails {
                title: title.into(),
                description: Some(desc.into()),
                remote: Some("remote".into()),
                ..Default::default()
            },
            posted_at: Some(Utc::now() - chrono::Duration::hours(age_h)),
            region: region.map(str::to_string),
            employment_type: kind.map(str::to_string),
        }
    }

    fn crit() -> Criteria {
        Criteria {
            max_age_hours: Some(25),
            country: "India".into(),
            stack: ["python", "c#", ".net", "java", "genai", "agentic ai"].iter().map(|s| s.to_string()).collect(),
            work_types: ["contract", "freelance", "consultant"].iter().map(|s| s.to_string()).collect(),
            require_remote: true,
        }
    }

    #[test]
    fn keeps_fresh_india_friendly_stack_jobs() {
        let now = Utc::now();
        let ok = raw("Senior Python Engineer", Some("Worldwide"), "Build RAG with LangChain.", Some("contract"), 3);
        assert_eq!(check(&ok, &crit(), now).unwrap(), vec!["python", "genai"]);
        let india = raw(".NET Developer", Some("India"), "C# services", Some("contract"), 10);
        assert!(check(&india, &crit(), now).is_ok());
        let apac = raw("Java Consultant", Some("APAC"), "Spring Boot", None, 1);
        assert!(check(&apac, &crit(), now).is_ok(), "consultant in the title");
        let freelance = raw("Python Developer (Freelance)", Some("Worldwide"), "", None, 1);
        assert!(check(&freelance, &crit(), now).is_ok());
        let full_time = raw("Senior Python Engineer", Some("Worldwide"), "We work with consultants.", Some("full_time"), 1);
        assert_eq!(
            check(&full_time, &crit(), now).unwrap_err(),
            "not a contract / consultant / freelance role",
            "a passing mention of consultants is not a consulting role"
        );
    }

    #[test]
    fn drops_stale_restricted_and_off_stack_jobs() {
        let now = Utc::now();
        let c = crit();
        assert!(check(&raw("Python Dev", Some("Worldwide"), "", Some("contract"), 30), &c, now).unwrap_err().contains("25 h"));
        let mut nodate = raw("Python Dev", Some("Worldwide"), "", Some("contract"), 1);
        nodate.posted_at = None;
        assert_eq!(check(&nodate, &c, now).unwrap_err(), "no posting date");
        assert!(check(&raw("Python Dev", Some("USA"), "", Some("contract"), 1), &c, now).unwrap_err().contains("only for applicants in USA"));
        assert!(check(&raw("Python Dev", Some("Europe, UK"), "", Some("contract"), 1), &c, now).is_err());
        let us_only = raw("Python Dev", Some("Remote"), "You must be located in the US to apply.", Some("contract"), 1);
        assert!(check(&us_only, &c, now).unwrap_err().contains("located in the us"));
        assert_eq!(check(&raw("Go Engineer", Some("Worldwide"), "Golang and Kubernetes", Some("contract"), 1), &c, now).unwrap_err(), "not in your tech stack");
        assert_eq!(
            check(&raw("Java Developer", Some("India"), "JavaScript only", Some("contract"), 1).with_onsite(), &c, now).unwrap_err(),
            "not remote"
        );
    }

    #[test]
    fn any_other_named_place_is_a_restriction() {
        let now = Utc::now();
        let c = crit();
        for region in ["Portugal", "Costa Rica", "Serbia", "Pakistan", "Remote - Italy", "San Francisco, CA | Remote"] {
            assert!(check(&raw("Python Developer", Some(region), "", Some("contract"), 1), &c, now).is_err(), "{region}");
        }
        for region in ["Remote", "Bengaluru", "Remote - India", "Anywhere in the World", "Americas, EMEA, APAC"] {
            assert!(check(&raw("Python Developer", Some(region), "", Some("contract"), 1), &c, now).is_ok(), "{region}");
        }
    }

    #[test]
    fn a_passing_mention_is_not_the_stack() {
        let c = crit();
        let art = raw("Conceptual Art Director", Some("Anywhere"), "Our studio also uses Python for tooling.", None, 1);
        assert!(stack_hits(&art, &c.stack).is_empty());
        let mut tagged = raw("Conceptual Art Director", Some("Anywhere"), "", Some("contract"), 1);
        tagged.details.skills = vec!["Generative AI".into()];
        assert_eq!(check(&tagged, &c, Utc::now()).unwrap_err(), "not an engineering role");
        let backend = raw("Backend Engineer", Some("Anywhere"), "Python services. Python, FastAPI and Postgres.", None, 1);
        assert_eq!(stack_hits(&backend, &c.stack), vec!["python"]);
    }

    #[test]
    fn javascript_is_not_java() {
        let j = raw("Frontend Engineer", Some("Worldwide"), "React and JavaScript", None, 1);
        assert!(stack_hits(&j, &crit().stack).is_empty());
    }

    /// The new defaults: any workplace, any role type.
    fn broad() -> Criteria {
        Criteria {
            stack: ["python", "c#", ".net", "java", "rust", "go", "genai", "agentic ai"].iter().map(|s| s.to_string()).collect(),
            work_types: Vec::new(),
            require_remote: false,
            ..crit()
        }
    }

    fn onsite(title: &str, region: &str, desc: &str) -> RawJob {
        let mut j = raw(title, Some(region), desc, Some("full_time"), 2);
        j.details.remote = Some("onsite".into());
        j
    }

    #[test]
    fn onsite_jobs_in_india_or_with_sponsorship_are_kept() {
        let now = Utc::now();
        let c = broad();
        assert!(check(&onsite("Senior Python Developer", "Bengaluru, Karnataka, India", "Python"), &c, now).is_ok());
        assert!(check(&onsite("Java Architect", "Pune", "Java, Spring"), &c, now).is_ok(), "Pune is in India");
        let sponsored = onsite("Java Architect", "Berlin, Germany", "Java. We offer visa sponsorship and relocation support.");
        assert!(check(&sponsored, &c, now).is_ok());
        let welcome = onsite("Rust Engineer", "Dubai, UAE", "Rust. International applicants welcome.");
        assert!(check(&welcome, &c, now).is_ok());
        let local = onsite("Python Engineer", "Munich, Germany", "Python services");
        assert_eq!(
            check(&local, &c, now).unwrap_err(),
            "on-site in Munich, Germany without visa sponsorship or relocation"
        );
        let refused = onsite("Python Engineer", "London, UK", "Python. We do not sponsor visas. Relocation support available.");
        assert!(check(&refused, &c, now).is_err(), "no sponsorship wins over relocation help");
        let citizens = onsite("Python Engineer", "Austin, TX", "Python. Visa sponsorship available. US citizens only.");
        assert!(check(&citizens, &c, now).unwrap_err().contains("us citizens"));
        // remote jobs restricted to another country are still dropped
        assert!(check(&raw("Python Dev", Some("USA"), "", None, 1), &c, now).unwrap_err().contains("only for applicants in USA"));
    }

    #[test]
    fn leads_managers_and_directors_count() {
        for t in [
            "Engineering Manager, AI Platform", "Director of Engineering", "Principal Software Engineer", "Lead - Python",
            "Head of AI", "Staff Data Engineer", "Solution Architect", "Enterprise Architect (GenAI)", "Golang Developer",
        ] {
            assert!(engineering_title(t), "{t}");
        }
        for t in ["Sales Manager", "Art Director", "Head of Marketing", "Account Lead"] {
            assert!(!engineering_title(t), "{t}");
        }
    }

    #[test]
    fn go_and_rust_stacks() {
        let c = broad();
        let go = raw("Backend Developer", Some("Worldwide"), "Golang microservices. Golang, gRPC.", None, 1);
        assert_eq!(stack_hits(&go, &c.stack), vec!["go"]);
        let not_go = raw("Backend Developer", Some("Worldwide"), "Ready to go? Let's go build things.", None, 1);
        assert!(stack_hits(&not_go, &c.stack).is_empty(), "'go' alone is not Go");
        let rust = raw("Rust Engineer", Some("Worldwide"), "", None, 1);
        assert_eq!(stack_hits(&rust, &c.stack), vec!["rust"]);
    }

    #[test]
    fn remote_and_contract_rank_first() {
        let types: Vec<String> = ["contract", "freelance", "consultant"].iter().map(|s| s.to_string()).collect();
        let both = raw("Python Developer", Some("Worldwide"), "", Some("contract"), 1);
        assert_eq!(priority(&both, &types, 10), (20, vec!["remote".to_string(), "contract".to_string()]));
        let freelance = onsite("Python Developer (Freelance)", "Pune", "");
        assert_eq!(priority(&freelance, &types, 10), (10, vec!["freelance".to_string()]));
        let office = onsite("Python Developer", "Pune", "");
        assert_eq!(priority(&office, &types, 10).0, 0);
    }

    impl RawJob {
        fn with_onsite(mut self) -> Self {
            self.details.remote = Some("onsite".into());
            self.details.description = Some("Office in Pune. JavaScript only. Java".into());
            self.details.title = "Java Developer".into();
            self
        }
    }
}
