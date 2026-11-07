# EmailHelper

**An AI email assistant that reads incoming mail, understands it, and writes
the reply for you, grounded in your own knowledge and tone, and saved as a
draft for a person to approve.**

EmailHelper turns any inbox into an assisted inbox. Every new message is
classified, matched against your company's knowledge, answered by a language
model, checked by a validator, and placed in your mail client as a ready-to-send
draft. Your team reviews and sends. Nothing leaves without a human.

It runs on your own infrastructure, works with local open-source models out of
the box, and can use OpenAI, Anthropic, Google Gemini or Groq when you want
them.

---

## What it automates

| Use case | What EmailHelper does |
|---|---|
| **Customer support** | Drafts first responses to product questions, how-to requests and account issues from your help content and past approved answers; routes complaints for priority review. |
| **Shared inboxes** (`info@`, `support@`, `hello@`) | Sorts every message by intent, skips noise and no-reply mail, and prepares answers for the ones that need one. |
| **Sales and partnership inquiries** | Recognises leads and partnership requests and replies with your pricing, positioning and next steps in your voice. |
| **Billing and invoices** | Identifies invoice and payment mail and drafts the standard acknowledgement or follow-up. |
| **Follow-ups** | Picks up the thread history and writes the follow-up that matches it. |
| **Several brands or teams in one place** | Each mailbox has its own persona, knowledge and signature; a reply is written only from that mailbox's knowledge. |

Typical results: first-draft time drops from minutes to seconds, answers stay
consistent across agents, and the team spends its time on the messages that
need judgement.

## How it works

```
 New email ──► Intake ──► Classify ──► Retrieve knowledge ──► Draft ──► Validate ──► Draft in your mail client
 (Thunderbird:            intent,      company profile,       LLM in    rules +       for a person to review,
  Gmail, Outlook,         priority,    tone, approved         your      LLM review    edit and send
  Zoho, any IMAP)         injection    answers, documents     voice                   + notification
                          scan         (hybrid search)
```

1. **Intake.** A lightweight Thunderbird extension hands each new message to
   the local service. Gmail, Outlook / Microsoft 365, Zoho and any IMAP account
   work. Messages are queued durably and processed exactly once.
2. **Classify.** Rules plus a model sort mail into support, sales, complaint,
   partnership, invoice, follow-up, spam and other, and decide whether a reply
   is needed. Untrusted content is scanned for prompt-injection attempts.
3. **Retrieve.** The knowledge service searches your material (company
   profile, tone guide, approved responses, FAQs, documents) with hybrid
   keyword + vector search, plus the thread's own history.
4. **Draft.** The model writes the reply in your tone, with your signature,
   using only what the retrieved knowledge supports.
5. **Validate.** Every draft is checked before anyone sees it: empty or
   placeholder text, leaked instructions, invented e-mail addresses, links,
   phone numbers or amounts, promises the knowledge does not back, injected
   content, low confidence, plus a second model review. Drafts are marked
   **Approved** or **Review required**.
6. **Deliver.** The draft appears in the mail client as a reply in the right
   thread. An optional Telegram message tells you a new draft is waiting.

**EmailHelper never sends email.** The service has no SMTP code and the mail
client extension never calls send; a test enforces this.

## Highlights

- **Your knowledge, your voice.** Plain Markdown and JSON files: company
  profile, tone of voice, approved answers, product and policy documents.
  Update a file, and the next draft uses it.
- **Choose your model.** Local models through Ollama by default (no data
  leaves the machine). OpenAI, Anthropic Claude, Google Gemini and Groq are
  optional, with automatic fallback to the local model if a provider fails.
- **Guardrails built in.** Validation rules, a reviewing model, confidence
  scoring and prompt-injection defences before any draft reaches a person.
- **Multi-mailbox, multi-persona.** One installation serves many mailboxes,
  each with its own identity, purpose and knowledge.
- **Reliable by design.** Durable work queue, idempotent intake, bounded
  concurrency, retries with backoff, circuit breakers, and a keyword-search
  fallback so drafting continues when the knowledge service is down.
- **Observable.** Health and readiness endpoints, Prometheus metrics, Grafana
  dashboards, structured logs and a built-in web dashboard.
- **Runs where you need it.** Windows, Linux or Docker; systemd units and
  Windows service scripts included. All services bind to localhost and
  require an API token.
- **Built for performance.** Core services in Rust (tokio, axum, SQLite with
  FTS5 and sqlite-vec).

## Architecture

| Component | Role | Location |
|---|---|---|
| Email agent | Intake API, queue, classifier, drafter, validator, notifications, dashboard (port 7800) | `rust-agent/` |
| Knowledge service | Ingestion, embeddings, hybrid search over your documents (port 7820) | `knowledge-service/`, `knowledge-core/` |
| Mail client extension | New-mail detection and draft saving in Thunderbird | `thunderbird-extension/` |
| Knowledge base | Company profile, tone, approved responses, documents | `knowledge/` (see `knowledge.example/`) |
| Operations | Docker Compose, Prometheus, Grafana, systemd, Windows tasks | `docker/`, `monitoring/`, `deploy/` |

Other mail sources (a helpdesk, a webhook, another client) can feed the same
REST intake API; see [documents/API.md](documents/API.md).

## Quick start

```bash
cp .env.example .env                       # set EH_API_TOKEN (32+ random characters)
cp -r knowledge.example knowledge          # then add your own company profile, tone and answers
cargo build --release -p emailhelper
ollama pull qwen2.5:7b                      # or MODEL_MODE=MOCK to try it without a model
./target/release/emailhelper serve
curl http://127.0.0.1:7800/health
```

Install `dist/emailhelper-thunderbird.xpi` in Thunderbird and enter the same
token in its options. On Windows, `run.cmd` builds and starts everything in one
step. Full instructions: [documents/INSTALL.md](documents/INSTALL.md).

## Adapting it to your business

- **Knowledge:** put your FAQs, policies, product sheets and best past replies
  in `knowledge/`; the knowledge service indexes them automatically.
- **Tone and signature:** one Markdown file per mailbox or brand.
- **Review flow:** drafts the validator is unsure about are marked *Review
  required*, so the team knows which ones to read closely.
- **Models:** pick local or cloud per deployment, with timeouts and fallback
  (`MODEL_MODE`, `*_API_KEY`, `LLM_REVIEW` in `.env`).
- **Notifications:** a Telegram alert whenever a new draft is ready.
- **Custom work:** new categories, other mail sources or helpdesk
  integrations are straightforward extensions of the pipeline.

Every setting is documented in
[documents/CONFIGURATION.md](documents/CONFIGURATION.md).

## Security and privacy

- Drafts only; sending is always a human decision.
- Local-first: with the default local model, mail and knowledge never leave
  your machine. Cloud models are used only when you add a key.
- Services listen on 127.0.0.1 and require a token; secrets stay in `.env`.
- Mail content is treated as untrusted: fenced in prompts and scanned for
  injection.

Threat model and controls: [documents/SECURITY.md](documents/SECURITY.md).

## Documentation

| Document | Contents |
|---|---|
| [INSTALL.md](documents/INSTALL.md) | Installation on Windows, Linux and Docker; Thunderbird; Telegram |
| [ARCHITECTURE.md](documents/ARCHITECTURE.md) | Components, email workflow, data model, reliability |
| [API.md](documents/API.md) | REST API and Telegram protocol |
| [KNOWLEDGE.md](documents/KNOWLEDGE.md) | Mailboxes, personas, knowledge files |
| [CONFIGURATION.md](documents/CONFIGURATION.md) | Every environment variable, with examples |
| [MONITORING.md](documents/MONITORING.md) | Health endpoints, metrics, Prometheus, Grafana, logs |
| [SECURITY.md](documents/SECURITY.md) | Threat model, controls, residual risks |
| [TESTING.md](documents/TESTING.md) | Test suites and how to run them |
| [TROUBLESHOOTING.md](documents/TROUBLESHOOTING.md) | Symptoms and fixes |
| [FAQ.md](documents/FAQ.md) | Common questions |
| [thunderbird-extension/README.md](thunderbird-extension/README.md) | Mail client extension |
| [docker/README.md](docker/README.md), [deploy/README.md](deploy/README.md) | Docker Compose, systemd, Windows service |

## Development

```bash
scripts/test-all.ps1        # Windows: formatting, lints and the full test suite
scripts/test-all.sh         # Linux / macOS
```

## Work with us

EmailHelper is the foundation for custom AI email automation: support desks
that answer in your voice, shared inboxes that triage themselves, and
assistants connected to your own knowledge and systems. If your company wants
an email support system like this, set up on your infrastructure, trained on
your knowledge and integrated with your tools, get in touch:

**Contact:** [techgroup@khmtechnology.com](mailto:shashilpu+EmailHelper@gmail.com)
