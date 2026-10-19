//! Plain-text extraction from Word `.docx` files (resumes). A `.docx` is a zip
//! archive whose body lives in `word/document.xml`; paragraphs are `<w:p>`
//! elements and text runs are `<w:t>`.

use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

const MAX_XML_BYTES: u64 = 20 * 1024 * 1024;

/// Extract paragraph text from a `.docx` file, one paragraph per line.
pub fn extract_text(path: &Path) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("not a docx: {e}"))?;
    let mut entry = zip
        .by_name("word/document.xml")
        .map_err(|e| format!("missing word/document.xml: {e}"))?;
    if entry.size() > MAX_XML_BYTES {
        return Err("document.xml too large".into());
    }
    let mut xml = String::new();
    entry.read_to_string(&mut xml).map_err(|e| e.to_string())?;
    Ok(text_from_document_xml(&xml))
}

/// Convert WordprocessingML body XML to plain text.
pub fn text_from_document_xml(xml: &str) -> String {
    static PARA: OnceLock<Regex> = OnceLock::new();
    static RUN: OnceLock<Regex> = OnceLock::new();
    let para = PARA.get_or_init(|| Regex::new(r"(?s)<w:p[ >].*?</w:p>").expect("valid regex"));
    let run = RUN.get_or_init(|| {
        Regex::new(r"<w:t(?:\s[^>]*)?>([^<]*)</w:t>|<w:tab/>").expect("valid regex")
    });
    let mut out = Vec::new();
    for p in para.find_iter(xml) {
        let mut line = String::new();
        for c in run.captures_iter(p.as_str()) {
            match c.get(1) {
                Some(t) => line.push_str(t.as_str()),
                None => line.push(' '),
            }
        }
        let line = unescape(line.trim());
        if !line.is_empty() {
            out.push(line);
        }
    }
    out.join("\n")
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    pub fn write_docx(path: &Path, paragraphs: &[&str]) {
        let body: String = paragraphs
            .iter()
            .map(|p| format!("<w:p><w:r><w:t xml:space=\"preserve\">{p}</w:t></w:r></w:p>"))
            .collect();
        let xml = format!(
            "<?xml version=\"1.0\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>{body}</w:body></w:document>"
        );
        let f = std::fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        z.start_file(
            "word/document.xml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        z.write_all(xml.as_bytes()).unwrap();
        z.finish().unwrap();
    }

    #[test]
    fn extracts_paragraphs() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("cv.docx");
        write_docx(
            &p,
            &["Jane Doe", "Skills: C#, .NET 8 &amp; ASP.NET Core", ""],
        );
        let t = extract_text(&p).unwrap();
        assert_eq!(t, "Jane Doe\nSkills: C#, .NET 8 & ASP.NET Core");
    }

    #[test]
    fn rejects_non_docx() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.docx");
        std::fs::write(&p, b"not a zip").unwrap();
        assert!(extract_text(&p).is_err());
    }
}
