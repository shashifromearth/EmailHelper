//! Structure-aware ETL for the knowledge folder: extract sections from
//! .docx/.pdf/.md/.txt/.json, split them into overlapping chunks that never
//! cross a section, and attach metadata (person, purpose, document type,
//! stack, section) derived from where the file lives.
//!
//! Layout (see documents/KNOWLEDGE.md):
//!   people/<person>/…                      person-level files
//!   people/<person>/resumes/<stack>/…      stack-specific resumes
//!   people/<person>/<purpose>/…            job_seeking / consulting / freelancing
//!   shared/…                               household-wide notes

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use sha2::{Digest, Sha256};

use crate::{AppError, AppResult};

/// Bump when extraction or chunking changes: every document is re-indexed.
pub const ETL_VERSION: &str = "2";

/// Target chunk size and overlap, in characters.
pub const CHUNK_CHARS: usize = 900;
pub const OVERLAP_CHARS: usize = 150;

/// Files the ETL reads.
pub const EXTENSIONS: &[&str] = &["md", "markdown", "txt", "json", "docx", "pdf"];

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct ChunkMeta {
    pub path: String,
    pub person_id: Option<String>,
    pub purpose: Option<String>,
    /// resume, profile, skills, experience, education, projects, certifications,
    /// compensation, preferences, reply_guide, about, tone, shared, notes
    pub doc_type: String,
    pub stack: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub ord: usize,
    pub section: String,
    pub title: String,
    pub content: String,
    pub hash: String,
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("valid regex"))
}

pub fn sha(data: impl AsRef<[u8]>) -> String {
    let mut h = Sha256::new();
    h.update(data.as_ref());
    format!("{:x}", h.finalize())
}

// ---------------------------------------------------------------- metadata

const PURPOSES: &[&str] = &["job_seeking", "consulting", "freelancing"];

/// Metadata for a knowledge file from its path (relative to the knowledge folder).
pub fn meta_for(rel: &str) -> ChunkMeta {
    let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    let file = parts.last().copied().unwrap_or("").to_lowercase();
    let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(&file).to_string();
    let mut m = ChunkMeta {
        path: rel.to_string(),
        ..Default::default()
    };
    let mut rest: &[&str] = &parts;
    if parts.first() == Some(&"people") && parts.len() >= 3 {
        m.person_id = Some(parts[1].to_string());
        rest = &parts[2..];
    } else if parts.first() == Some(&"shared") {
        m.doc_type = "shared".into();
        return m;
    }
    // purpose folder
    if rest.len() >= 2 && PURPOSES.contains(&rest[0]) {
        m.purpose = Some(rest[0].to_string());
        rest = &rest[1..];
    }
    // resumes/<stack>/file
    if rest.len() >= 2 && rest[0].eq_ignore_ascii_case("resumes") {
        m.doc_type = "resume".into();
        if rest.len() >= 3 {
            m.stack = Some(rest[1].to_lowercase());
        }
        return m;
    }
    if rest.len() >= 2 && rest[0].eq_ignore_ascii_case("replies") {
        m.doc_type = "reply_guide".into();
        return m;
    }
    m.doc_type = match stem.as_str() {
        s if s.contains("resume") || s.contains("cv") => "resume",
        "profile" => "profile",
        "skills" => "skills",
        "employment_history" | "experience" => "experience",
        "education" => "education",
        "projects" => "projects",
        "certifications" => "certifications",
        s if s.contains("salary") => "compensation",
        "preferences" => "preferences",
        "company_profile" | "about" | "services" | "portfolio" => "about",
        "rates" | "availability" => "compensation",
        "tone" => "tone",
        _ => "notes",
    }
    .into();
    m
}

// ---------------------------------------------------------------- extraction

/// (section heading, text) pairs, in document order.
pub type Sections = Vec<(String, String)>;

pub fn extract(path: &Path) -> AppResult<Sections> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "docx" => docx_sections(path),
        "pdf" => {
            let text = pdf_extract::extract_text(path)
                .map_err(|e| AppError::other(format!("pdf {}: {e}", path.display())))?;
            Ok(text_sections(&text, true))
        }
        "json" => {
            let raw = std::fs::read_to_string(path)?;
            Ok(json_sections(&raw))
        }
        "md" | "markdown" => Ok(markdown_sections(&std::fs::read_to_string(path)?)),
        _ => Ok(text_sections(&std::fs::read_to_string(path)?, false)),
    }
}

/// Word documents: headings from paragraph styles (Heading1…, Title) or short
/// all-bold paragraphs; tables become " | "-joined rows.
pub fn docx_sections(path: &Path) -> AppResult<Sections> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| AppError::other(format!("docx {}: {e}", path.display())))?;
    let mut xml = String::new();
    {
        use std::io::Read;
        let mut doc = zip
            .by_name("word/document.xml")
            .map_err(|e| AppError::other(format!("docx {}: {e}", path.display())))?;
        doc.read_to_string(&mut xml)?;
    }
    Ok(docx_xml_sections(&xml))
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

pub fn docx_xml_sections(xml: &str) -> Sections {
    static PARA: OnceLock<Regex> = OnceLock::new();
    static TEXT: OnceLock<Regex> = OnceLock::new();
    static STYLE: OnceLock<Regex> = OnceLock::new();
    static ROW: OnceLock<Regex> = OnceLock::new();
    static CELL: OnceLock<Regex> = OnceLock::new();
    let para = re(&PARA, r"(?s)<w:p[ >].*?</w:p>");
    let text = re(&TEXT, r"(?s)<w:t(?: [^>]*)?>(.*?)</w:t>");
    let style = re(&STYLE, r#"<w:pStyle w:val="([^"]+)""#);
    let row = re(&ROW, r"(?s)<w:tr[ >].*?</w:tr>");
    let cell = re(&CELL, r"(?s)<w:tc[ >].*?</w:tc>");

    // Tables first: replace each row by one paragraph of " | "-joined cells.
    let mut flat = String::with_capacity(xml.len());
    let mut last = 0;
    for r in row.find_iter(xml) {
        flat.push_str(&xml[last..r.start()]);
        let cells: Vec<String> = cell
            .find_iter(r.as_str())
            .map(|c| {
                text.captures_iter(c.as_str())
                    .map(|t| xml_unescape(&t[1]))
                    .collect::<Vec<_>>()
                    .join("")
                    .trim()
                    .to_string()
            })
            .filter(|c| !c.is_empty())
            .collect();
        flat.push_str(&format!("<w:p><w:r><w:t>{}</w:t></w:r></w:p>", cells.join(" | ").replace('<', "&lt;")));
        last = r.end();
    }
    flat.push_str(&xml[last..]);

    let mut out: Sections = Vec::new();
    let mut heading = String::new();
    let mut buf = String::new();
    for p in para.find_iter(&flat) {
        let p = p.as_str();
        let t: String = text.captures_iter(p).map(|c| xml_unescape(&c[1])).collect();
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        let st = style.captures(p).map(|c| c[1].to_lowercase()).unwrap_or_default();
        let runs = p.matches("<w:r>").count() + p.matches("<w:r ").count();
        let bold_runs = p.matches("<w:b/>").count() + p.matches("<w:b ").count();
        let is_heading = st.contains("heading")
            || st == "title"
            || st == "subtitle"
            || (runs > 0 && bold_runs >= runs && t.chars().count() <= 80 && !t.ends_with('.'));
        if is_heading {
            if !buf.trim().is_empty() {
                out.push((heading.clone(), buf.trim().to_string()));
            }
            buf.clear();
            heading = t.to_string();
        } else {
            buf.push_str(t);
            buf.push('\n');
        }
    }
    if !buf.trim().is_empty() {
        out.push((heading, buf.trim().to_string()));
    }
    out
}

pub fn markdown_sections(raw: &str) -> Sections {
    let mut out: Sections = Vec::new();
    let mut heading = String::new();
    let mut buf = String::new();
    for line in raw.lines() {
        if let Some(h) = line.strip_prefix('#') {
            if !buf.trim().is_empty() {
                out.push((heading.clone(), buf.trim().to_string()));
            }
            buf.clear();
            heading = h.trim_start_matches('#').trim().to_string();
        } else {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    if !buf.trim().is_empty() {
        out.push((heading, buf.trim().to_string()));
    }
    out
}

/// Plain text / PDF text. With `detect_headings`, short title-like lines
/// ("EXPERIENCE", "Technical Skills") start a new section.
pub fn text_sections(raw: &str, detect_headings: bool) -> Sections {
    static HEAD: OnceLock<Regex> = OnceLock::new();
    let head = re(&HEAD, r"^[A-Z][A-Za-z0-9&/ ,()+.-]{2,60}:?$");
    let mut out: Sections = Vec::new();
    let mut heading = String::new();
    let mut buf = String::new();
    for line in raw.lines() {
        let l = line.trim();
        let looks_like_heading = detect_headings
            && !l.is_empty()
            && !l.contains(',') // "Python, Rust" is a list, not a title
            && l.chars().count() <= 60
            && head.is_match(l)
            && !l.ends_with('.')
            && (l.chars().filter(|c| c.is_alphabetic()).all(|c| c.is_uppercase())
                || l.split_whitespace().count() <= 4);
        if looks_like_heading {
            if !buf.trim().is_empty() {
                out.push((heading.clone(), buf.trim().to_string()));
            }
            buf.clear();
            heading = l.trim_end_matches(':').to_string();
        } else {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    if !buf.trim().is_empty() {
        out.push((heading, buf.trim().to_string()));
    }
    out
}

/// JSON: one section per top-level key, with nested key paths kept
/// ("experience[0].company: Acme") so the meaning survives chunking.
pub fn json_sections(raw: &str) -> Sections {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return text_sections(raw, false);
    };
    let mut out = Vec::new();
    match &v {
        serde_json::Value::Object(m) => {
            for (k, val) in m {
                let mut lines = Vec::new();
                flatten(&human(k), val, &mut lines);
                if !lines.is_empty() {
                    out.push((human(k), lines.join("\n")));
                }
            }
        }
        other => {
            let mut lines = Vec::new();
            flatten("", other, &mut lines);
            out.push((String::new(), lines.join("\n")));
        }
    }
    out
}

/// "expected_text" → "expected text" (searchable words instead of identifiers).
fn human(key: &str) -> String {
    key.replace(['_', '-'], " ")
}

fn flatten(prefix: &str, v: &serde_json::Value, out: &mut Vec<String>) {
    use serde_json::Value;
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                let key = if prefix.is_empty() { human(k) } else { format!("{prefix} › {}", human(k)) };
                flatten(&key, v, out);
            }
        }
        Value::Array(a) if a.iter().all(|x| !x.is_object() && !x.is_array()) => {
            let items: Vec<String> = a
                .iter()
                .map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string()))
                .collect();
            if !items.is_empty() {
                out.push(format!("{prefix}: {}", items.join(", ")));
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                flatten(&format!("{prefix}[{i}]"), x, out);
            }
        }
        Value::Null => {}
        Value::String(s) if s.trim().is_empty() => {}
        Value::String(s) => out.push(format!("{prefix}: {s}")),
        other => out.push(format!("{prefix}: {other}")),
    }
}

// ---------------------------------------------------------------- chunking

/// Split sections into chunks of about `CHUNK_CHARS` with `OVERLAP_CHARS`
/// overlap, breaking at paragraph, line or sentence boundaries and never
/// across sections. Titles read "Document › Section".
pub fn chunk(doc_title: &str, sections: &Sections) -> Vec<Chunk> {
    let mut out = Vec::new();
    for (section, text) in sections {
        let title = if section.is_empty() {
            doc_title.to_string()
        } else {
            format!("{doc_title} › {section}")
        };
        for piece in split(text, CHUNK_CHARS, OVERLAP_CHARS) {
            let content = piece.trim().to_string();
            if content.is_empty() {
                continue;
            }
            out.push(Chunk {
                ord: out.len(),
                section: section.clone(),
                hash: sha(format!("{title}\n{content}")),
                title: title.clone(),
                content,
            });
        }
    }
    out
}

/// Split text into windows of at most `max` chars with `overlap`.
pub fn split(text: &str, max: usize, overlap: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let hard_end = (start + max).min(chars.len());
        let mut end = hard_end;
        if hard_end < chars.len() {
            // prefer a paragraph, then a line, then a sentence, then a space break
            let window: String = chars[start..hard_end].iter().collect();
            let min_keep = max / 2;
            let pick = ["\n\n", "\n", ". ", " "].iter().find_map(|sep| {
                window.rfind(sep).filter(|&i| window[..i].chars().count() >= min_keep).map(|i| {
                    window[..i + sep.len()].chars().count()
                })
            });
            if let Some(n) = pick {
                end = start + n;
            }
        }
        out.push(chars[start..end].iter().collect());
        if end >= chars.len() {
            break;
        }
        start = end.saturating_sub(overlap).max(start + 1);
    }
    out
}

/// Readable document title from a path ("people/a/resumes/dotnet/resume.docx" → "resume (dotnet)").
pub fn doc_title(rel: &str, meta: &ChunkMeta) -> String {
    let file = rel.rsplit('/').next().unwrap_or(rel);
    let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file).replace(['_', '-'], " ");
    match &meta.stack {
        Some(s) => format!("{stem} ({s})"),
        None => stem,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_from_paths() {
        let m = meta_for("people/shashi/resumes/dotnet/resume.docx");
        assert_eq!(m.person_id.as_deref(), Some("shashi"));
        assert_eq!(m.doc_type, "resume");
        assert_eq!(m.stack.as_deref(), Some("dotnet"));
        let m = meta_for("people/shashi/job_seeking/replies/recruiting.md");
        assert_eq!(m.purpose.as_deref(), Some("job_seeking"));
        assert_eq!(m.doc_type, "reply_guide");
        let m = meta_for("people/shashi/consulting/resumes/genai/profile.pdf");
        assert_eq!((m.purpose.as_deref(), m.stack.as_deref(), m.doc_type.as_str()), (Some("consulting"), Some("genai"), "resume"));
        assert_eq!(meta_for("people/shashi/job_seeking/salary.json").doc_type, "compensation");
        assert_eq!(meta_for("people/shashi/employment_history.json").doc_type, "experience");
        assert_eq!(meta_for("shared/house-rules.md").doc_type, "shared");
    }

    #[test]
    fn docx_headings_and_tables() {
        let xml = r#"<w:document><w:body>
            <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Experience</w:t></w:r></w:p>
            <w:p><w:r><w:t>Lead Backend Engineer at McKinsey &amp; Company.</w:t></w:r></w:p>
            <w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Skills</w:t></w:r></w:p>
            <w:tbl><w:tr><w:tc><w:p><w:r><w:t>Python</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>8 years</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
            </w:body></w:document>"#;
        let s = docx_xml_sections(xml);
        assert_eq!(s[0].0, "Experience");
        assert!(s[0].1.contains("McKinsey & Company"));
        assert_eq!(s[1].0, "Skills", "a short all-bold paragraph is a heading");
        assert_eq!(s[1].1, "Python | 8 years", "table rows keep their cells together");
    }

    #[test]
    fn json_keeps_key_paths() {
        let s = json_sections(r#"{"identity": {"full_name": "Asha"}, "jobs": [{"company": "Acme", "years": 3}]}"#);
        assert_eq!(s[0], ("identity".to_string(), "identity › full name: Asha".to_string()));
        assert!(s[1].1.contains("jobs[0] › company: Acme"));
        assert!(s[1].1.contains("jobs[0] › years: 3"));
    }

    #[test]
    fn pdf_style_text_headings() {
        let s = text_sections("SUMMARY\nArchitect with 12 years.\nTechnical Skills\nPython, Rust\n", true);
        assert_eq!(s.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), vec!["SUMMARY", "Technical Skills"]);
    }

    #[test]
    fn chunks_overlap_and_stay_in_their_section() {
        let long = "Sentence about distributed systems. ".repeat(80);
        let secs: Sections = vec![("Experience".into(), long.clone()), ("Skills".into(), "Python".into())];
        let c = chunk("resume", &secs);
        assert!(c.len() >= 3, "{}", c.len());
        assert!(c.iter().all(|x| x.content.chars().count() <= CHUNK_CHARS));
        assert_eq!(c.last().unwrap().title, "resume › Skills");
        let a: String = c[0].content.chars().rev().take(40).collect();
        let tail: String = a.chars().rev().collect();
        assert!(c[1].content.contains(tail.trim()), "consecutive chunks overlap");
    }
}
