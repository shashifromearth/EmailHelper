# Monitoring Guide

The full guide (alerts, PromQL, manual dashboard import) is in
[`monitoring/README.md`](../monitoring/README.md). This page summarises it.

## Endpoints (agent, 127.0.0.1:7800)

| Endpoint | Auth | Purpose |
|---|---|---|
| `/health` | none | Component checks: database, llm, browser_worker, telegram |
| `/liveness` | none | 200 while the process is up |
| `/readiness` | none | 200 when the database is open and migrated (used by `emailhelper healthcheck`) |
| `/metrics` | none | Prometheus exposition |
| `/dashboard` | token (entered in page) | Applied / in-progress / blocked / failed applications, interviews, recruiters, company pipeline, open questions |
| `/api/v1/status` | token | Summary used by the Thunderbird popup |

## Metrics

| Metric | Type | Labels |
|---|---|---|
| `emails_processed_total` | counter | `account_type`, `category` |
| `email_duplicates_total` | counter | — |
| `drafts_created_total` | counter | `validation` |
| `validation_failures_total` | counter | — |
| `agent_failures_total` | counter | `agent` |
| `failures_total` | counter | `component` |
| `processing_duration_seconds` | histogram | `workflow` (email, job_application, research) |
| `llm_requests_total` | counter | `provider`, `status` (ok, error, fallback) |
| `llm_request_duration_seconds` | histogram | `provider` |
| `queue_depth` | gauge | — |
| `jobs_detected_total` | counter | `source` |
| `jobs_discovered_total` | counter | `source` |
| `applications_started_total` / `applications_completed_total` | counter | — |
| `applications_failed_total` / `applications_blocked_total` | counter | `reason` |
| `applications_in_progress` | gauge | — |
| `recruiter_matches_total` | counter | — |
| `telegram_notifications_total` | counter | `event` |
| `telegram_requests_total` | counter | `direction` |
| `playwright_failures_total` | counter | `operation` |
| `research_runs_total` | counter | `status` |
| `build_info` | gauge | `version` |

## Stack

- **Native agent with containerised monitoring:**
  `docker compose -f docker/docker-compose.monitoring.yml --env-file .env up -d`.
  Prometheus scrapes `host.docker.internal:7800`.
- **Everything in Docker:**
  `docker compose -f docker/docker-compose.yml --env-file .env --profile monitoring up -d`.

| Service | URL |
|---|---|
| Prometheus | http://127.0.0.1:9090 |
| Grafana | http://127.0.0.1:3000 (admin password from `GRAFANA_ADMIN_PASSWORD`) |

Dashboards provisioned automatically:

- **EmailHelper – Overview:** email throughput, drafts, validation, latency
  p50/p95, queue, LLM, failures.
- **EmailHelper – Jobs & Research:** detection, discovery, application funnel,
  blocked reasons, recruiters, Playwright failures.

## Logs

Logs are JSON lines on stdout (`EH_LOG_FORMAT=json`) with `level`,
`timestamp`, `message` and structured fields: `email_id`, `job_id`, `kind`,
`application_id`, `duration_ms`, `category`, `outcome`. Ship them with any
collector (Loki/Promtail, Vector, journald). Level control:
`RUST_LOG=info,emailhelper=debug`.
