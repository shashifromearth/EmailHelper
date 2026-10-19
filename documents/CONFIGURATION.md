# Configuration Reference

All settings are environment variables. They are loaded from the process
environment and from `.env` in the working directory (override the file path
with `EH_ENV_FILE`). Start from `.env.example`. Every setting has a safe
default: local Ollama, Telegram off, job and research agents off, and
submit mode `review`.

Invalid values stop startup with a clear error and exit code 2. Missing
cloud API keys never cause a failure: the agent falls back to local Ollama.

## Core

| Variable | Default | Description |
|---|---|---|
| `EH_BIND_ADDR` | `127.0.0.1:7800` | API listen address. Must be loopback unless `EH_ALLOW_NON_LOOPBACK=true`, which is only for an isolated container network. |
| `EH_ALLOW_NON_LOOPBACK` | `false` | Allows binding to a non-loopback address (Docker). |
| `EH_API_TOKEN` | *(generated)* | Shared secret for the `X-EmailHelper-Token` header. If empty, a random token is written to `$EH_DATA_DIR/api-token.txt`. |
| `EH_DATA_DIR` | `./data` | Directory for the database, the generated token and worker state. |
| `EH_DATABASE_PATH` (alias `EH_DB_PATH`) | `$EH_DATA_DIR/emailhelper.db` | SQLite file. |
| `EH_KNOWLEDGE_DIR` | `./knowledge` | Profile JSON and Markdown knowledge files. |
| `EH_LOG_FORMAT` | `json` | `json` or `pretty`. |
| `RUST_LOG` | `info` | Log filter, e.g. `info,emailhelper=debug`. |
| `EH_MAX_CONCURRENCY` | `10` | Maximum number of workflows running at once. |
| `EH_MAX_BODY_BYTES` | `262144` | HTTP request body limit; larger requests get `413`. |
| `EH_MAX_EMAIL_TEXT_BYTES` | `65536` | Email bodies are truncated to this length. |
| `EH_MAX_ATTACHMENT_BYTES` | `26214400` | Attachments above this size are flagged in the audit log. Attachment content is never ingested. |
| `EH_WORKFLOW_VERSION` | `1` | Part of the idempotency key; bump it to re-process mail. |
| `EH_MAX_RETRIES` | `3` | Attempts per email job before it is marked dead. |

## LLM

| Variable | Default | Description |
|---|---|---|
| `MODEL_MODE` | `LOCAL` | `LOCAL`, `REMOTE_OPENAI`, `REMOTE_GEMINI`, `REMOTE_ANTHROPIC` or `MOCK` (deterministic, offline). |
| `OLLAMA_BASE_URL` | `http://127.0.0.1:11434` | Ollama server address. |
| `OLLAMA_MODEL` | `qwen2.5:7b` | Also `qwen2.5:3b` (low RAM), `llama3` or `mistral`. |
| `OPENAI_API_KEY` / `OPENAI_MODEL` / `OPENAI_BASE_URL` | — / `gpt-4o-mini` / `https://api.openai.com/v1` | Works with any OpenAI-compatible endpoint. |
| `GEMINI_API_KEY` / `GEMINI_MODEL` | — / `gemini-2.0-flash` | The key is sent in a header, never in the URL. |
| `ANTHROPIC_API_KEY` / `ANTHROPIC_MODEL` | — / `claude-haiku-5-5` | Messages API. |
| `LLM_TIMEOUT_SECS` | `90` | Timeout per request. |
| `LLM_FALLBACK_TO_LOCAL` | `true` | Retries a failed remote request on Ollama. |

## Telegram

| Variable | Default | Description |
|---|---|---|
| `TELEGRAM_ENABLED` | `false` | Turns on notifications and the interactive bot. |
| `TELEGRAM_BOT_TOKEN` | — | From @BotFather. |
| `TELEGRAM_CHAT_ID` | — | Your chat id. Messages from any other chat are ignored. |
| `TELEGRAM_API_BASE` | `https://api.telegram.org` | Override for testing. |
| `TELEGRAM_POLL_TIMEOUT_SECS` | `50` | `getUpdates` long-poll timeout. |

If Telegram is enabled but the token or chat id is missing, Telegram is
disabled with a warning.

## Job application agent

| Variable | Default | Description |
|---|---|---|
| `JOB_AGENT_ENABLED` | `false` | Processes job emails and drives applications. |
| `JOB_SUBMIT_MODE` | `review` | `review` asks on Telegram before submitting, `auto` submits when score ≥ threshold, `dry_run` never submits. |
| `JOB_AUTO_APPLY_THRESHOLD` | `80` | Score at or above which the decision is "apply". |
| `JOB_REVIEW_THRESHOLD` | `60` | Scores in `[review, auto)` need your approval; below this, the job is skipped. |
| `JOB_RESUME_PATH` | `$EH_KNOWLEDGE_DIR/resume.pdf` | Resume file to upload. `resume.json.resume_file` takes precedence when it exists. |
| `JOB_MAX_ATTEMPTS` | `3` | Retries per application step for transient errors. |
| `BROWSER_WORKER_URL` | `http://127.0.0.1:7801` | Browser worker address. |
| `BROWSER_WORKER_TOKEN` | — | Shared secret (`X-Worker-Token`); must match the worker's value. |
| `BROWSER_WORKER_TIMEOUT_SECS` | `180` | Timeout per worker call. |

Browser worker settings (read by `browser-worker/`): `BROWSER_WORKER_HOST`,
`BROWSER_WORKER_PORT`, `BROWSER_PROFILE_DIR`, `BROWSER_HEADLESS`,
`BROWSER_SCREENSHOT_DIR`, `BROWSER_NAV_TIMEOUT_MS`,
`BROWSER_ALLOWED_UPLOAD_DIRS`. See `browser-worker/README.md`.

## Research agent

| Variable | Default | Description |
|---|---|---|
| `RESEARCH_ENABLED` | `false` | Turns on the daily discovery scheduler. |
| `RESEARCH_DAILY_TIME` | `07:30` | Local time (HH:MM). |
| `SEARXNG_URL` | — | SearXNG base URL. The JSON output format must be enabled. |
| `RESEARCH_SEARCH_TERMS` | the 7 terms from the brief | Separated by `;`. Generic words (remote, jobs, India) are ignored when matching titles. |
| `RESEARCH_RSS_FEEDS` | — | `;`-separated feed URLs. |
| `RESEARCH_GREENHOUSE_BOARDS` / `RESEARCH_LEVER_BOARDS` / `RESEARCH_ASHBY_BOARDS` | — | Company board slugs (public APIs). |
| `RESEARCH_CAREER_PAGES` | — | Career page URLs, opened through the browser worker. |
| `RESEARCH_MAX_RESULTS_PER_QUERY` | `20` | Maximum SearXNG results per query. |
| `RESEARCH_SUMMARY_TOP_N` | `10` | Number of jobs in the Telegram summary. |
| `RECRUITER_DISCOVERY_ENABLED` | `false` | Finds recruiters for recruiting or job emails. |
| `RECRUITER_LINKEDIN_ENABLED` | `false` | LinkedIn people search through your logged-in worker profile. |
| `RECRUITER_DAILY_LIMIT` | `20` | Maximum LinkedIn searches per day. |

## Knowledge files

See [API.md §4](API.md#4-knowledge-directory-eh_knowledge_dir-default-knowledge).
Notable switches in `preferences.json`:

- `auto_accept_application_consent` (default `false`): when `false`, required
  privacy or consent checkboxes are escalated to you instead of being ticked
  automatically.
- `blacklisted_companies`, `remote` (`remote_only`, `hybrid`, `onsite` or
  `any`), `preferred_locations`, `target_roles` and
  `visa_sponsorship_required` all feed the matcher.

## Example: fully local with Telegram and job agent in review mode

```dotenv
EH_API_TOKEN=3f8a…(32+ random chars)
MODEL_MODE=LOCAL
OLLAMA_MODEL=qwen2.5:7b
TELEGRAM_ENABLED=true
TELEGRAM_BOT_TOKEN=123456:ABC…
TELEGRAM_CHAT_ID=987654321
JOB_AGENT_ENABLED=true
JOB_SUBMIT_MODE=review
BROWSER_WORKER_TOKEN=9c1e…(32+ random chars)
RESEARCH_ENABLED=true
SEARXNG_URL=http://127.0.0.1:8888
RESEARCH_GREENHOUSE_BOARDS=anthropic;databricks
```
