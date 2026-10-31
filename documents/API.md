# Interface Contracts

This document is the single source of truth for every interface between the
components of EmailHelper. Each component must follow it exactly.

```
Thunderbird ──(MailExtension)──► Rust orchestrator (127.0.0.1:7800) ──► Browser worker (127.0.0.1:7801, Node + Playwright)
                                        │
                                        ├─► Ollama / OpenAI / Gemini / Anthropic
                                        ├─► SQLite (+ FTS5 knowledge index)
                                        ├─► SearXNG / RSS / Greenhouse / Lever / Ashby public APIs
                                        └─► Telegram Bot API (notifications + interactive replies)
```

All services bind to `127.0.0.1` only. Nothing is ever exposed externally.
The one exception is Docker Compose: inside its private network the agent and
worker bind `0.0.0.0` (`EH_ALLOW_NON_LOOPBACK=true`, `BROWSER_ALLOW_REMOTE=true`),
and compose publishes their ports on `127.0.0.1` only.

---

## 1. Rust orchestrator REST API (`http://127.0.0.1:7800`)

### Authentication

Every `/api/*` route requires the header:

```
X-EmailHelper-Token: <EH_API_TOKEN>
```

Requests without a valid token get `401`. CORS is not enabled, so web pages
cannot call the API. `/health`, `/readiness`, `/liveness`, `/metrics` and
`/dashboard` don't need the token. The dashboard's JSON calls go through
`/api/v1/dashboard`, and the page asks the user for the token.

Errors always use this shape:

```json
{ "error": "validation_failed", "message": "subject exceeds 998 characters" }
```

### `POST /api/v1/emails`: submit a newly received email

Request body (max `EH_MAX_BODY_BYTES`, default 256 KiB; `body_text` is
truncated server side at 64 KiB):

```json
{
  "account_id": "account3",
  "account_name": "Work Gmail",
  "account_email": "me@example.com",
  "account_type": "imap",
  "message_id": "<CAF=abc@mail.gmail.com>",
  "tb_message_id": 1234,
  "folder": "INBOX",
  "in_reply_to": "<prev@example.com>",
  "references": ["<root@example.com>", "<prev@example.com>"],
  "from": "Jane Doe <jane@acme.com>",
  "to": ["me@example.com"],
  "cc": [],
  "subject": "Question about pricing",
  "date": "2026-10-18T09:30:00Z",
  "body_text": "Hi, ...",
  "body_html": null,
  "attachments": [{ "name": "spec.pdf", "content_type": "application/pdf", "size": 48213 }]
}
```

Required: `account_id`, `message_id`, `tb_message_id`, `from`, `subject` (may
be empty string), `body_text` (may be empty string). Everything else is optional.

`account_type` is one of `gmail`, `outlook`, `zoho`, `imap`, `pop3`, `other`.
The MailExtension API doesn't expose server hostnames, so the extension infers
the type from the identity email domains, then the account name, then the
protocol (`ews` → `outlook`). It can be overridden per account in the options
page, for example for Google Workspace or Microsoft 365 on a custom domain.
Unknown values are stored as `other`.

Response `202 Accepted`:

```json
{ "email_id": "0f6c…", "job_id": "9a1e…", "status": "queued" }
```

or `200 OK` when this `(account_id, message_id, workflow_version)` has already
been processed:

```json
{ "email_id": "0f6c…", "job_id": null, "status": "duplicate" }
```

### `GET /api/v1/drafts/pending?wait=25`: long poll for drafts to save

Returns immediately if drafts are waiting. Otherwise it holds the request for
up to `wait` seconds (max 55) and returns as soon as a draft becomes ready, or
returns an empty list on timeout. The extension calls it in a loop. This is a
long poll, not a busy poll.

```json
{
  "drafts": [
    {
      "draft_id": "d7c2…",
      "kind": "reply",
      "account_id": "account3",
      "tb_message_id": 1234,
      "message_id": "<CAF=abc@mail.gmail.com>",
      "to": ["jane@acme.com"],
      "subject": "Re: Question about pricing",
      "body": "Hi Jane,\n\n…",
      "validation": "APPROVED",
      "notes": ["Written as Shashi Bhushan Kumar from shashilpu@gmail.com (job_seeking)"],
      "from_address": "shashilpu@gmail.com",
      "attachments": [
        {
          "name": "Shashi_Resume_GOLANG_PYTHON_AWS_GenAI_Architect.docx",
          "content_type": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
          "data_base64": "UEsDBBQ…"
        }
      ]
    }
  ]
}
```

- `kind = "reply"`: the extension calls `messenger.compose.beginReply(tb_message_id, "replyToSender", { plainTextBody: body })`.
  Thunderbird then sets `In-Reply-To` / `References` and keeps the thread intact.
- `kind = "new"` (for example recruiter outreach): the extension calls
  `messenger.compose.beginNew({ to, subject, plainTextBody: body })` using the
  default identity of `account_id`. `tb_message_id` and `message_id` are null.
- `from_address`: the mailbox that received the email (from `knowledge/people.json`). The extension sends the
  reply from the identity with this address in that account, and never uses another account's copy of the
  message (BCC copies share a Message-ID). `null` in legacy single-person mode.
- `attachments`: files to attach before saving, usually the resume chosen
  for the job when the email asks for one. The extension adds each file with
  `messenger.compose.addAttachment`. Only files inside the knowledge folder are
  sent, up to 10 MB each. If a file can't be attached, the draft is still
  saved and the ack carries a warning.
- `validation = "REVIEW_REQUIRED"`: the extension still saves the draft but
  puts this line at the top of the body:
  `[EmailHelper: REVIEW REQUIRED – <notes joined by "; ">]`.

The draft is then saved with `messenger.compose.saveMessage(tabId, { mode: "draft" })`
and the compose window is closed. **The extension never calls `sendMessage`.**

A draft stays in the pending list until it is acknowledged. After 10 minutes
without an ack it is handed out again.

### `POST /api/v1/drafts/{draft_id}/ack`

```json
{ "status": "saved", "tb_draft_message_id": 5678, "error": null }
```

`status` is `saved` or `failed`. On `failed`, include `error`; the orchestrator
retries up to 3 times and then marks the draft `failed`. On `saved`, `error`
may carry a warning (for example "could not attach resume.docx"); the
orchestrator logs it and asks the person on Telegram to attach the file.

Response: `204 No Content`.

### `GET /api/v1/status`: status summary for the extension popup

```json
{
  "version": "1.0.0",
  "llm_provider": "ollama",
  "llm_model": "qwen2.5:7b",
  "queue_depth": 0,
  "emails_processed": 42,
  "drafts_created": 30,
  "drafts_pending": 1,
  "review_required": 3,
  "failures": 0,
  "telegram_enabled": true,
  "job_agent_enabled": false,
  "research_enabled": false
}
```

### Other API routes (all need the token)

| Method | Path | Purpose |
|---|---|---|
| GET  | `/api/v1/dashboard` | Aggregated JSON for the dashboard page |
| GET  | `/api/v1/jobs?limit=50` | Discovered and detected jobs, ranked |
| POST | `/api/v1/jobs` | `{ "url": "...", "apply": false, "person": "shashi" }`: open, extract, score and store a job by URL; `apply: true` also starts an application (needs `JOB_AGENT_ENABLED`) |
| POST | `/api/v1/apply/answers` | Chrome extension: `{ "person": "shashi", "job": { "title": "...", "company": "...", "skills": [] }, "fields": [Field] }` → `{ "person", "answers": [{ "kind": "value" \| "resume" \| "skip" \| "ask", "id", "value"?, "reason"? }] }`. Same rules as the agent's own applications; nothing is stored or submitted |
| POST | `/api/v1/apply/escalations` | Chrome extension, stuck: `{ "link_id", "url", "title", "company", "platform", "person", "kind", "reason" }` → `{ "id", "telegram": bool }`. Messages the person on Telegram; an open escalation for the same URL is returned instead of messaging again |
| GET  | `/api/v1/apply/escalations/{id}` | `{ "id", "status": "open" \| "skip" \| "done", "answer", "created_at", "updated_at" }`. A Telegram reply to the message sets it: "skip"/"/skip" → skip, "done"/"fixed"/"ok" → done, other text → done with `answer` |
| POST | `/api/v1/apply/escalations/{id}/resolve` | `{ "status": "done" \| "skip" }`: the page got fixed or the extension gave up. The first resolution wins; returns the escalation as above |
| POST | `/api/v1/apply/events` | `{ "events": [{ "kind", "link_id"?, "url", "platform"?, "person"?, "field"?, "reason", "detail"? }] }` (at most 200; long text is cut) → `{ "stored": n }`. Every applier failure and outcome, for the feedback loop |
| GET  | `/api/v1/apply/events/summary?days=7` | `{ "days", "by_kind": {kind: n}, "top": [{ "platform", "kind", "field", "reason", "count", "last_url", "last_at" }] }`: the 50 most frequent problems (`applied` left out of `top`) |
| GET  | `/api/v1/applications?status=blocked` | Applications |
| POST | `/api/v1/applications/{id}/answer` | `{ "question_id": "...", "answer": "..." }`: same effect as a Telegram reply |
| POST | `/api/v1/questions/{id}/answer` | `{ "answer": "..." }`: answer any open question, including person facts such as notice period |
| POST | `/api/v1/applications/{id}/resume` | Retry a blocked or failed application |
| GET  | `/api/v1/recruiters?company=Acme` | Recruiter contacts |
| POST | `/api/v1/research/run` | Run job discovery now |
| GET  | `/api/v1/audit?limit=100` | Audit log |

### Unauthenticated operational routes

| Path | Meaning |
|---|---|
| `/health` | `{"status":"ok","version":…,"checks":{"database":"ok","llm":"ok|degraded","browser_worker":"ok|disabled|down","telegram":"ok|disabled","knowledge_service":"ok|down|off","job_links":"ok|down|off"}}`; `status` is `degraded` when the database, the knowledge service or job-links is down |
| `/liveness` | `200` while the process is running |
| `/readiness` | `200` when the DB is open and migrations have run, otherwise `503` |
| `/metrics` | Prometheus text exposition |
| `/dashboard` | Lightweight HTML dashboard (data loaded via `/api/v1/dashboard`) |

---

## 2. Browser worker API (`http://127.0.0.1:7801`, Node.js + Playwright)

Every route except `/health` requires `X-Worker-Token: <BROWSER_WORKER_TOKEN>`.

The worker uses `chromium.launchPersistentContext(BROWSER_PROFILE_DIR, { headless })`,
so cookies and logins survive restarts. One page is used per `session_key`
(the application id), and pages are closed after `fill` finishes, either
submitted or blocked.

Worker environment:

| Variable | Default |
|---|---|
| `BROWSER_WORKER_HOST` | `127.0.0.1` |
| `BROWSER_WORKER_PORT` | `7801` |
| `BROWSER_WORKER_TOKEN` | (required) |
| `BROWSER_PROFILE_DIR` | `./data/browser-profile` |
| `BROWSER_HEADLESS` | `true` |
| `BROWSER_SCREENSHOT_DIR` | `./data/screenshots` |
| `BROWSER_NAV_TIMEOUT_MS` | `30000` |
| `BROWSER_ALLOWED_UPLOAD_DIRS` | `./knowledge;./data` (files outside these are refused) |

### Common types

```ts
type Blocker = {
  kind: "captcha" | "otp" | "mfa" | "login_required" | "access_denied" |
        "upload_failure" | "unexpected_form" | "rate_limited" | "site_error" |
        "network_error";
  detail: string;
};

type Field = {
  id: string;            // stable: element id || name || `field_<n>` in DOM order
  label: string;         // visible label text, aria-label, placeholder, or name
  type: "text" | "email" | "tel" | "url" | "number" | "date" | "textarea" |
        "select" | "radio" | "checkbox" | "file";
  required: boolean;
  options: string[];     // select/radio option labels (empty otherwise)
  accept?: string;       // for file inputs
};
```

### `GET /health`

`{ "status": "ok", "browser": "chromium", "headless": true, "version": "1.0.0" }`

### `POST /v1/extract`

Request: `{ "url": "https://boards.greenhouse.io/acme/jobs/123" }`

Response:

```json
{
  "final_url": "https://…",
  "title": "Staff AI Engineer - Acme",
  "text": "visible text, whitespace collapsed, max 40000 chars",
  "json_ld": [ { "@type": "JobPosting", "...": "..." } ],
  "links": [ { "text": "Apply", "href": "https://…" } ],
  "blocker": null
}
```

### `POST /v1/apply/inspect`

Request: `{ "session_key": "app_123", "url": "https://…" }`

The worker opens the URL and, if the page has an "Apply" button or link
instead of a form, clicks it. It then finds the application form and lists
its fields.

```json
{ "final_url": "https://…", "ats": "greenhouse", "fields": [Field], "blocker": null }
```

`ats` is one of `greenhouse`, `lever`, `ashby`, `workday`, `linkedin`, `generic`,
detected from the hostname and DOM markers.

### `POST /v1/apply/fill`

```json
{
  "session_key": "app_123",
  "url": "https://…",
  "answers": { "first_name": "Asha", "relocate": "No", "skills": ["Rust", "Python"], "consent": true },
  "files": { "resume": "C:/…/knowledge/resume.pdf" },
  "submit": false
}
```

The worker:

1. Reuses or opens the page and fills every field it has an answer for.
2. Sets file inputs, then checks that the file name shows up in the input or
   page. If the upload fails, it returns blocker `upload_failure`.
3. Checks for blockers (captcha iframes for reCAPTCHA, hCaptcha or Turnstile;
   OTP or verification-code inputs; login walls; HTTP 403/429).
4. If `submit` is true **and** there are no missing required fields **and** no
   blocker, it clicks the submit button and waits for a confirmation, either a
   URL change or text such as "thank you", "application received" or
   "submitted".

```json
{
  "status": "submitted",
  "final_url": "https://…/confirmation",
  "missing_required": [],
  "blocker": null,
  "confirmation_text": "Thank you for applying!",
  "screenshot_path": "data/screenshots/app_123-1729240000.png"
}
```

`status` is one of `filled`, `submitted`, `blocked` or `error`. A screenshot is
always taken after fill or submit.

Implementation notes (clients must tolerate extra fields):

- If submit was requested but required fields are still empty, `status` is
  `filled`, `missing_required` lists their ids and `blocker` is `null`.
- `error` covers exceptions, no confirmation within 15 s, and multi-step
  forms or validation errors that leave the form on the page.
- Confirmation text must be *new* after the click. A URL change with the form
  gone also counts as `submitted`.
- If the page has a CAPTCHA, the worker still fills the form (so
  `missing_required` and the screenshot are useful) but never submits.
- Fill responses also carry `filled: string[]`, `skipped: [...]` and
  `error: string|null`.
- Field ids follow these rules:
  - radio and checkbox groups use their `name`;
  - duplicate ids get `_2`, `_3` and so on;
  - answers are matched by id, then by id ignoring case, then by
    normalised label;
  - `true` and `false` map to "Yes" and "No".

### `POST /v1/linkedin/people`

Uses the user's own logged-in LinkedIn session from the persistent profile. If
the session isn't logged in, it returns `blocker.kind = "login_required"`.
Rate limit: at most one search every 20 seconds, enforced by the worker. Results
are only read from the search results page; profiles are never opened in bulk.

The worker runs one search per role, at most 3 roles per request, keeping
navigations at least 20 s apart. A 3-role request can therefore take about
60 s, so set client timeouts to roughly 20 s × roles + 30 s. The agent's
default `BROWSER_WORKER_TIMEOUT_SECS=180` covers this.

Request: `{ "company": "Acme", "roles": ["recruiter", "talent acquisition", "hiring manager"], "limit": 10 }`

Response: `{ "people": [ { "name": "…", "headline": "Senior Technical Recruiter at Acme", "profile_url": "https://www.linkedin.com/in/…" } ], "blocker": null }`

---

## 3. Telegram interaction

The orchestrator sends messages with the Bot API (`sendMessage`, `parse_mode`
unset, plain text) and receives replies with `getUpdates` long polling
(`timeout=50`). Only messages from `TELEGRAM_CHAT_ID` are accepted.

A human-escalation message looks like this:

```
⚠️ Application Blocked
Company: Acme
Role: Staff AI Engineer
URL: https://…
Reason: unknown_question
Question: What is your expected salary?
Ref: Q-7F3A

Reply to this message with your answer.
```

Replies are routed to the open question in one of two ways:

1. `reply_to_message.message_id` matches the stored `telegram_message_id`, or
2. the text starts with `Q-XXXX <answer>`.

Special answers: `skip` abandons the application and `submit` approves a
pending review.

Bot commands: `/status`, `/jobs` (top 10 open jobs), `/pending` (open
questions), `/help`.

---

## 4. Knowledge directory (`EH_KNOWLEDGE_DIR`, default `./knowledge`)

| File | Purpose |
|---|---|
| `profile.json` | Identity and contact details, links, work authorization, notice period |
| `resume.json` | Headline, summary, total experience, resume file path |
| `skills.json` | `{ "core": [...], "secondary": [...] }` |
| `salary_preferences.json` | Currency, minimum and expected salary |
| `preferences.json` | Remote preference, locations, target roles, blacklist |
| `employment_history.json` | Array of roles |
| `education.json` | Array of degrees |
| `projects.json` | Array of projects |
| `certifications.json` | Array of certifications |
| `company_profile.md` | Your business or company profile, used for email replies |
| `tone.md` | Writing-style guidance for drafts |
| `approved_responses/*.md` | Canned or approved answers (retrieved by FTS) |
| `docs/**/*.md`, `*.txt` | Any other reference material (retrieved by FTS) |

Everything is indexed into SQLite FTS5 at startup and whenever a file changes
(by checking its mtime during the reindex that runs before each retrieval
batch).
