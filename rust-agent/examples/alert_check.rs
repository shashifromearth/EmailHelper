//! Parse every `<source>_*.txt` alert body in a folder and summarise.
fn main() {
    let dir = std::env::args().nth(1).expect("usage: alert_check <dir>");
    let mut files: Vec<_> = std::fs::read_dir(&dir).expect("dir").flatten().map(|e| e.path()).collect();
    files.sort();
    let (mut emails, mut empty, mut jobs, mut with_company) = (0, 0, 0, 0);
    for f in files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let source = name.split('_').next().unwrap_or("").to_string();
        let body = std::fs::read_to_string(&f).unwrap_or_default();
        let found = emailhelper::jobs::alerts::parse(&source, &body);
        emails += 1;
        if found.is_empty() {
            empty += 1;
            println!("EMPTY {name}: {}", body.lines().filter(|l| !l.trim().is_empty()).take(3).collect::<Vec<_>>().join(" | "));
        }
        for j in &found {
            jobs += 1;
            if j.company.is_some() {
                with_company += 1;
            }
        }
        if let Some(j) = found.first() {
            println!("{name}: {} job(s); first: {} | {} | {}", found.len(), j.title, j.company.as_deref().unwrap_or("-"), j.location.as_deref().unwrap_or("-"));
        }
    }
    println!("\nemails {emails}, no jobs parsed {empty}, jobs {jobs}, with company {with_company}");
}
