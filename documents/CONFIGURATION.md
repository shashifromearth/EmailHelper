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
| `RESEARCH_APPLICANT_COUNTRY` | first authorized country in the research person's profile | Remote jobs must be open to applicants in this country ("Worldwide", "APAC", "India"); jobs limited to other regions, or whose description says "US only", "must be located in the US", "EU only", "US citizens" and similar, are dropped. On-site / hybrid jobs are kept when they are in this country, or abroad when the description offers visa sponsorship, relocation help or welcomes international applicants (and doesn't say "no sponsorship"). |
| `RESEARCH_REQUIRE_REMOTE` | `false` | `true`: a kept job must be remote (or a distributed team). `false`: on-site / hybrid jobs are kept too, under the rule above. |
| `RESEARCH_WORK_TYPES` | `none` | Hard filter: a kept job must be one of these role types (e.g. `contract;freelance;consultant`), judged from the board's employment type, the title/tags and clear phrases such as "contract role" or "independent consultant". `none` = any role type. |
| `RESEARCH_PREFERRED_WORK_TYPES` | `contract;freelance;consultant` | Role types ranked first. |
| `RESEARCH_PRIORITY_BOOST` | `10` | Ranking points for a remote job, and again for a preferred role type. Orders the Telegram summary and the applications started; the stored match score is unchanged. `0` turns ranking boosts off. |
| `RESEARCH_TECH_STACK` | `python;c#;.net;java;rust;go;genai;agentic ai` | A kept job must mention one of these (synonyms included: Django/FastAPI, ASP.NET, Spring, Golang, LLM/RAG/LangChain, LangGraph/multi-agent). Titles must be engineering roles: developer/engineer, senior/lead/staff/principal, architect, engineering manager/director. `none` falls back to `RESEARCH_SEARCH_TERMS` matching titles. |

### Job APIs and search APIs

Read once a day after the boards above. Every request carries an identifying
User-Agent and waits between requests; queries rotate daily so a long list
is covered over a few days within the budget. A source whose key is missing
is skipped with one log line. Jobs found on several sources are kept once
(same canonical URL, or same company and title). Tests and `research_check`
use `RESEARCH_SOURCES=none` or mock servers; no test calls a real API.

| Variable | Default | Description |
|---|---|---|
| `RESEARCH_SOURCES` | `adzuna;jooble;careerjet;arbeitnow;themuse;google_cse;brave` | Sources to read (`none` = off). |
| `RESEARCH_API_QUERIES` | 13 role + stack keywords (`python developer`, `.net developer`, `golang developer`, `genai architect`, `engineering manager`…) | Keywords for Adzuna, Jooble and Careerjet. |
| `RESEARCH_API_MAX_REQUESTS` | `250` | Requests per run across all API sources. |
| `RESEARCH_API_MAX_REQUESTS_PER_SOURCE` | `40` | Requests per run for one source. |
| `RESEARCH_API_DELAY_MS` | each API's own (Adzuna 2500, Brave 1100, The Muse/Google 500, others 1000) | Pause between two requests to one API. |
| `ADZUNA_APP_ID` / `ADZUNA_APP_KEY` | — | [developer.adzuna.com](https://developer.adzuna.com) (free: 25 requests/min, 250/day). |
| `ADZUNA_COUNTRIES` | `in;us;gb;de;sg;nl` | Adzuna country codes (Adzuna covers at, au, be, br, ca, ch, de, es, fr, gb, in, it, mx, nl, nz, pl, sg, us, za; no UAE). |
| `ADZUNA_API_BASE` | `https://api.adzuna.com/v1/api/jobs` | Endpoint override (tests). |
| `JOOBLE_API_KEY` | — | [jooble.org/api/about](https://jooble.org/api/about). |
| `JOOBLE_LOCATIONS` | `Remote;India` | One query per keyword and location. |
| `JOOBLE_API_BASE` | `https://jooble.org/api` | Endpoint override. |
| `CAREERJET_AFFID` | — | Affiliate id from [careerjet.com/partners](https://www.careerjet.com/partners/api). |
| `CAREERJET_LOCALES` | `en_IN;en_AE;en_SG;en_GB;en_US` | Careerjet sites to search (the locale's country is added to each job's location). |
| `CAREERJET_USER_IP` | `127.0.0.1` | Sent as `user_ip` (Careerjet requires it); set your public IP. |
| `CAREERJET_API_BASE` | `http://public.api.careerjet.net/search` | Endpoint override. |
| `ARBEITNOW_PAGES` / `ARBEITNOW_API_BASE` | `3` / `https://www.arbeitnow.com/api/job-board-api` | No key. Mostly Europe; on-site jobs pass only with visa sponsorship / relocation. |
| `THEMUSE_PAGES` / `THEMUSE_API_KEY` / `THEMUSE_API_BASE` | `5` / — / `https://www.themuse.com/api/public/jobs` | No key needed (a key raises the hourly limit). Reads senior and management Software Engineering / Data Science jobs marked "Flexible / Remote". |
| `GOOGLE_CSE_KEY` / `GOOGLE_CSE_CX` | — | Google Programmable Search JSON API key and search engine id (free: 100 queries/day). |
| `GOOGLE_CSE_API_BASE` | `https://www.googleapis.com/customsearch/v1` | Endpoint override. |
| `BRAVE_SEARCH_KEY` | — | Brave Search API subscription token ([api-dashboard.search.brave.com](https://api-dashboard.search.brave.com)). |
| `BRAVE_SEARCH_API_BASE` | `https://api.search.brave.com/res/v1/web/search` | Endpoint override. |
| `RESEARCH_SEARCH_ROLES` | `developer;senior engineer;architect;lead;engineering manager` | Search-API queries are `site:<site> "<role>" <stack>`; Google and Brave each take a different daily window of them. |
| `RESEARCH_SEARCH_STACKS` | `python;.net;java;golang;rust;generative ai;agentic ai` | |
| `RESEARCH_SEARCH_SITES` | `linkedin;glassdoor;naukri;wellfound;indeed;instahyre;hirist;foundit;iimjobs;upwork;freelancer;arc;dice;remoteco` | Only results that are job postings on these sites are kept. The sites themselves are never fetched; their postings are listed in the Telegram summary with the link, to apply with the browser extension (the agent never applies to them). |
| `RESEARCH_AUTO_APPLY` | `false` | Start applications for discovered jobs that aren't skipped (needs `JOB_AGENT_ENABLED`). Jobs from your own mail are handled by the job agent regardless. |
| `RECRUITER_DISCOVERY_ENABLED` | `false` | Finds recruiters for recruiting or job emails. |
| `RECRUITER_LINKEDIN_ENABLED` | `false` | LinkedIn people search through your logged-in worker profile. |
| `RECRUITER_DAILY_LIMIT` | `20` | Maximum LinkedIn searches per day. |

## Knowledge files

People, mailboxes, purposes, private facts and resumes are described in
[KNOWLEDGE.md](KNOWLEDGE.md).


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
