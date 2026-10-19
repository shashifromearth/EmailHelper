# EmailHelper monitoring

Prometheus scrapes the agent's `/metrics` endpoint every 15 s, evaluates the
alert rules in `prometheus/alerts.yml`, and Grafana shows two provisioned
dashboards. Everything listens on `127.0.0.1` only.

```
monitoring/
├── prometheus/
│   ├── prometheus.yml          # compose stack: scrapes emailhelper:7800
│   ├── prometheus.native.yml   # agent runs natively: scrapes host.docker.internal:7800
│   └── alerts.yml              # alert rules
└── grafana/
    ├── provisioning/
    │   ├── datasources/prometheus.yml   # datasource "Prometheus", uid "prometheus"
    │   └── dashboards/emailhelper.yml   # loads dashboards/*.json into folder "EmailHelper"
    └── dashboards/
        ├── emailhelper-overview.json    # uid emailhelper-overview
        └── emailhelper-jobs.json        # uid emailhelper-jobs
```

## Endpoints

| Endpoint | URL | Auth |
|---|---|---|
| Agent health (JSON with per-dependency checks) | http://127.0.0.1:7800/health | none |
| Agent readiness (200 once DB is open and migrated, else 503) | http://127.0.0.1:7800/readiness | none |
| Agent liveness (200 while the process runs) | http://127.0.0.1:7800/liveness | none |
| Agent metrics (Prometheus text format) | http://127.0.0.1:7800/metrics | none |
| Agent HTML dashboard | http://127.0.0.1:7800/dashboard | asks for `EH_API_TOKEN` |
| Prometheus UI / alerts | http://127.0.0.1:9090 , http://127.0.0.1:9090/alerts | none (loopback only) |
| Grafana | http://127.0.0.1:3000 | admin / `GRAFANA_ADMIN_PASSWORD` |

## Running it

- **Everything in Docker:** `docker compose --env-file ../.env --profile monitoring up -d`
  from `docker/` (see `docker/README.md`).
- **Agent native on Windows/macOS, monitoring in Docker:**
  `docker compose -f docker-compose.monitoring.yml --env-file ../.env up -d` from
  `docker/`. Prometheus scrapes `host.docker.internal:7800`, which Docker Desktop
  forwards to the host loopback, so the agent can stay on `127.0.0.1:7800`.
- **Agent native on Linux:** `host.docker.internal` maps to the docker bridge,
  which can't reach a loopback-only agent. Install Prometheus/Grafana from your
  distro, use `prometheus.native.yml` with the target changed to
  `127.0.0.1:7800` and `rule_files` pointing at the local `alerts.yml`.

Set `GRAFANA_ADMIN_PASSWORD` in the repo-root `.env` **before the first start**.
The fallback `change-me-now` is only a placeholder. Grafana stores the password
in its volume on first start, so changing the env var later has no effect. Use
`docker compose exec grafana grafana cli admin reset-admin-password <new>`
instead. Anonymous access and sign-up are disabled.

## Importing dashboards manually

Use this for a Grafana you already run, without the provisioning files.

1. Add a Prometheus datasource with **uid `prometheus`**: *Connections → Data
   sources → Add → Prometheus*. Set the URL. To set the uid, provision it with
   `grafana/provisioning/datasources/prometheus.yml` or create it through the
   API with `"uid": "prometheus"`.
2. *Dashboards → New → Import → Upload JSON file*, pick
   `grafana/dashboards/emailhelper-overview.json`, then repeat for
   `emailhelper-jobs.json`.
3. If your datasource has a different uid, search and replace
   `"uid": "prometheus"` in the JSON before importing.

The provisioned dashboards are read-only in the UI (`allowUiUpdates: false`).
To change one, edit the JSON in the repo. Grafana reloads it within 30 s.

## Dashboards

**EmailHelper - Overview:** totals for the selected range (emails, drafts,
drafts needing review, validation failures, failures, LLM errors, duplicates,
Telegram notifications), agent up/version and current queue depth. Time series
show emails/min by category and by account type, drafts by validation result,
validation failures, processing duration p50/p95 per workflow, queue depth,
agent failures by agent, failures by component, LLM requests by provider and
status, LLM latency p50/p95, Telegram notifications by event and duplicates.

**EmailHelper - Jobs:** jobs detected (from email) and discovered (research) by
source; applications started, completed, failed (by reason) and blocked (by
reason); success ratio (completed / started, plus a rolling 24 h line);
applications in progress; job-application duration p50/p95; recruiter matches;
Playwright failures by operation; research runs by status; and Telegram
requests by direction.

Stat panels use `increase(...[$__range])`, the total over the selected time
range. Time series use `rate(...[$__rate_interval]) * 60`, shown per minute.

## Alerts

There is no Alertmanager in the stack. Firing alerts appear at
http://127.0.0.1:9090/alerts, and Grafana can query `ALERTS{alertstate="firing"}`.
To route them to a notification channel, add Alertmanager and uncomment the
`alerting:` block in `prometheus.yml`.

| Alert | Severity | Condition | What to do |
|---|---|---|---|
| `AgentDown` | critical | `up{job="emailhelper"} == 0` for 2 m | Agent crashed or isn't reachable. Check `docker compose logs emailhelper` / `journalctl -u emailhelper` / `%LOCALAPPDATA%\EmailHelper\logs`. |
| `QueueBacklog` | warning | `queue_depth > 20` for 10 m | Processing can't keep up. The LLM is usually slow or unavailable: check Ollama and the model size. |
| `HighFailureRate` | warning | `rate(failures_total[10m]) > 0.1/s` per component for 5 m | One component is failing repeatedly. The `component` label says which. |
| `ValidationFailuresSpike` | warning | more than 5 validation failures in 30 m | Drafts are being rejected. Review `knowledge/`, `tone.md` and the model choice. |
| `EmailProcessingSlow` | warning | email p95 above 30 s for 15 m | Too slow against the 15 s draft target. Use a smaller model or a GPU. |
| `LLMErrors` | warning | more than 25 % of requests to a provider fail (and more than 0.005/s) for 5 m | API key, quota, network or Ollama problem. |
| `LLMFallbackInUse` | info | any `status="fallback"` request in 30 m | A remote provider failed and Ollama was used instead. |
| `PlaywrightFailures` | warning | more than 3 browser-worker failures per operation in 30 m | Browser worker down, logged out, site changed or rate-limited. |
| `TelegramFailures` | warning | more than 2 `failures_total{component=~"telegram.*"}` in 15 m | Bad bot token or chat id, or Telegram is unreachable. |
| `ApplicationsBlocked` | info | any `applications_blocked_total` increase in 1 h | An application needs you: answer the Telegram question or call `POST /api/v1/applications/{id}/answer`. |

`TelegramFailures` assumes the agent counts Telegram errors as
`failures_total{component="telegram"}`. Adjust the matcher if the component is
named differently.

## Example PromQL

```promql
# Emails processed per hour, by category
sum by (category) (increase(emails_processed_total[1h]))

# Share of drafts that needed human review over the last day
sum(increase(drafts_created_total{validation="REVIEW_REQUIRED"}[24h]))
  / sum(increase(drafts_created_total[24h]))

# p95 email processing time
histogram_quantile(0.95, sum by (le) (rate(processing_duration_seconds_bucket{workflow="email"}[15m])))

# LLM error ratio per provider
sum by (provider) (rate(llm_requests_total{status="error"}[10m]))
  / sum by (provider) (rate(llm_requests_total[10m]))

# p95 LLM latency per provider
histogram_quantile(0.95, sum by (le, provider) (rate(llm_request_duration_seconds_bucket[10m])))

# Application success ratio over 7 days
sum(increase(applications_completed_total[7d])) / sum(increase(applications_started_total[7d]))

# Top block reasons this week
topk(5, sum by (reason) (increase(applications_blocked_total[7d])))

# Running version
build_info
```

## Metrics reference

| Type | Name | Labels |
|---|---|---|
| counter | `emails_processed_total` | `account_type`, `category` |
| counter | `email_duplicates_total` | none |
| counter | `drafts_created_total` | `validation` (`APPROVED` \| `REVIEW_REQUIRED`) |
| counter | `validation_failures_total` | none |
| counter | `agent_failures_total` | `agent` |
| counter | `failures_total` | `component` |
| counter | `llm_requests_total` | `provider`, `status` (`ok` \| `error` \| `fallback`) |
| counter | `jobs_detected_total`, `jobs_discovered_total` | `source` |
| counter | `applications_started_total`, `applications_completed_total` | none |
| counter | `applications_failed_total`, `applications_blocked_total` | `reason` |
| counter | `recruiter_matches_total` | none |
| counter | `telegram_notifications_total` | `event` |
| counter | `telegram_requests_total` | `direction` (`out` \| `in`) |
| counter | `playwright_failures_total` | `operation` |
| counter | `research_runs_total` | `status` |
| histogram | `processing_duration_seconds` | `workflow` (`email` \| `job_application` \| `research`) |
| histogram | `llm_request_duration_seconds` | `provider` |
| gauge | `queue_depth`, `applications_in_progress` | none |
| gauge | `build_info` | `version` (value is always 1) |
