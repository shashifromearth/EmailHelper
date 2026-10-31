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
| `EH_EXCLUDED_SENDERS` | — | Senders that never enter the system, as addresses (`alerts@paytm.com`) and/or domains (`hdfcbank.com` or `@hdfcbank.com`, subdomains included), separated by `;` or `,`. The Thunderbird extension skips them before reading the message, and the agent drops them before storing or classifying anything. Only a counter (`emails_excluded_total`) records them. |
| `EVAL_LIVE_SAMPLE` | `0.25` | Share of real drafts scored in the background by the LLM judge (groundedness, relevance, faithfulness). The private values are masked first. The 7-day averages appear as `draft_quality_7d` in `GET /api/v1/status`. `0` turns it off. |
| `EH_MAX_EMAIL_AGE_HOURS` | `72` | Mail older than this (for example a bulk pass over old unread messages) is recorded and checked for job signals by rules, but gets no LLM call and no draft. `0` turns the limit off. |

## LLM

| Variable | Default | Description |
|---|---|---|
| `MODEL_MODE` | `LOCAL` | `LOCAL`, `REMOTE_GROQ`, `REMOTE_GEMINI`, `REMOTE_OPENAI`, `REMOTE_ANTHROPIC` or `MOCK` (deterministic, offline). Cloud modes send email text to that provider. Values from `private.json` never leave the machine: they are inserted after the draft is generated. |
| `OLLAMA_BASE_URL` | `http://127.0.0.1:11434` | Ollama server address. |
| `OLLAMA_MODEL` | `qwen2.5:7b` | Also `qwen2.5:3b` (low RAM), `llama3` or `mistral`. |
| `OLLAMA_NUM_CTX` | `8192` | Context window requested from Ollama. Ollama's own default of 4096 tokens can silently cut off long draft prompts. |
| `OPENAI_API_KEY` / `OPENAI_MODEL` / `OPENAI_BASE_URL` | — / `gpt-4o-mini` / `https://api.openai.com/v1` | Works with any OpenAI-compatible endpoint. |
| `GEMINI_API_KEY` / `GEMINI_MODEL` | — / `gemini-flash-latest` | The key is sent in a header, never in the URL. |
| `ANTHROPIC_API_KEY` / `ANTHROPIC_MODEL` | — / `claude-haiku-5-5` | Messages API. |
| `GROQ_API_KEY` / `GROQ_MODEL` / `GROQ_BASE_URL` | — / `openai/gpt-oss-20b` / `https://api.groq.com/openai/v1` | Groq's OpenAI-compatible API. Reasoning models (gpt-oss, o-series, qwen3) are asked for low reasoning effort and given extra token headroom. |
| `LLM_TIMEOUT_SECS` | `90` | Timeout per request. |
| `LLM_FALLBACK_TO_LOCAL` | `true` | Retries a failed remote request on Ollama. |
| `LLM_FALLBACK_CLOUD` | `true` | In a cloud mode, tries the other cloud providers that have a key first. The order is Groq, Gemini, OpenAI, Anthropic, then Ollama. `LOCAL` mode never uses the cloud. |

Run `emailhelper llm-test` to send a one-line test prompt to every provider
that has a key (and to Ollama), and see which ones work and how fast they
answer.
| `LLM_REVIEW` | `auto` | Adds an LLM second-opinion review of each draft. `auto` turns it on for cloud models and off for local Ollama, where it costs a whole extra call. Rule-based validation always runs. |
| `LLM_MAX_PARALLEL` | `0` (auto) | Maximum concurrent model calls. Auto is 1 for local Ollama, since a CPU model only gets slower with parallel requests, and 4 for cloud models. |
| `OLLAMA_KEEP_ALIVE` | `24h` | How long Ollama keeps the model loaded, which avoids a reload of about 12 s. |

## Telegram

| Variable | Default | Description |
|---|---|---|
| `TELEGRAM_ENABLED` | `false` | Turns on notifications and the interactive bot. |
| `TELEGRAM_BOT_TOKEN` | — | From @BotFather. |
| `TELEGRAM_CHAT_ID` | — | Default chat for all people. Person-specific chats can be added in `knowledge/people.json`. Messages from any other chat are ignored. Run `emailhelper telegram-chats` to list chat IDs. |
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
| `JOB_DIGEST_TIME` | `19:00` | Local time of the daily Telegram digest of good matches you must apply to yourself (platform-only jobs). `emailhelper job-digest-now` sends it immediately. |
| `JOB_DIGEST_MAX` | `15` | Maximum jobs per digest; anything left over is sent the next day, and entries expire after 7 days. |
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
| `RESEARCH_CHECK_INTERVAL_HOURS` | `4` | Discovery runs once per day. At startup and then every this many hours the agent checks whether today's run has happened (`research.last_run_date` in the `settings` table, shown as `research_ran_today` in `GET /api/v1/status`) and runs it immediately if not. |
| `RESEARCH_SCHEDULE_ENABLED` | `true` | Turn the daily check off (runs can still be started with `POST /api/v1/research/run`). |
| `RESEARCH_DAILY_TIME` | `07:30` | No longer used by discovery (kept for compatibility). |
| `SEARXNG_URL` | — | SearXNG base URL. The JSON output format must be enabled. |
| `RESEARCH_SEARCH_TERMS` | the 7 terms from the brief | Separated by `;`. Generic words (remote, jobs, India) are ignored when matching titles. |
| `RESEARCH_RSS_FEEDS` | — | `;`-separated feed URLs. |
| `RESEARCH_GREENHOUSE_BOARDS` / `RESEARCH_LEVER_BOARDS` / `RESEARCH_ASHBY_BOARDS` | — | Company board slugs (public APIs). |
| `RESEARCH_CAREER_PAGES` | — | Career page URLs, opened through the browser worker. |
| `RESEARCH_MAX_RESULTS_PER_QUERY` | `20` | Maximum SearXNG results per query. |
| `RESEARCH_SUMMARY_TOP_N` | `10` | Number of jobs in the Telegram summary. |
| `RESEARCH_REMOTE_BOARDS` | `remotive;remoteok;himalayas;jobicy;weworkremotely` | Remote-job boards read once a day through their public APIs/feeds (`none` turns them off). `RESEARCH_<BOARD>_URL` overrides an endpoint. |
| `RESEARCH_MAX_AGE_HOURS` | `25` | Only postings published within this many hours are kept; postings without a date are dropped. `0` keeps everything. |
| `RESEARCH_APPLICANT_COUNTRY` | first authorized country in the research person's profile | Jobs must be in this country or open to applicants from it ("Worldwide", "APAC", "India"). Jobs limited to other regions, or whose description says "US only", "must be located in the US", "EU only" and similar, are dropped. |
| `RESEARCH_REQUIRE_REMOTE` | `true` | A kept job must be remote (or a distributed team). |
| `RESEARCH_WORK_TYPES` | `contract;freelance;consultant` | A kept job must be one of these role types, judged from the board's employment type, the title/tags and clear phrases such as "contract role" or "independent consultant" (`none` = full-time roles too). |
| `RESEARCH_TECH_STACK` | `python;c#;.net;java;genai;agentic ai` | A kept job must mention one of these (synonyms included: Django/FastAPI, ASP.NET, Spring, LLM/RAG/LangChain, LangGraph/multi-agent). `none` falls back to `RESEARCH_SEARCH_TERMS` matching titles. |
| `RESEARCH_AUTO_APPLY` | `false` | Start applications for discovered jobs that aren't skipped (needs `JOB_AGENT_ENABLED`). Jobs from your own mail are handled by the job agent regardless. |
| `RECRUITER_DISCOVERY_ENABLED` | `false` | Finds recruiters for recruiting or job emails. |
| `RECRUITER_LINKEDIN_ENABLED` | `false` | LinkedIn people search through your logged-in worker profile. |
| `RECRUITER_DAILY_LIMIT` | `20` | Maximum LinkedIn searches per day. |

## Knowledge files

People, mailboxes, purposes, private facts and resumes are described in
[KNOWLEDGE.md](KNOWLEDGE.md).

## Knowledge service (RAG)

| Variable | Default | Description |
|---|---|---|
| `KNOWLEDGE_SERVICE_ADDR` | `127.0.0.1:7820` | Where the knowledge service listens (loopback only unless `KNOWLEDGE_SERVICE_ALLOW_REMOTE=true`). |
| `KNOWLEDGE_SERVICE_URL` | `http://127.0.0.1:7820` | Where the agent finds it; `none` = local keyword search only. |
| `KNOWLEDGE_SERVICE_TOKEN` | `EH_API_TOKEN` | Shared token, sent as `x-knowledge-token`. |
| `KNOWLEDGE_DB` | `data/knowledge.db` | Chunks, keyword index and vectors. |
| `RAG_GEMINI_ENABLED` / `RAG_GEMINI_MODEL` | `true` / `gemini-embedding-001` | Gemini embeddings (needs `GEMINI_API_KEY`). |
| `RAG_LOCAL_MODEL` | `nomic-embed-text` | Local Ollama embedding model (`ollama pull nomic-embed-text`). |
| `RAG_ETL_CHECK_HOURS` | `6` | The ETL runs once a day; this is how often it checks whether today's run happened. |

## RAG evaluation

```bash
emailhelper eval generate 500
```

```bash
emailhelper eval run
```

`generate` builds `data/eval/dataset.jsonl` from the knowledge folder:
- **facts:** a recruiter asks for a known fact;
- **grounded:** a question written by the LLM from a real chunk, with that
  chunk's file expected among the retrieved ones;
- **isolation:** asked about another person;
- **privacy:** PAN, DOB and similar; the private values are checked locally
  and never sent to a model;
- **unanswerable:** the answer must not be invented.

`run` answers every question through the agent's retrieval. An LLM judge
scores groundedness, relevance, faithfulness and accuracy, alongside the
retrieval hit rate and safety violations. Reports are written to
`data/eval/report-*.json|md`. With the free Groq tier a full run of 500
takes hours, because of rate limits; `eval run 50` runs a sample.


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
