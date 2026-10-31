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
| `GEMINI_API_KEY` / `GEMINI_MODEL` / `GEMINI_BASE_URL` | — / `gemini-flash-latest` / `https://generativelanguage.googleapis.com/v1beta` | The key is sent in a header, never in the URL. The knowledge service uses the same key and base URL for embeddings. |
| `ANTHROPIC_API_KEY` / `ANTHROPIC_MODEL` / `ANTHROPIC_BASE_URL` | — / `claude-haiku-5-5` / `https://api.anthropic.com/v1` | Messages API. |
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

Browser worker settings (read by `browser-worker/`, which also needs
`BROWSER_WORKER_TOKEN`; see `browser-worker/README.md`):

| Variable | Default | Description |
|---|---|---|
| `BROWSER_WORKER_HOST` | `127.0.0.1` | Listen address; non-loopback is refused unless `BROWSER_ALLOW_REMOTE=true`. |
| `BROWSER_WORKER_PORT` | `7801` | Listen port. |
| `BROWSER_ALLOW_REMOTE` | `false` | Allows a non-loopback host; only on an isolated internal network (Docker sets it). |
| `BROWSER_PROFILE_DIR` | `./data/browser-profile` | Persistent browser profile (your logged-in sessions). |
| `BROWSER_HEADLESS` | `true` | `false` shows the browser window. |
| `BROWSER_CHANNEL` | *(bundled Chromium)* | Use an installed browser: `msedge` or `chrome`. |
| `BROWSER_SCREENSHOT_DIR` | `./data/screenshots` | Where step screenshots are saved. |
| `BROWSER_ALLOWED_UPLOAD_DIRS` | `./knowledge;./data` | Only files under these directories may be uploaded. |
| `BROWSER_NAV_TIMEOUT_MS` | `30000` | Page navigation timeout. |
| `BROWSER_MAX_PAGES` | `3` | Concurrent browser operations and open pages; further requests queue. |
| `BROWSER_IDLE_TIMEOUT_MS` | `600000` | Idle session pages are closed after this long. |
| `BROWSER_NETWORK_IDLE_CAP_MS` | `3000` | Upper limit on the "network idle" wait after navigation. |
| `BROWSER_LOG_LEVEL` | `info` | `debug`, `info`, `warn`, `error` or `silent`. |
| `LINKEDIN_MIN_INTERVAL_MS` | `20000` | Minimum time between LinkedIn navigations (lower values are raised to 20000). |
| `LINKEDIN_MAX_SEARCHES_PER_REQUEST` | `3` | LinkedIn people searches per request (one per role). |
| `LINKEDIN_BASE_URL` | `https://www.linkedin.com` | Test-only override (mock server). |

## Job-link queue

`job-links/` is a separate local service: the shared job database the
Chrome extension applies from (in your own browser). With
`JOB_LINKS_ENABLED=true` the job agent writes every job link it finds in mail
to the service, with the person, mailbox, email id, what the alert says about
the job, a matcher score/decision from that and the resume to use, **instead
of** opening pages and applying through the browser worker. Job emails are
still only handled when `JOB_AGENT_ENABLED=true`. The research agent also
puts every job it keeps (not skipped by the matcher) there, with
`source=research`, the board or API name, when it was posted, the score and
the resume to use; its daily summary counts them.

| Variable | Default | Description |
|---|---|---|
| `JOB_LINKS_ENABLED` | `false` | Queue job links on the service instead of applying with the browser worker. `run.cmd` also starts the service when this (or `JOB_LINKS_TOKEN`) is set. |
| `JOB_LINKS_URL` | `http://127.0.0.1:7810` | Service address, as the agent sees it. |
| `JOB_LINKS_TOKEN` | — | Shared secret (`x-joblinks-token` header), 16+ characters. The service refuses to start without it; the agent and the extension send the same value. |

Read only by the service (`job-links/README.md` has the API):

| Variable | Default | Description |
|---|---|---|
| `JOB_LINKS_PORT` | `7810` | Port; the service binds 127.0.0.1 only. |
| `JOB_LINKS_SNAPSHOT` | `data/job-links.json` | JSON snapshot loaded at start-up and rewritten atomically when links change. |
| `JOB_LINKS_SNAPSHOT_SECS` | `5` | How often a changed store is written (and once more on shutdown). |
| `JOB_LINKS_LEASE_SECS` | `900` | How long a claimed link stays claimed without a result before it goes back to `new`. |
| `JOB_LINKS_MAX_AGE_HOURS` | `336` (14 days) | Claims skip `new` links posted longer ago than this (undated links: first seen); they stay in the store and in listings. `0` = no limit. A claim's `max_age_hours` overrides it. Links with the same score and work type (remote / contract) are claimed newest first. |

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
| `RESEARCH_REMOTE_BOARDS` | `remotive;remoteok;himalayas;jobicy;weworkremotely` | Remote-job boards read once a day through their public APIs/feeds (`none` turns them off): each board's recent list plus one worldwide search per `RESEARCH_TECH_STACK` term (C#/.NET share one), at most 9 requests per board. We Work Remotely has no search and is read from its programming feeds. `RESEARCH_<BOARD>_URL` replaces a board's requests with one endpoint (tests). |
| `RESEARCH_MAX_AGE_HOURS` | `25` | Only postings published within this many hours are kept. Postings without a date (SearXNG hits, career pages) are exempt and judged on the other rules; the job store dedupes by URL, so an undated job is new only the first time it is seen. `0` keeps everything. |
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
| `RESEARCH_API_QUERIES` | 21 role + stack keywords (`python developer`, `.net architect`, `golang developer`, `genai architect`, `tech lead`, `staff engineer`, `principal engineer`, `engineering director`, `ai engineering manager`, `freelance developer`, `contract developer`…; full list in `.env.example`) | Keywords for Adzuna, Jooble and Careerjet. One request per keyword and country/location; the list rotates daily within the budget. |
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
| `RESEARCH_SEARCH_ROLES` | `developer\|software engineer;senior engineer\|staff engineer\|principal engineer;tech lead\|lead developer\|lead engineer;architect;engineering manager\|engineering director\|head of engineering;contract\|freelance` | Search-API queries are `site:<site> "<role>" <stack>`; Google and Brave each take a different daily window of them. `a\|b` in one entry becomes `("a" OR "b")`, so several roles share one query (6 roles × 6 stacks × 14 sites by default). |
| `RESEARCH_SEARCH_STACKS` | `python;c#\|.net;java;golang;rust;generative ai\|agentic ai` | Stack words, same `\|` syntax. |
| `GREENHOUSE_API_BASE` / `LEVER_API_BASE` / `ASHBY_API_BASE` | `https://boards-api.greenhouse.io/v1/boards` / `https://api.lever.co/v0/postings` / `https://api.ashbyhq.com/posting-api/job-board` | Endpoint overrides (tests). |
| `RESEARCH_SEARCH_SITES` | `linkedin;glassdoor;naukri;wellfound;indeed;instahyre;hirist;foundit;iimjobs;upwork;freelancer;arc;dice;remoteco` | Only results that are job postings on these sites are kept. The sites themselves are never fetched; their postings are listed in the Telegram summary with the link, to apply with the browser extension (the agent never applies to them). |
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
| `KNOWLEDGE_SERVICE_ALLOW_REMOTE` | `false` | `true` allows a non-loopback `KNOWLEDGE_SERVICE_ADDR`; only inside an isolated container network. |
| `KNOWLEDGE_SERVICE_URL` | `http://127.0.0.1:7820` | Where the agent finds it; `none` = local keyword search only. |
| `KNOWLEDGE_SERVICE_TOKEN` | `EH_API_TOKEN` | Shared token, sent as `x-knowledge-token`. |
| `KNOWLEDGE_DB` | `$EH_DATA_DIR/knowledge.db` | Chunks, keyword index and vectors. |
| `RAG_GEMINI_ENABLED` / `RAG_GEMINI_MODEL` | `true` / `gemini-embedding-001` | Gemini embeddings (needs `GEMINI_API_KEY`; uses `GEMINI_BASE_URL`). `false` = local Ollama embeddings only. |
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
