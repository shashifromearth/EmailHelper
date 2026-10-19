# EmailHelper

A local-first AI assistant made of three agents. It runs on your machine and
uses local Ollama by default:

- **Email Agent:** detects new mail in Thunderbird (Gmail, Outlook, Zoho,
  IMAP), classifies it, retrieves context, drafts a reply, validates it and
  saves it as a Thunderbird draft. **It never sends email.**
- **Job Application Agent:** detects job opportunities in email, opens
  postings with Playwright, scores them against your resume, generates cover
  letters, fills forms, uploads your resume, and submits or asks you first.
  When it gets stuck (CAPTCHA, OTP, unknown question) it pauses and asks on
  Telegram, then resumes when you reply.
- **Research Agent:** runs daily job discovery (SearXNG, RSS, Greenhouse,
  Lever, Ashby, career pages), ranks the results, and finds recruiters and
  hiring managers. It writes outreach drafts for your review.

```
Thunderbird ─► MailExtension ─► Rust orchestrator (127.0.0.1:7800) ─► Email / Job / Research agents
                                     │                                   │
                                     ├─ SQLite + FTS5 knowledge base     ├─ Ollama · OpenAI · Gemini · Anthropic
                                     ├─ Prometheus /metrics · Grafana    ├─ Browser worker (Playwright, 127.0.0.1:7801)
                                     └─ Telegram bot (alerts + replies)  └─ SearXNG · RSS · ATS public APIs
```

## Quick start

```bash
cp .env.example .env                       # set EH_API_TOKEN (32+ random chars)
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
| [CONFIGURATION.md](documents/CONFIGURATION.md) | Every environment variable, with examples |
| [MONITORING.md](documents/MONITORING.md) | Health endpoints, metrics, Prometheus, Grafana, logs |
| [SECURITY.md](documents/SECURITY.md) | Threat model, controls, residual risks |
| [TESTING.md](documents/TESTING.md) | Test suites and how to run them |
| [TROUBLESHOOTING.md](documents/TROUBLESHOOTING.md) | Symptoms → fixes |
| [FAQ.md](documents/FAQ.md) | Common questions |
| [thunderbird-extension/README.md](thunderbird-extension/README.md) | Extension details |
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
browser-worker/        Node + Playwright service (extract, inspect, fill, upload, submit, LinkedIn)
tests/mock-sites/      Mock Greenhouse / Lever / career portal / blocked pages for Playwright tests
knowledge/             Your profile JSON, tone, company profile, approved responses
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
