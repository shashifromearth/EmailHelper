# Architecture

## Components

```
┌──────────────────────┐   POST /api/v1/emails         ┌───────────────────────────────────────────────┐
│ Thunderbird          │ ─────────────────────────────►│ Rust orchestrator  (emailhelper, 127.0.0.1)   │
│  MailExtension       │   GET  /api/v1/drafts/pending  │                                               │
│  • onNewMailReceived │ ◄───────────── long poll ──────│  axum API ─► durable queue (SQLite) ─► agents │
│  • compose.saveMsg   │   POST /drafts/{id}/ack        │                                               │
└──────────────────────┘                                │  Email agent    Job agent    Research agent   │
                                                        │      │              │              │          │
        ┌───────────────────────────────────────────────┤      ▼              ▼              ▼          │
        │                                               │  LLM router (Ollama │ OpenAI │ Gemini │ Claude)│
        ▼                                               │  Knowledge base (SQLite FTS5 over knowledge/) │
┌──────────────────────┐  /v1/extract /v1/apply/*       │  Notifier + interactive Telegram bot          │
│ Browser worker       │ ◄──────────────────────────────│  Prometheus /metrics, /health, dashboard      │
│ Node + Playwright    │  /v1/linkedin/people           └───────────────────────────────────────────────┘
│ persistent Chromium  │                                         │                     │
└──────────────────────┘                                         ▼                     ▼
                                                     SearXNG · RSS · Greenhouse/   Telegram Bot API
                                                     Lever/Ashby public APIs
```

| Component | Tech | Location |
|---|---|---|
| Thunderbird extension | MailExtension (MV2, TB 115+) | `thunderbird-extension/` |
| Orchestrator | Rust, tokio, axum, rusqlite, reqwest | `rust-agent/` |
| Browser worker | Node.js 20+, Playwright Chromium | `browser-worker/` |
| Knowledge | JSON + Markdown files | `knowledge/` |
| Ops | Docker Compose, Prometheus, Grafana, systemd, Windows tasks | `docker/`, `monitoring/`, `deploy/` |

## Email workflow

```
New mail (Thunderbird) ─► POST /emails ─► idempotency key email:{account}:{message-id}:v{N}
   ─► processing_jobs row (queued) ─► dispatcher (semaphore = EH_MAX_CONCURRENCY)
   ─► Classifier  (rules + LLM JSON; injection scan)
        ├─ job opportunity? ─► job_email job (Job agent)   ├─ recruiting? ─► recruiter_discovery job
        └─ needs reply? no ─► done (alerts, no-reply senders, spam)
   ─► Context     (thread history, company_profile.md, tone.md, FTS5 snippets)
   ─► Drafter     (LLM; untrusted content fenced; signature enforced)
   ─► Validator   (rules: empty / placeholders / leaked prompt / invented emails, links, amounts,
                   phones / unbacked promises / injection / low confidence  +  LLM review)
                   → APPROVED | REVIEW_REQUIRED
   ─► drafts row (pending) ─► Notify waiters ─► extension long poll returns it
   ─► extension: compose.beginReply → saveMessage({mode:"draft"}) → close → ack
   ─► Telegram "New draft generated"
```

Emails are **never sent**. The backend has no SMTP code, and the extension
never calls `compose.sendMessage`. A unit test checks for this.

## Job application workflow

```
job_email ─► detect links (LinkedIn/Naukri/Glassdoor/Wellfound/Foundit/Greenhouse/Lever/Ashby/Workday/career sites)
          ─► browser worker /v1/extract (fallback: direct HTTP) ─► JSON-LD JobPosting / heuristics
          ─► matcher (skills 35 · role 20 · experience 15 · salary 15 · location 15) + hard rules
             (blacklist, salary floor, relocation, visa, no skill overlap)
          ─► decision: ≥80 apply · 60–79 review · <60 skip
          ─► application state machine (persisted in `applications`, resumable):
               inspect form ─► answer fields (profile map → answer memory → RAG+LLM → human)
               ─► fill + upload resume (+ submit if allowed) ─► submitted
               blocked (captcha/otp/mfa/login/unknown question/upload failure) ─► Telegram question
               human reply ─► answer stored (+ remembered) ─► re-queued ─► continues
```

Submit modes (`JOB_SUBMIT_MODE`):

| Mode | Score ≥ 80 | Score 60–79 |
|---|---|---|
| `review` (default) | fill, then ask "submit?" | ask first, then fill and submit |
| `auto` | fill and submit | ask first, then fill and submit |
| `dry_run` | fill only | fill only |

The agent never submits when the score is below threshold, a required
answer is missing, a CAPTCHA or OTP is unresolved, or the resume upload
failed.

## Research workflow

A daily scheduler, set by `RESEARCH_DAILY_TIME`, sleeps until the configured
time. It then queues a `research` job, which makes the run durable and
retryable. The job:

1. Queries SearXNG for each search term, restricted to ATS sites, and
   stores the results in `research_findings`.
2. Reads the configured RSS/Atom feeds, the Greenhouse, Lever and Ashby public
   board APIs, and any career pages (through the browser worker).
3. Filters by search terms, scores every job against the resume, de-duplicates
   by fingerprint, stores the results in `jobs` and sends a Telegram summary
   of the top N.
4. Starts applications for non-skip jobs when the job agent is enabled.

Recruiter discovery runs for every recruiting or job email when enabled. It:

- adds the sender as a contact, with their title parsed from the signature;
- researches the company via SearXNG;
- optionally runs a LinkedIn people search through the user's own session,
  capped per day;
- ranks the contacts (technical recruiter > TA > hiring manager > HR);
- writes outreach drafts: Thunderbird `kind:"new"` drafts for contacts with
  an email address, and the text in Telegram for LinkedIn-only contacts.

## Reliability

- **Durable queue.** `processing_jobs` rows are the source of truth. On
  startup, `running` jobs return to `queued` and are re-dispatched, and
  applications left mid-flight are re-queued.
- **Idempotency.** Each email job has a unique key of
  `(account_id, message_id, workflow_version)`, so duplicates are ignored
  across restarts. Bumping `EH_WORKFLOW_VERSION` re-processes mail.
- **Retries.** Failed jobs retry with exponential backoff (2ⁿ s) up to
  `max_attempts`, then move to `dead`, write an audit record and send a
  Telegram alert.
- **No polling loops.** The queue is channel-driven, the draft API uses
  long polling backed by `tokio::sync::Notify`, and Telegram uses
  `getUpdates` long polling.
- **Graceful degradation.** An LLM outage falls back to rule-based
  classification. A failing remote provider falls back to Ollama. A Telegram
  outage is logged and counted but never blocks work.

## Data model (SQLite)

| Table | Purpose |
|---|---|
| `emails` | Received messages, classification and status |
| `processing_jobs` | Durable work queue with idempotency keys |
| `drafts` | Generated drafts and their delivery/ack state |
| `audit_logs` | Append-only audit trail |
| `settings` (`config` view) | Runtime key/value state (Telegram offset, quotas) |
| `knowledge_registry`, `knowledge_fts` | FTS5 index of the knowledge directory |
| `jobs` | Detected and discovered jobs with score, decision and reasons |
| `applications`, `application_status` | Application state machine and history |
| `human_questions`, `answer_memory` | Human-in-the-loop questions; reusable answers |
| `recruiters`, `interviews`, `research_findings` | Research output |
