//! Load / performance tests: concurrency, throughput, latency and cold start.

mod common;

use std::time::{Duration, Instant};

use common::*;
use emailhelper::llm::MockProvider;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn processes_fifty_concurrent_emails_with_bounded_concurrency() {
    // 50 emails; each LLM call takes 100 ms (3 calls per email).
    let a = builder()
        .llm(MockProvider::default().with_delay(100))
        .env("EH_MAX_CONCURRENCY", "10")
        .start()
        .await;
    let started = Instant::now();
    let mut handles = Vec::new();
    for i in 0..50 {
        let http = a.http.clone();
        let url = a.url("/api/v1/emails");
        let body = email(
            &format!("acct{}", i % 4),
            &format!("<load-{i}@example.com>"),
            "client@example.org",
            &format!("Support request #{i}"),
            "Hello, our dashboard shows an error when exporting reports. Could you take a look?",
        );
        handles.push(tokio::spawn(async move {
            let t = Instant::now();
            let r = http
                .post(url)
                .header("X-EmailHelper-Token", TOKEN)
                .json(&body)
                .send()
                .await
                .unwrap();
            (r.status().as_u16(), t.elapsed())
        }));
    }
    let mut max_accept = Duration::ZERO;
    for h in handles {
        let (status, took) = h.await.unwrap();
        assert_eq!(status, 202);
        max_accept = max_accept.max(took);
    }
    assert!(
        max_accept < Duration::from_secs(2),
        "ingest latency {max_accept:?}"
    );

    let drafts = a.collect_drafts(50, Duration::from_secs(60)).await;
    let elapsed = started.elapsed();
    assert_eq!(drafts.len(), 50);
    // Serial would take >= 50 * 0.3 s = 15 s; 10-way concurrency ~1.5 s.
    assert!(
        elapsed < Duration::from_secs(12),
        "50 emails took {elapsed:?}"
    );
    println!(
        "load: 50 emails in {elapsed:?} ({:.1} emails/s), max ingest {max_accept:?}",
        50.0 / elapsed.as_secs_f64()
    );

    // Drafts are handed out just before each workflow's final bookkeeping.
    wait_for(
        "all workflows finished",
        Duration::from_secs(10),
        || async {
            let m = a.metrics().await;
            (metric_sum(&m, "emails_processed_total") == 50.0
                && metric_sum(&m, "queue_depth") == 0.0)
                .then_some(())
        },
    )
    .await;
}

#[tokio::test]
async fn single_draft_latency_is_well_under_target() {
    let a = builder().start().await;
    let t = Instant::now();
    a.post(
        "/api/v1/emails",
        &email(
            "acct1",
            "<lat@x>",
            "jane@acme.com",
            "Pricing",
            "What does a two-week prototype sprint cost?",
        ),
    )
    .await;
    let d = a.collect_drafts(1, Duration::from_secs(15)).await;
    assert_eq!(d.len(), 1);
    // Excluding real model inference, orchestration overhead must be tiny
    // relative to the 15 s draft target.
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
}

#[tokio::test]
async fn cold_start_is_fast() {
    let t = Instant::now();
    let a = builder().start().await;
    let r = a.http.get(a.url("/readiness")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "cold start {:?}",
        t.elapsed()
    );
}
