//! Run one board's postings through the research filters and print the verdicts.
//! usage: research_check <greenhouse|lever|ashby|remotive|remoteok|himalayas|jobicy|weworkremotely> <board-or-url> [title-substring]
#[tokio::main]
async fn main() {
    use emailhelper::research::{filters, remote_boards, sources};
    let args: Vec<String> = std::env::args().collect();
    let (kind, board) = (args[1].as_str(), args[2].as_str());
    let want = args.get(3).map(|s| s.to_lowercase());
    let http = reqwest::Client::new();
    let jobs = match kind {
        "greenhouse" => sources::parse_greenhouse(board, &sources::fetch_json(&http, &format!("https://boards-api.greenhouse.io/v1/boards/{board}/jobs?content=true")).await.unwrap()),
        "lever" => sources::parse_lever(board, &sources::fetch_json(&http, &format!("https://api.lever.co/v0/postings/{board}?mode=json")).await.unwrap()),
        "ashby" => sources::parse_ashby(board, &sources::fetch_json(&http, &format!("https://api.ashbyhq.com/posting-api/job-board/{board}?includeCompensation=true")).await.unwrap()),
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
        max_age_hours: Some(25),
        country: "India".into(),
        stack: ["python", "c#", ".net", "java", "genai", "agentic ai"].iter().map(|s| s.to_string()).collect(),
        // ROLE_TYPES=none to see what full-time remote jobs would add
        work_types: if std::env::var("ROLE_TYPES").as_deref() == Ok("none") {
            Vec::new()
        } else {
            ["contract", "freelance", "consultant"].iter().map(|s| s.to_string()).collect()
        },
        require_remote: true,
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
            println!("{:?} | {} | {} | region {:?} | posted {:?} | type {:?} | {:?}", v, j.details.title, j.details.company.as_deref().unwrap_or("-"), j.region, j.posted_at, j.employment_type, j.details.url);
        }
    }
    println!("{kept} kept of {total}");
}
