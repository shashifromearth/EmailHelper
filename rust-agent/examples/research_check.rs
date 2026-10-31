//! Run one source's postings through the research filters and print the verdicts.
//! usage: research_check <source> [board-or-url|-] [title-substring]
//!   source: greenhouse|lever|ashby <board>
//!           remotive|remoteok|himalayas|jobicy|weworkremotely [url|-]
//!           adzuna|jooble|careerjet|arbeitnow|themuse|google_cse|brave  (keys and settings from .env)
//! Filters come from .env too (RESEARCH_MAX_AGE_HOURS, RESEARCH_TECH_STACK, …).
//! ROLE_TYPES=none drops the role-type filter, REMOTE_ONLY=true requires remote.
#[tokio::main]
async fn main() {
    use emailhelper::research::{filters, job_apis, remote_boards, sources};
    let _ = dotenvy::dotenv();
    let args: Vec<String> = std::env::args().collect();
    let Some(kind) = args.get(1).map(String::as_str) else {
        eprintln!("usage: research_check <source> [board-or-url|-] [title-substring]");
        std::process::exit(2);
    };
    let board = args.get(2).map(String::as_str).unwrap_or("-");
    let want = args.get(3).map(|s| s.to_lowercase());
    let cfg = emailhelper::config::Config::from_lookup(|k| match k {
        "RESEARCH_SOURCES" => Some(kind.to_string()),
        _ => std::env::var(k).ok(),
    })
    .expect("config");
    let r = &cfg.research;
    let http = reqwest::Client::new();
    let jobs = match kind {
        "greenhouse" => sources::parse_greenhouse(board, &sources::fetch_json(&http, &format!("{}/{board}/jobs?content=true", r.greenhouse_api_base)).await.unwrap()),
        "lever" => sources::parse_lever(board, &sources::fetch_json(&http, &format!("{}/{board}?mode=json", r.lever_api_base)).await.unwrap()),
        "ashby" => sources::parse_ashby(board, &sources::fetch_json(&http, &format!("{}/{board}?includeCompensation=true", r.ashby_api_base)).await.unwrap()),
        k if job_apis::SOURCES.contains(&k) => {
            if let Some(key) = job_apis::missing_key(&r.apis, k) {
                eprintln!("{k}: set {key} in .env first");
                std::process::exit(2);
            }
            let mut errors = Vec::new();
            let jobs = job_apis::collect(&http, &r.apis, r.max_age_hours, &mut errors).await;
            for e in errors {
                eprintln!("error: {e}");
            }
            jobs
        }
        k => {
            let urls: Vec<&str> = if board == "-" { remote_boards::default_urls(k).to_vec() } else { vec![board] };
            let mut all = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for u in urls {
                for j in remote_boards::fetch(&http, k, u).await.unwrap_or_default() {
                    if seen.insert(j.details.url.clone().unwrap_or_default()) {
                        all.push(j);
                    }
                }
            }
            all
        }
    };
    let c = filters::Criteria {
        max_age_hours: r.max_age_hours,
        country: r.applicant_country.clone().unwrap_or_else(|| "India".into()),
        stack: r.tech_stack.clone(),
        work_types: if std::env::var("ROLE_TYPES").as_deref() == Ok("none") { Vec::new() } else { r.work_types.clone() },
        require_remote: r.require_remote || std::env::var("REMOTE_ONLY").as_deref() == Ok("true"),
    };
    let now = chrono::Utc::now();
    let (mut kept, mut total) = (0, 0);
    for j in &jobs {
        if want.as_ref().is_some_and(|w| !j.details.title.to_lowercase().contains(w)) {
            continue;
        }
        total += 1;
        let v = filters::check(j, &c, now);
        if v.is_ok() {
            kept += 1;
        }
        if want.is_some() || v.is_ok() {
            let (boost, why) = filters::priority(j, &r.preferred_work_types, r.priority_boost);
            println!(
                "{:?} | +{boost} {why:?} | {} | {} | region {:?} | posted {:?} | type {:?} | {:?}",
                v, j.details.title, j.details.company.as_deref().unwrap_or("-"), j.region, j.posted_at, j.employment_type, j.details.url
            );
        }
    }
    println!("{kept} kept of {total}");
}
