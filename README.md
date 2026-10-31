# EmailHelper

A local-first AI assistant made of three agents. It runs on your machine and
uses local Ollama by default:

- **Email Agent:** detects new mail in Thunderbird (Gmail, Outlook, Zoho,
  IMAP), classifies it, retrieves context, drafts a reply, validates it and
  saves it as a Thunderbird draft. **It never sends email.**
- **Job Application Agent:** finds the job links in email (text and HTML),
  scores them against your profile and puts them in the shared job database
  (`job-links`). The **Chrome extension** takes them one by one, fills each
  application with answers from your knowledge (profile, past answers,
  hybrid search plus an LLM) and the resume chosen for the job's stack.
  You review and submit.
- **Research Agent:** runs a daily worldwide job discovery across job APIs,
  remote boards, ATS boards and search APIs, ranking remote, contract and
  freelance roles first. It puts the jobs it keeps into the same job
  database (with source, freshness and status) and finds recruiters for
  outreach drafts.

```
Thunderbird ─► MailExtension ─► email agent (7800) ─► knowledge service (7820, hybrid RAG search)
                                     │    ▲                     ▲
              research agent ────────┤    │ form answers        │ resume pick
                                     ▼    │                     │
                              job-links (7810, job DB) ◄── Chrome extension (applies, you submit)
```

## Quick start

```bash
cp .env.example .env                       # set EH_API_TOKEN (32+ random chars)
cp -r knowledge.example knowledge          # then replace with your own profile (git-ignored)
cargo build --release -p emailhelper
ollama pull qwen2.5:7b                      # or set MODEL_MODE=MOCK to try without a model
./target/release/emailhelper serve
curl http://127.0.0.1:7800/health
```

Then install `dist/emailhelper-thunderbird.xpi` in Thunderbird and enter the
same token in the extension options. Full steps are in
[documents/INSTALL.md](documents/INSTALL.md).

## Documentation

| Document | Contents |
|---|---|
| [INSTALL.md](documents/INSTALL.md) | Installation (Windows, Linux, Docker), Thunderbird, Telegram, worker |
| [ARCHITECTURE.md](documents/ARCHITECTURE.md) | Components, workflows, state machine, data model, reliability |
| [API.md](documents/API.md) | REST API, browser worker API, Telegram protocol, knowledge files |
| [KNOWLEDGE.md](documents/KNOWLEDGE.md) | People, mailboxes and purposes, private facts, resumes per tech stack, Telegram setup |
| [CONFIGURATION.md](documents/CONFIGURATION.md) | Every environment variable, with examples |
| [MONITORING.md](documents/MONITORING.md) | Health endpoints, metrics, Prometheus, Grafana, logs |
| [SECURITY.md](documents/SECURITY.md) | Threat model, controls, residual risks |
| [TESTING.md](documents/TESTING.md) | Test suites and how to run them |
| [TROUBLESHOOTING.md](documents/TROUBLESHOOTING.md) | Symptoms → fixes |
| [FAQ.md](documents/FAQ.md) | Common questions |
| [thunderbird-extension/README.md](thunderbird-extension/README.md) | Extension details |
| [chrome-extension/README.md](chrome-extension/README.md) | Job applier: install, use, safety |
| [job-links/README.md](job-links/README.md) | Shared job database API |
| [browser-worker/README.md](browser-worker/README.md) | Playwright worker, logins, safety |
| [docker/README.md](docker/README.md), [deploy/README.md](deploy/README.md) | Docker Compose, systemd, Windows task/service |

## Repository layout

```
rust-agent/            Rust orchestrator (axum API, durable queue, agents, LLM router, Telegram, metrics)
  src/agents/          classifier · context · drafter · validator · pipeline
  src/jobs/            detect · extract · matcher · answers · cover_letter · application · browser
  src/research/        sources (SearXNG/RSS/ATS) · discovery (daily) · recruiters
  tests/               end-to-end, job agent, research/Telegram, load tests
thunderbird-extension/ MailExtension (new-mail detection, draft saving, popup, options)
chrome-extension/      Job applier (MV3): queue → fill → you submit
job-links/             Shared job database service (memory + snapshot, claim/result API)
knowledge-service/     ETL, embeddings, hybrid search, resume choice
knowledge-core/        People, profiles, resumes and ETL shared by the services
browser-worker/        Node + Playwright service (extract, inspect, fill, upload, submit, LinkedIn)
tests/mock-sites/      Mock Greenhouse / Lever / career portal / blocked pages for Playwright tests
knowledge.example/     Sample profile JSON, tone, company profile, approved responses (copy to knowledge/)
knowledge/             Your own profile and resume (git-ignored, never committed)
documents/             Documentation
docker/ monitoring/ deploy/   Compose, Prometheus/Grafana, systemd & Windows deployment
scripts/               Test and helper scripts
```

## Safety defaults

- Drafts only, never sent.
- Job agent off. When enabled, `JOB_SUBMIT_MODE=review` asks before every
  submission.
- No CAPTCHA solving: CAPTCHAs are escalated to you.
- LinkedIn features off, and read-only when enabled.
- All services bind to 127.0.0.1, and the API requires a token.
- Cloud LLMs are used only when you configure a key; otherwise everything
  runs on local Ollama.

## Development

```bash
scripts/test-all.ps1        # Windows: fmt, clippy, cargo test, extension + worker tests
scripts/test-all.sh         # Linux/macOS
```
