//! People, mailboxes and purposes.
//!
//! `knowledge/people.json` says who owns which mailbox and what each mailbox
//! is for. Each person has their own folder:
//!
//! ```text
//! knowledge/
//!   people.json
//!   shared/                          household-wide notes (optional)
//!   people/<id>/
//!     profile.json resume.json skills.json employment_history.json ...
//!     private.json                   DOB, PAN, Aadhaar, ... (never indexed or sent to an LLM)
//!     job_seeking/ preferences.json salary.json resumes/ replies/
//!     consulting/  ...               any purpose folder
//!     freelancing/ ...
//! ```
//!
//! Without `people.json` the whole knowledge folder is one person (legacy).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

use super::profile::Profile;

pub const LEGACY_ID: &str = "default";
pub const JOB_SEEKING: &str = "job_seeking";
pub const CONSULTING: &str = "consulting";
pub const FREELANCING: &str = "freelancing";

/// Normalise purpose names from config ("jobs" → "job_seeking", ...).
pub fn normalize_purpose(p: &str) -> String {
    let p = p.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    match p.as_str() {
        "job" | "jobs" | "job_search" | "job_hunting" | "job_seeking" | "career" => {
            JOB_SEEKING.into()
        }
        "consult" | "consulting" | "consultancy" => CONSULTING.into(),
        "freelance" | "freelancing" | "freelancer" => FREELANCING.into(),
        _ => p,
    }
}

// ---------------------------------------------------------------- config file

#[derive(Debug, Clone, Deserialize, Default)]
struct PeopleFile {
    #[serde(default)]
    people: Vec<PersonCfg>,
    /// "classify_only" (default) or "default_person".
    #[serde(default)]
    unassigned_mailboxes: Option<String>,
    #[serde(default)]
    default_person: Option<String>,
    /// Whose profile daily job discovery scores against.
    #[serde(default)]
    research_person: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PersonCfg {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    mailboxes: Vec<MailboxCfg>,
    #[serde(default)]
    job_agent: bool,
    #[serde(default)]
    telegram_chat_id: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum MailboxCfg {
    Plain(String),
    Full {
        address: String,
        #[serde(default)]
        purposes: Vec<String>,
    },
}

// ---------------------------------------------------------------- runtime types

#[derive(Debug, Clone, PartialEq)]
pub struct Mailbox {
    pub address: String,
    pub purposes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharePolicy {
    /// Fill the real value into the draft when the email asks for it.
    OnRequest,
    /// Say it will be shared over a secure channel.
    SecureChannel,
    /// Politely decline to share it by email.
    Never,
}

impl SharePolicy {
    fn parse(s: &str) -> Self {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace([' ', '-'], "_")
            .as_str()
        {
            "on_request" | "yes" | "share" | "always" => Self::OnRequest,
            "never" | "no" => Self::Never,
            _ => Self::SecureChannel,
        }
    }
}

/// Sensitive facts from `private.json`. Values never go into prompts; the
/// drafter inserts them after generation (see `agents::facts`).
#[derive(Debug, Clone, Default)]
pub struct PrivateFacts {
    pub values: BTreeMap<String, String>,
    pub share: BTreeMap<String, SharePolicy>,
}

const ALWAYS_SENSITIVE: &[&str] = &[
    "pan",
    "aadhaar",
    "passport",
    "bank_account",
    "ifsc",
    "uan",
    "ssn",
];

impl PrivateFacts {
    pub fn load(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let v: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "invalid private.json; ignoring");
                return Self::default();
            }
        };
        let mut out = Self::default();
        if let Some(obj) = v.as_object() {
            for (k, val) in obj {
                if k == "share" {
                    if let Some(s) = val.as_object() {
                        for (sk, sv) in s {
                            if let Some(p) = sv.as_str() {
                                out.share
                                    .insert(sk.to_ascii_lowercase(), SharePolicy::parse(p));
                            }
                        }
                    }
                    continue;
                }
                let text = match val {
                    serde_json::Value::String(s) => s.trim().to_string(),
                    serde_json::Value::Number(n) => n.to_string(),
                    _ => continue,
                };
                if !text.is_empty() {
                    out.values.insert(k.to_ascii_lowercase(), text);
                }
            }
        }
        out
    }

    pub fn policy(&self, key: &str) -> SharePolicy {
        if let Some(p) = self.share.get(key) {
            return *p;
        }
        if ALWAYS_SENSITIVE.contains(&key) {
            SharePolicy::SecureChannel
        } else {
            SharePolicy::OnRequest
        }
    }
}

/// One resume file and the tech stack it represents.
#[derive(Debug, Clone, PartialEq)]
pub struct Resume {
    pub path: PathBuf,
    pub file_name: String,
    /// Stack hinted by the file name (weighted higher when matching).
    pub name_tags: Vec<String>,
    /// Skills found in the resume text.
    pub tags: Vec<String>,
    pub default: bool,
    /// Purpose folder the file lives in (`consulting`, `freelancing`, …);
    /// None for files in the person's own folder.
    pub purpose: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Person {
    pub id: String,
    pub name: String,
    pub dir: PathBuf,
    /// Path prefix inside the knowledge index ("people/<id>", or "" in legacy mode).
    pub rel_dir: String,
    pub mailboxes: Vec<Mailbox>,
    pub job_agent: bool,
    pub telegram_chat_id: Option<i64>,
    pub profile: Arc<Profile>,
    pub private: PrivateFacts,
    pub resumes: Vec<Resume>,
}

impl Person {
    /// All purposes across this person's mailboxes.
    pub fn purposes(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for m in &self.mailboxes {
            for p in &m.purposes {
                if !v.contains(p) {
                    v.push(p.clone());
                }
            }
        }
        v
    }

    /// The best resume for a job's skills: name hints count double, then
    /// skills in the resume text. Falls back to the default resume.
    ///
    /// `purpose` narrows the candidates: a consulting or freelancing email
    /// prefers documents in that purpose's folder; job applications (and
    /// purposes without their own documents) use the person's general and
    /// `job_seeking` resumes.
    pub fn select_resume(&self, job_skills: &[String], purpose: Option<&str>) -> Option<(&Resume, Vec<String>)> {
        let own: Vec<&Resume> = match purpose {
            Some(p) if p != JOB_SEEKING => self.resumes.iter().filter(|r| r.purpose.as_deref() == Some(p)).collect(),
            _ => Vec::new(),
        };
        let candidates: Vec<&Resume> = if !own.is_empty() {
            own
        } else {
            let general: Vec<&Resume> = self
                .resumes
                .iter()
                .filter(|r| r.purpose.is_none() || r.purpose.as_deref() == Some(JOB_SEEKING))
                .collect();
            if general.is_empty() { self.resumes.iter().collect() } else { general }
        };
        if candidates.is_empty() {
            return None;
        }
        let wanted: Vec<String> = job_skills.iter().map(|s| s.to_lowercase()).collect();
        let mut best: Option<(&Resume, usize, Vec<String>)> = None;
        for r in candidates.iter().copied() {
            let mut matched: Vec<String> = Vec::new();
            let mut score = 0usize;
            for t in r.name_tags.iter().chain(r.tags.iter()) {
                let tl = t.to_lowercase();
                if wanted.contains(&tl) && !matched.iter().any(|m| m.eq_ignore_ascii_case(t)) {
                    matched.push(t.clone());
                    score += if r.name_tags.contains(t) { 2 } else { 1 };
                }
            }
            let better = match &best {
                None => true,
                Some((b, bs, _)) => score > *bs || (score == *bs && r.default && !b.default),
            };
            if better {
                best = Some((r, score, matched));
            }
        }
        let (r, score, matched) = best?;
        if score == 0 {
            let d = candidates.iter().copied().find(|r| r.default).unwrap_or(candidates[0]);
            return Some((d, Vec::new()));
        }
        Some((r, matched))
    }
}

/// Which person (and purposes) an email belongs to.
#[derive(Debug, Clone, Default)]
pub struct Route {
    pub person: Option<Arc<Person>>,
    pub mailbox: Option<String>,
    /// Purposes of the receiving mailbox; empty = any / legacy.
    pub purposes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unassigned {
    ClassifyOnly,
    DefaultPerson,
}

#[derive(Debug, Clone)]
pub struct People {
    pub persons: Vec<Arc<Person>>,
    pub legacy: bool,
    pub unassigned: Unassigned,
    default_person: Option<String>,
    research_person: Option<String>,
}

impl People {
    /// Load `people.json` (or legacy single-person mode) from the knowledge dir.
    pub fn load(knowledge_dir: &Path) -> Self {
        let cfg_path = knowledge_dir.join("people.json");
        let file: Option<PeopleFile> = std::fs::read_to_string(&cfg_path).ok().and_then(|raw| {
            match serde_json::from_str(&raw) {
                Ok(f) => Some(f),
                Err(e) => {
                    tracing::error!(file = %cfg_path.display(), error = %e, "invalid people.json; using legacy single-person mode");
                    None
                }
            }
        });
        let Some(file) = file.filter(|f| !f.people.is_empty()) else {
            let profile = Arc::new(Profile::load(knowledge_dir));
            let resumes = scan_resumes(&[(knowledge_dir.join("resumes"), true, None)], &profile);
            let person = Person {
                id: LEGACY_ID.into(),
                name: profile.display_name().to_string(),
                dir: knowledge_dir.to_path_buf(),
                rel_dir: String::new(),
                mailboxes: Vec::new(),
                job_agent: true,
                telegram_chat_id: None,
                private: PrivateFacts::load(&knowledge_dir.join("private.json")),
                resumes,
                profile,
            };
            return Self {
                persons: vec![Arc::new(person)],
                legacy: true,
                unassigned: Unassigned::DefaultPerson,
                default_person: None,
                research_person: None,
            };
        };
        let persons = file
            .people
            .iter()
            .filter(|p| valid_id(&p.id))
            .map(|p| {
                let dir = knowledge_dir.join("people").join(&p.id);
                let profile = Arc::new(Profile::load(&dir));
                // Resumes/profiles may sit in the person's folder, in any purpose
                // folder (consulting/, freelancing/, …) or its resumes/ subfolder.
                let resumes = scan_resumes(&resume_dirs(&dir), &profile);
                let mailboxes = p
                    .mailboxes
                    .iter()
                    .map(|m| match m {
                        MailboxCfg::Plain(a) => Mailbox {
                            address: a.trim().to_ascii_lowercase(),
                            purposes: Vec::new(),
                        },
                        MailboxCfg::Full { address, purposes } => Mailbox {
                            address: address.trim().to_ascii_lowercase(),
                            purposes: purposes.iter().map(|x| normalize_purpose(x)).collect(),
                        },
                    })
                    .collect();
                Arc::new(Person {
                    id: p.id.clone(),
                    name: p
                        .name
                        .clone()
                        .filter(|n| !n.trim().is_empty())
                        .unwrap_or_else(|| profile.display_name().to_string()),
                    rel_dir: format!("people/{}", p.id),
                    mailboxes,
                    job_agent: p.job_agent,
                    telegram_chat_id: p.telegram_chat_id,
                    private: PrivateFacts::load(&dir.join("private.json")),
                    resumes,
                    profile,
                    dir,
                })
            })
            .collect();
        Self {
            persons,
            legacy: false,
            unassigned: match file.unassigned_mailboxes.as_deref() {
                Some("default_person") => Unassigned::DefaultPerson,
                _ => Unassigned::ClassifyOnly,
            },
            default_person: file.default_person,
            research_person: file.research_person,
        }
    }

    pub fn person(&self, id: &str) -> Option<Arc<Person>> {
        self.persons.iter().find(|p| p.id == id).cloned()
    }

    pub fn default_person(&self) -> Arc<Person> {
        self.default_person
            .as_deref()
            .and_then(|id| self.person(id))
            .unwrap_or_else(|| self.persons[0].clone())
    }

    /// Person whose profile research/discovery uses.
    pub fn research_person(&self) -> Arc<Person> {
        self.research_person
            .as_deref()
            .and_then(|id| self.person(id))
            .or_else(|| self.persons.iter().find(|p| p.job_agent).cloned())
            .unwrap_or_else(|| self.default_person())
    }

    /// Route an email by the receiving account address, then To/Cc.
    pub fn route(&self, account_email: Option<&str>, to: &[String], cc: &[String]) -> Route {
        if self.legacy {
            return Route {
                person: Some(self.persons[0].clone()),
                mailbox: account_email.map(|a| a.to_ascii_lowercase()),
                purposes: Vec::new(),
            };
        }
        let mut candidates: Vec<String> = Vec::new();
        if let Some(a) = account_email {
            candidates.push(a.trim().to_ascii_lowercase());
        }
        for addr in to.iter().chain(cc.iter()) {
            candidates.push(crate::db::emails::email_address(addr));
        }
        for c in &candidates {
            for p in &self.persons {
                if let Some(m) = p.mailboxes.iter().find(|m| &m.address == c) {
                    return Route {
                        person: Some(p.clone()),
                        mailbox: Some(m.address.clone()),
                        purposes: m.purposes.clone(),
                    };
                }
            }
        }
        match self.unassigned {
            Unassigned::ClassifyOnly => Route {
                person: None,
                mailbox: account_email.map(|a| a.to_ascii_lowercase()),
                purposes: Vec::new(),
            },
            Unassigned::DefaultPerson => Route {
                person: Some(self.default_person()),
                mailbox: account_email.map(|a| a.to_ascii_lowercase()),
                purposes: Vec::new(),
            },
        }
    }
}

fn valid_id(id: &str) -> bool {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !ok {
        tracing::error!(
            id,
            "invalid person id in people.json (use letters, digits, _ or -); skipped"
        );
    }
    ok
}

/// File-name hints → stack tags ("dotnet_resume.docx" → .NET, C#).
const NAME_HINTS: &[(&str, &[&str])] = &[
    ("dotnet", &[".NET", "C#"]),
    ("net", &[".NET", "C#"]),
    ("csharp", &["C#", ".NET"]),
    ("python", &["Python"]),
    ("django", &["Python", "Django"]),
    ("fastapi", &["Python", "FastAPI"]),
    ("java", &["Java"]),
    ("spring", &["Java", "Spring Boot"]),
    ("golang", &["Go"]),
    ("go", &["Go"]),
    ("rust", &["Rust"]),
    ("node", &["Node.js", "JavaScript"]),
    ("nodejs", &["Node.js", "JavaScript"]),
    ("react", &["React", "JavaScript"]),
    ("angular", &["Angular", "TypeScript"]),
    ("frontend", &["JavaScript", "TypeScript"]),
    ("fullstack", &["JavaScript", "TypeScript"]),
    ("ai", &["Machine Learning", "LLMs"]),
    ("ml", &["Machine Learning"]),
    ("genai", &["Generative AI", "LLMs", "RAG"]),
    ("data", &["SQL", "Spark"]),
    ("devops", &["Kubernetes", "Docker", "Terraform"]),
    ("cloud", &["AWS", "Azure"]),
    ("aws", &["AWS"]),
    ("az", &["Azure"]),
    ("kafka", &["Kafka"]),
    ("k8s", &["Kubernetes"]),
    ("azure", &["Azure"]),
    ("architect", &["System Design", "Microservices"]),
];

#[derive(Deserialize)]
struct ResumeIndexEntry {
    file: String,
    #[serde(default)]
    stack: Vec<String>,
    #[serde(default)]
    default: bool,
}

/// Read the text of a resume (docx/md/txt; PDFs are matched by name and
/// `resumes.json` tags only).
pub fn resume_text(path: &Path) -> String {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("docx") => super::docx::extract_text(path).unwrap_or_default(),
        Some("md" | "txt") => std::fs::read_to_string(path).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Directories that may hold a person's resumes, with their purpose.
fn resume_dirs(person_dir: &Path) -> Vec<(PathBuf, bool, Option<String>)> {
    let mut dirs = vec![(person_dir.to_path_buf(), false, None)];
    let mut subs: Vec<PathBuf> = std::fs::read_dir(person_dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
        .unwrap_or_default();
    subs.sort();
    for sub in subs {
        let name = sub.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if name.starts_with('.') || name == "private" || name == "replies" {
            continue;
        }
        let purpose = normalize_purpose(&name);
        dirs.push((sub.clone(), false, Some(purpose.clone())));
        dirs.push((sub.join("resumes"), true, Some(purpose)));
    }
    dirs
}

/// Scan resume files. Each entry is (directory, is_dedicated_resume_dir,
/// purpose): a dedicated `resumes/` folder may also hold .md/.txt resumes;
/// other folders only contribute documents (.docx/.pdf/.doc/.rtf).
fn scan_resumes(dirs: &[(PathBuf, bool, Option<String>)], profile: &Profile) -> Vec<Resume> {
    let mut files: Vec<(PathBuf, Option<String>)> = Vec::new();
    let mut index: Vec<ResumeIndexEntry> = Vec::new();
    for (dir, dedicated, purpose) in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut found: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let ext = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase());
                p.is_file()
                    && match ext.as_deref() {
                        Some("docx" | "pdf" | "doc" | "rtf") => true,
                        Some("md" | "txt") => *dedicated,
                        _ => false,
                    }
            })
            .collect();
        found.sort();
        files.extend(found.into_iter().map(|p| (p, purpose.clone())));
        if let Some(entries) = std::fs::read_to_string(dir.join("resumes.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<Vec<ResumeIndexEntry>>(&raw).ok())
        {
            index.extend(entries);
        }
    }
    let mut vocab: Vec<String> = crate::jobs::extract::COMMON_SKILLS
        .iter()
        .map(|s| s.to_string())
        .collect();
    for s in profile.all_skills() {
        if !vocab.iter().any(|v| v.eq_ignore_ascii_case(&s)) {
            vocab.push(s);
        }
    }
    for extra in [
        ".NET",
        "C#",
        "Node.js",
        "Spring Boot",
        "Django",
        "FastAPI",
        "Angular",
    ] {
        if !vocab.iter().any(|v| v.eq_ignore_ascii_case(extra)) {
            vocab.push(extra.into());
        }
    }
    let mut out: Vec<Resume> = files
        .into_iter()
        .map(|(path, purpose)| {
            let file_name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let mut name_tags: Vec<String> = Vec::new();
            for token in stem.split(|c: char| !c.is_ascii_alphanumeric()) {
                if let Some((_, tags)) = NAME_HINTS.iter().find(|(k, _)| *k == token) {
                    for t in *tags {
                        if !name_tags.iter().any(|x| x == t) {
                            name_tags.push(t.to_string());
                        }
                    }
                }
            }
            let entry = index
                .iter()
                .find(|e| e.file.eq_ignore_ascii_case(&file_name));
            if let Some(e) = entry {
                for t in &e.stack {
                    if !name_tags.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                        name_tags.push(t.clone());
                    }
                }
            }
            let text = resume_text(&path).to_lowercase();
            let tags: Vec<String> = vocab
                .iter()
                .filter(|s| crate::jobs::extract::contains_word(&text, &s.to_lowercase()))
                .cloned()
                .collect();
            Resume {
                default: entry.map(|e| e.default).unwrap_or(false),
                path,
                file_name,
                name_tags,
                tags,
                purpose,
            }
        })
        .collect();
    // One default per purpose group (general + job_seeking share a group): a
    // generically named file ("resume.docx") if any, else the first.
    let group = |r: &Resume| match r.purpose.as_deref() {
        None | Some(JOB_SEEKING) => String::new(),
        Some(p) => p.to_string(),
    };
    let groups: Vec<String> = {
        let mut g: Vec<String> = out.iter().map(group).collect();
        g.dedup();
        g.sort();
        g.dedup();
        g
    };
    for g in groups {
        let members: Vec<usize> = (0..out.len()).filter(|&i| group(&out[i]) == g).collect();
        if members.iter().any(|&i| out[i].default) {
            continue;
        }
        let idx = members
            .iter()
            .copied()
            .find(|&i| out[i].name_tags.is_empty())
            .unwrap_or(members[0]);
        out[idx].default = true;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    fn setup() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let k = d.path();
        write(
            &k.join("people.json"),
            r#"{
          "people": [
            {"id": "shashi", "name": "Shashi Kumar", "job_agent": true, "telegram_chat_id": 11,
             "mailboxes": [
               {"address": "Shashi.Jobs@example.org", "purposes": ["jobs"]},
               {"address": "shashi.all@example.org", "purposes": ["job_seeking", "consulting", "freelance"]}
             ]},
            {"id": "himanshu", "mailboxes": ["himanshu@example.org"]}
          ]
        }"#,
        );
        write(
            &k.join("people/shashi/profile.json"),
            r#"{"full_name": "Shashi Kumar", "email": "shashi.jobs@example.org"}"#,
        );
        write(
            &k.join("people/shashi/job_seeking/salary.json"),
            r#"{"expected_text": "30 LPA", "notice_period": "60 days"}"#,
        );
        write(
            &k.join("people/shashi/private.json"),
            r#"{"pan": "ABCDE1234F", "date_of_birth": "1990-01-02", "share": {"date_of_birth": "on_request"}}"#,
        );
        write(
            &k.join("people/shashi/job_seeking/resumes/python_resume.md"),
            "Python, FastAPI, AWS, Kubernetes",
        );
        write(
            &k.join("people/shashi/job_seeking/resumes/dotnet_resume.md"),
            "C#, .NET 8, Azure, SQL",
        );
        write(
            &k.join("people/himanshu/profile.json"),
            r#"{"full_name": "Himanshu Kumar"}"#,
        );
        d
    }

    #[test]
    fn routes_by_mailbox_and_purpose() {
        let d = setup();
        let people = People::load(d.path());
        assert!(!people.legacy);
        let r = people.route(Some("shashi.jobs@example.org"), &[], &[]);
        assert_eq!(r.person.as_ref().unwrap().id, "shashi");
        assert_eq!(r.purposes, vec!["job_seeking"]);
        let r = people.route(Some("SHASHI.ALL@example.org"), &[], &[]);
        assert_eq!(r.purposes, vec!["job_seeking", "consulting", "freelancing"]);
        let r = people.route(Some("himanshu@example.org"), &[], &[]);
        assert_eq!(
            r.person.as_ref().unwrap().name,
            "Himanshu Kumar",
            "name falls back to profile"
        );
        assert!(r.purposes.is_empty());
        // Unknown account but addressed (To) to a known mailbox.
        let r = people.route(
            Some("other@example.org"),
            &["Shashi <shashi.jobs@example.org>".into()],
            &[],
        );
        assert_eq!(r.person.unwrap().id, "shashi");
        // Unassigned mailbox: classify only.
        assert!(people
            .route(Some("stranger@example.org"), &[], &[])
            .person
            .is_none());
    }

    #[test]
    fn loads_overlays_private_and_resumes() {
        let d = setup();
        let people = People::load(d.path());
        let s = people.person("shashi").unwrap();
        assert_eq!(s.profile.salary.expected_text, "30 LPA");
        assert_eq!(s.profile.salary.notice_period, "60 days");
        assert_eq!(s.private.values.get("pan").unwrap(), "ABCDE1234F");
        assert_eq!(s.private.policy("pan"), SharePolicy::SecureChannel);
        assert_eq!(s.private.policy("date_of_birth"), SharePolicy::OnRequest);
        assert_eq!(s.resumes.len(), 2);
        let (r, m) = s.select_resume(&["C#".into(), "Azure".into()], None).unwrap();
        assert_eq!(r.file_name, "dotnet_resume.md");
        assert!(m.contains(&"C#".to_string()));
        let (r, _) = s
            .select_resume(&["Python".into(), "Kubernetes".into()], Some(JOB_SEEKING))
            .unwrap();
        assert_eq!(r.file_name, "python_resume.md");
        let (r, m) = s.select_resume(&["COBOL".into()], None).unwrap();
        assert!(r.default && m.is_empty());
        assert_eq!(people.research_person().id, "shashi");
    }

    #[test]
    fn legacy_mode_without_people_json() {
        let d = tempfile::tempdir().unwrap();
        write(
            &d.path().join("profile.json"),
            r#"{"full_name": "Solo User"}"#,
        );
        let people = People::load(d.path());
        assert!(people.legacy);
        let r = people.route(Some("anything@example.org"), &[], &[]);
        assert_eq!(r.person.unwrap().name, "Solo User");
    }

    #[test]
    fn purpose_aliases() {
        assert_eq!(normalize_purpose("Jobs"), "job_seeking");
        assert_eq!(normalize_purpose("freelance"), "freelancing");
        assert_eq!(normalize_purpose("Consultancy"), "consulting");
        assert_eq!(normalize_purpose("teaching"), "teaching");
    }

    #[test]
    fn purpose_documents_are_preferred_for_that_purpose() {
        let d = setup();
        let k = d.path();
        write(&k.join("people/shashi/consulting/Shashi_Consulting_Profile.md"), "Architecture consulting");
        write(&k.join("people/shashi/consulting/Shashi_Consulting_Profile.docx"), "not a real docx");
        write(&k.join("people/shashi/freelancing/resumes/freelance_python.md"), "Python FastAPI");
        let people = People::load(k);
        let s = people.person("shashi").unwrap();
        let (r, _) = s.select_resume(&["Python".into()], Some(CONSULTING)).unwrap();
        assert_eq!(r.file_name, "Shashi_Consulting_Profile.docx", "consulting folder docs win for consulting");
        assert_eq!(r.purpose.as_deref(), Some(CONSULTING));
        let (r, _) = s.select_resume(&["Python".into()], Some(FREELANCING)).unwrap();
        assert_eq!(r.file_name, "freelance_python.md");
        let (r, _) = s.select_resume(&["Python".into()], Some(JOB_SEEKING)).unwrap();
        assert_eq!(r.file_name, "python_resume.md", "job applications never use consulting docs");
        assert!(s.resumes.iter().all(|r| r.file_name != "Shashi_Consulting_Profile.md"), ".md outside resumes/ is not a resume");
    }
}
