# Testing

## One command

```powershell
scripts\test-all.ps1      # Windows: fmt --check, clippy -D warnings, cargo test, extension + worker tests
scripts\e2e-browser.ps1   # real Playwright E2E: Rust agent → browser worker → mock ATS sites
```

```bash
scripts/test-all.sh && scripts/e2e-browser.sh   # Linux/macOS
```

## Suites

| Suite | Location | What it covers |
|---|---|---|
| Rust unit tests | `rust-agent/src/**` (`#[cfg(test)]`) | Config parsing and validation, migrations, every repository, FTS query sanitising, prompt-injection detection, sanitisation, LLM JSON parsing and fallback, classifier rules, validator rules (empty, placeholders, invented emails/links/amounts/phones, unbacked promises, injection, low confidence), job-link detection and normalisation, JSON-LD and heuristic extraction, salary parsing, matcher scoring and hard rules, form answering (profile mapping, option matching, EEO decline, consent escalation, sensitive questions), cover letters, recruiter ranking, RSS/Atom and Greenhouse/Lever/Ashby parsing, scheduler timing, Telegram ref parsing, metric names |
| Email E2E | `rust-agent/tests/email_pipeline.rs` | Real HTTP API: email → classification → context → draft → validation → long poll → ack → audit/metrics. Also: duplicates across restart, 4-account multi-account (Gmail/Outlook/Zoho/IMAP), auth and Origin checks, body limits, alerts and spam getting no draft, prompt injection and hallucinations forcing review, LLM outage handling, draft-save retry/failure, crash recovery, long-poll wake-up |
| Job agent | `rust-agent/tests/job_agent.rs` | With a mocked worker and Telegram: detect → extract → score → fill → upload → submit (auto). Unknown question → Telegram → reply → resume. CAPTCHA blocks and never submits until cleared. Review-mode approval (unclear reply ≠ consent). Skip. Missing resume → upload_failure. State survives restart. Low score → no application |
| Research & Telegram | `rust-agent/tests/research_and_telegram.rs` | Draft notification format, Telegram outage resilience, daily discovery across SearXNG/RSS/Greenhouse/Lever/Ashby with ranking, de-duplication and summary, recruiter discovery (sender, LinkedIn via worker, company research, outreach drafts) |
| LLM providers | `rust-agent/tests/llm_providers.rs` | Ollama full email→draft run (JSON mode for classify and validate), OpenAI, Gemini (key in header, not URL) and Anthropic wire formats, remote→Ollama fallback |
| Load / performance | `rust-agent/tests/load.rs` | 50 concurrent emails with 10-way concurrency and 100 ms simulated model latency; ingest latency; single-draft overhead; cold start < 5 s |
| Real browser E2E | `rust-agent/tests/e2e_browser.rs` + `scripts/e2e-browser.*` | Real Chromium via the worker against mock Greenhouse (JSON-LD, Apply flow, LLM free-text answer, EEO, resume upload, confirmation), Lever, a career portal (expected CTC, notice period, cover letter), and a CAPTCHA page (blocked, nothing submitted). Mock ATS records the multipart submissions. |
| Thunderbird extension | `thunderbird-extension/tests/*.test.js` | MIME parsing, HTML→text, headers, account-type inference, payload validation, outbox retry and persistence, dedupe LRU, draft saving (reply/new, HTML vs plain, REVIEW banner, ack on success and failure, lost-ack re-ack), and a guard that `sendMessage(` never appears |
| Browser worker | `browser-worker/tests/*.test.js` | Auth and limits, extract with JSON-LD, inspect field lists for Greenhouse/Lever/career, fill + upload + submit verified through mock submissions, upload outside allowed dirs refused, CAPTCHA/OTP/login blockers, submit refused when required fields are missing, LinkedIn parser, persistent session across restart |

## Thunderbird manual verification checklist

Automated tests cover the extension's logic against a fake `messenger` API.
Before a release, also check these in a real Thunderbird with the agent
running in `MODEL_MODE=MOCK`:

1. **New message detection:** send yourself an email. Within seconds,
   `/api/v1/status` `emails_processed` increments.
2. **Multi-account processing:** repeat with a second account. `account_id`
   differs in `/api/v1/audit`.
3. **Draft creation:** a draft appears in the account's Drafts folder.
4. **Thread preservation and reply headers:** open the draft. It has
   `In-Reply-To` and `References` (View → Message Source).
5. **Restart recovery:** quit Thunderbird, send mail, restart it. The outbox
   delivers the message and the draft appears.
6. **Duplicate prevention:** move the message out of and back into the Inbox.
   No second draft.
7. **No automatic sending:** the Sent folder is unchanged; every draft
   waits for you.

## Latest results (2026-10-19, Windows 10, Rust 1.99.1 GNU, Node 24.20)

| Suite | Result |
|---|---|
| `cargo fmt --check`, `cargo clippy --all-targets -D warnings` | clean |
| Rust unit | 65 passed |
| email_pipeline / job_agent / research_and_telegram / llm_providers / load | 11 / 8 / 4 / 5 / 3 passed |
| Real-browser E2E (`scripts/e2e-browser.ps1`) | passed: Greenhouse, Lever and career portal submitted with resume; CAPTCHA blocked with no submission |
| Thunderbird extension | 112 passed |
| Browser worker (Playwright) | 47 passed |

## Performance (release build, `MODEL_MODE=MOCK`)

| Target | Measured |
|---|---|
| Idle RAM < 250 MB | **15.5 MB** working set (18.3 MB after processing) |
| Cold start < 5 s | **77 ms** internal startup; 1.5 s from process spawn to `/readiness` 200 |
| Draft creation < 15 s | **0.28 s** orchestration overhead; real time = overhead + model inference (qwen2.5:7b on CPU typically 5–12 s for the 3 LLM calls) |
| ≥ 10 concurrent emails | 50 concurrent emails in **2.6 s** with 10-way concurrency and 100 ms simulated LLM latency per call (≈19 emails/s); ingest p100 < 90 ms |
| Binary size | 10.2 MB |
