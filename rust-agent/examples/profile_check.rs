//! Show how the agent sees the knowledge folder: people, mailboxes, profiles,
//! resumes and which resume it would pick for sample job descriptions.
//! `cargo run -p emailhelper --example profile_check -- knowledge`
use emailhelper::db::jobs::JobDetails;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "knowledge".into());
    let people = emailhelper::knowledge::People::load(std::path::Path::new(&dir));
    println!(
        "mode: {}",
        if people.legacy {
            "single person (no people.json)"
        } else {
            "people.json"
        }
    );
    for p in &people.persons {
        let i = &p.profile.identity;
        println!("\n== {} ({})", p.name, p.id);
        for m in &p.mailboxes {
            println!("   mailbox {} -> {:?}", m.address, m.purposes);
        }
        println!(
            "   job agent: {} | telegram chat: {:?}",
            p.job_agent, p.telegram_chat_id
        );
        println!("   contact: {} | {} | {}", i.email, i.phone, i.location);
        println!(
            "   current: {} @ {} | notice: {:?}",
            i.current_title, i.current_company, i.notice_period
        );
        println!(
            "   salary: expected {:?} current {:?} | private facts: {:?}",
            p.profile.salary.expected_text,
            p.profile.salary.current_text,
            p.private.values.keys().collect::<Vec<_>>()
        );
        println!(
            "   apply guard: {}",
            emailhelper::jobs::application::profile_problem(&p.profile)
                .unwrap_or_else(|| "ok".into())
        );
        println!("   resumes ({}):", p.resumes.len());
        for r in &p.resumes {
            println!(
                "     {}{} name→{:?} text skills: {}",
                r.file_name,
                if r.default { " [default]" } else { "" },
                r.name_tags,
                r.tags.len()
            );
        }
        if p.resumes.is_empty() {
            continue;
        }
        let samples = [
            "Senior .NET Developer: C#, ASP.NET Core, Azure, SQL Server",
            "Golang Backend Engineer: Go, Kubernetes, gRPC, AWS",
            "Java Architect: Java, Spring Boot, Kafka, AWS",
            "Rust Systems Engineer: Rust, Tokio, distributed systems",
            "GenAI Architect: Python, LangChain, RAG, LLMs, Azure OpenAI",
        ];
        let mut vocab = p.profile.all_skills();
        for r in &p.resumes {
            for t in r.name_tags.iter().chain(r.tags.iter()) {
                if !vocab.iter().any(|v| v.eq_ignore_ascii_case(t)) {
                    vocab.push(t.clone());
                }
            }
        }
        println!("   resume choice for sample jobs:");
        for jd in samples {
            let mut d = JobDetails::default();
            emailhelper::jobs::extract::enrich_from_text(&mut d, jd, &vocab);
            if let Some((r, matched)) = p.select_resume(&d.skills, Some("job_seeking")) {
                println!("     {:<62} -> {} {:?}", jd, r.file_name, matched);
            }
        }
    }
}
