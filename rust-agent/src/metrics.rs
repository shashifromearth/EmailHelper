//! Prometheus metrics. Names and labels are part of the public contract used
//! by `monitoring/` dashboards and alerts; don't rename them casually.

use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, IntGaugeVec, Opts,
    Registry, TextEncoder,
};

pub struct Metrics {
    pub registry: Registry,
    pub emails_processed_total: IntCounterVec,
    pub email_duplicates_total: IntCounter,
    pub drafts_created_total: IntCounterVec,
    pub validation_failures_total: IntCounter,
    pub agent_failures_total: IntCounterVec,
    pub failures_total: IntCounterVec,
    pub processing_duration_seconds: HistogramVec,
    pub llm_requests_total: IntCounterVec,
    pub llm_request_duration_seconds: HistogramVec,
    pub queue_depth: IntGauge,
    pub jobs_detected_total: IntCounterVec,
    pub jobs_discovered_total: IntCounterVec,
    pub applications_started_total: IntCounter,
    pub applications_completed_total: IntCounter,
    pub applications_failed_total: IntCounterVec,
    pub applications_blocked_total: IntCounterVec,
    pub applications_in_progress: IntGauge,
    pub recruiter_matches_total: IntCounter,
    pub telegram_notifications_total: IntCounterVec,
    pub telegram_requests_total: IntCounterVec,
    pub playwright_failures_total: IntCounterVec,
    pub research_runs_total: IntCounterVec,
    pub build_info: IntGaugeVec,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

fn counter(r: &Registry, name: &str, help: &str) -> IntCounter {
    let c = IntCounter::new(name, help).expect("valid metric");
    r.register(Box::new(c.clone())).expect("unique metric");
    c
}

fn counter_vec(r: &Registry, name: &str, help: &str, labels: &[&str]) -> IntCounterVec {
    let c = IntCounterVec::new(Opts::new(name, help), labels).expect("valid metric");
    r.register(Box::new(c.clone())).expect("unique metric");
    c
}

fn histogram_vec(
    r: &Registry,
    name: &str,
    help: &str,
    labels: &[&str],
    buckets: Vec<f64>,
) -> HistogramVec {
    let h = HistogramVec::new(HistogramOpts::new(name, help).buckets(buckets), labels)
        .expect("valid metric");
    r.register(Box::new(h.clone())).expect("unique metric");
    h
}

impl Metrics {
    pub fn new() -> Self {
        let r = Registry::new();
        let duration_buckets = vec![
            0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0,
        ];
        let queue_depth = IntGauge::new("queue_depth", "Jobs queued or running").unwrap();
        r.register(Box::new(queue_depth.clone())).unwrap();
        let in_progress =
            IntGauge::new("applications_in_progress", "Applications currently running").unwrap();
        r.register(Box::new(in_progress.clone())).unwrap();
        let build_info =
            IntGaugeVec::new(Opts::new("build_info", "Build information"), &["version"]).unwrap();
        r.register(Box::new(build_info.clone())).unwrap();
        build_info
            .with_label_values(&[env!("CARGO_PKG_VERSION")])
            .set(1);

        Self {
            emails_processed_total: counter_vec(
                &r,
                "emails_processed_total",
                "Emails fully processed",
                &["account_type", "category"],
            ),
            email_duplicates_total: counter(
                &r,
                "email_duplicates_total",
                "Duplicate email submissions ignored",
            ),
            drafts_created_total: counter_vec(
                &r,
                "drafts_created_total",
                "Drafts created",
                &["validation"],
            ),
            validation_failures_total: counter(
                &r,
                "validation_failures_total",
                "Drafts that failed validation (REVIEW_REQUIRED)",
            ),
            agent_failures_total: counter_vec(
                &r,
                "agent_failures_total",
                "Agent step failures",
                &["agent"],
            ),
            failures_total: counter_vec(
                &r,
                "failures_total",
                "Failures by component",
                &["component"],
            ),
            processing_duration_seconds: histogram_vec(
                &r,
                "processing_duration_seconds",
                "End-to-end workflow duration",
                &["workflow"],
                duration_buckets.clone(),
            ),
            llm_requests_total: counter_vec(
                &r,
                "llm_requests_total",
                "LLM requests",
                &["provider", "status"],
            ),
            llm_request_duration_seconds: histogram_vec(
                &r,
                "llm_request_duration_seconds",
                "LLM request latency",
                &["provider"],
                duration_buckets,
            ),
            queue_depth,
            jobs_detected_total: counter_vec(
                &r,
                "jobs_detected_total",
                "Job opportunities detected in email",
                &["source"],
            ),
            jobs_discovered_total: counter_vec(
                &r,
                "jobs_discovered_total",
                "Jobs discovered by research",
                &["source"],
            ),
            applications_started_total: counter(
                &r,
                "applications_started_total",
                "Applications started",
            ),
            applications_completed_total: counter(
                &r,
                "applications_completed_total",
                "Applications submitted",
            ),
            applications_failed_total: counter_vec(
                &r,
                "applications_failed_total",
                "Applications failed",
                &["reason"],
            ),
            applications_blocked_total: counter_vec(
                &r,
                "applications_blocked_total",
                "Applications paused for a human",
                &["reason"],
            ),
            applications_in_progress: in_progress,
            recruiter_matches_total: counter(
                &r,
                "recruiter_matches_total",
                "Recruiter contacts found",
            ),
            telegram_notifications_total: counter_vec(
                &r,
                "telegram_notifications_total",
                "Telegram notifications sent",
                &["event"],
            ),
            telegram_requests_total: counter_vec(
                &r,
                "telegram_requests_total",
                "Telegram API traffic",
                &["direction"],
            ),
            playwright_failures_total: counter_vec(
                &r,
                "playwright_failures_total",
                "Browser worker failures",
                &["operation"],
            ),
            research_runs_total: counter_vec(
                &r,
                "research_runs_total",
                "Research discovery runs",
                &["status"],
            ),
            build_info,
            registry: r,
        }
    }

    pub fn render(&self) -> String {
        let mut buf = Vec::new();
        let enc = TextEncoder::new();
        if let Err(e) = enc.encode(&self.registry.gather(), &mut buf) {
            tracing::error!(error = %e, "failed to encode metrics");
        }
        String::from_utf8(buf).unwrap_or_default()
    }

    pub fn failure(&self, component: &str) {
        self.failures_total.with_label_values(&[component]).inc();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_required_metric_names() {
        let m = Metrics::new();
        m.emails_processed_total
            .with_label_values(&["gmail", "support"])
            .inc();
        m.drafts_created_total
            .with_label_values(&["APPROVED"])
            .inc();
        m.agent_failures_total
            .with_label_values(&["classifier"])
            .inc();
        m.processing_duration_seconds
            .with_label_values(&["email"])
            .observe(1.0);
        m.jobs_discovered_total
            .with_label_values(&["greenhouse"])
            .inc();
        m.jobs_detected_total.with_label_values(&["linkedin"]).inc();
        m.applications_failed_total.with_label_values(&["x"]).inc();
        m.applications_blocked_total
            .with_label_values(&["captcha"])
            .inc();
        m.telegram_notifications_total
            .with_label_values(&["draft_created"])
            .inc();
        m.telegram_requests_total.with_label_values(&["out"]).inc();
        m.playwright_failures_total
            .with_label_values(&["fill"])
            .inc();
        m.research_runs_total.with_label_values(&["ok"]).inc();
        m.llm_requests_total
            .with_label_values(&["ollama", "ok"])
            .inc();
        m.llm_request_duration_seconds
            .with_label_values(&["ollama"])
            .observe(0.5);
        m.failure("api");
        let out = m.render();
        for name in [
            "emails_processed_total",
            "email_duplicates_total",
            "drafts_created_total",
            "validation_failures_total",
            "agent_failures_total",
            "failures_total",
            "processing_duration_seconds_bucket",
            "llm_requests_total",
            "llm_request_duration_seconds_bucket",
            "queue_depth",
            "jobs_detected_total",
            "jobs_discovered_total",
            "applications_started_total",
            "applications_completed_total",
            "applications_failed_total",
            "applications_blocked_total",
            "applications_in_progress",
            "recruiter_matches_total",
            "telegram_notifications_total",
            "telegram_requests_total",
            "playwright_failures_total",
            "research_runs_total",
            "build_info",
        ] {
            assert!(out.contains(name), "missing {name}");
        }
    }
}
