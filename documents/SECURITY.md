# Security Review

## Threat model

EmailHelper processes untrusted input (inbound email, web pages, job
postings) on a personal machine that holds credentials (mail accounts,
browser sessions) and personal data (resume, salary expectations). The main
risks:

1. A remote attacker uses email content to make the agent act: prompt
   injection.
2. A local web page or process calls the local API (CSRF / local abuse).
3. Data leaves the machine unexpectedly.
4. Actions are taken irreversibly on the user's behalf.

## Controls

| Area | Control |
|---|---|
| Network exposure | The API binds to `127.0.0.1` and refuses non-loopback addresses unless `EH_ALLOW_NON_LOOPBACK` is set; Docker uses it only inside a private network, with ports published on 127.0.0.1. The browser worker binds to 127.0.0.1 and refuses `0.0.0.0` unless `BROWSER_ALLOW_REMOTE=true`. |
| Authentication | Every `/api/*` route needs `X-EmailHelper-Token`, compared in constant time. The worker needs `X-Worker-Token`. If no token is set, a random 64-hex-char token is generated. Short tokens trigger a warning. |
| CSRF / browser abuse | No CORS headers. Requests carrying a foreign `Origin` (anything other than `moz-extension://`, `http://127.0.0.1` or `http://localhost`) are rejected even with a valid token. Responses set `nosniff`, `X-Frame-Options: DENY`, `no-store`, and a CSP on the dashboard. |
| Input validation | JSON body limit (`413`); field length checks (`422`); recipient and attachment count limits; NUL rejection; body truncation; attachment content never accepted (metadata only, oversize flagged). |
| Prompt injection | Untrusted text is sanitised: control and zero-width/bidi characters stripped, delimiters neutralised, length truncated. It is fenced in `<<<UNTRUSTED …>>>` blocks under a system rule to treat it as data. Injection phrases are detected (ignore-instructions, role override, prompt exfiltration, role tags, jailbreak, credential exfiltration); any hit forces `REVIEW_REQUIRED`. |
| Hallucination / policy | The validator flags email addresses, links, amounts and phone numbers that don't appear in the sources, unbacked refund/discount/guarantee/liability language, placeholders and leaked prompts. An optional LLM reviewer adds a second check. |
| Irreversible actions | Email is never sent: there is no SMTP code, and the extension only saves drafts (enforced by a test). Job submission defaults to `review`; it never submits below threshold, with missing answers, or with an unresolved CAPTCHA/OTP. Unclear approvals are not treated as consent. CAPTCHAs are never solved automatically. LinkedIn: read-only, opt-in, rate-limited, no messaging. |
| Uploads | The worker only uploads files under `BROWSER_ALLOWED_UPLOAD_DIRS` (resolved absolute paths). |
| Secrets | Secrets are read from the environment / `.env` only. `.env` is git-ignored and excluded from the Docker build context. API keys go in headers (Gemini uses `x-goog-api-key`, not the URL). Internal error details are logged, not returned to clients. |
| Telegram | Only messages from `TELEGRAM_CHAT_ID` are processed. Notifications carry metadata (sender, subject, IDs), not email bodies. |
| Data at rest | SQLite in `EH_DATA_DIR`. Use OS disk encryption (BitLocker / LUKS). The audit log records all agent actions, including the full answers on submitted applications. |
| Supply chain | Dependencies are pinned via `Cargo.lock` and `package-lock.json`. Images are pinned for Prometheus and Grafana. Rust TLS uses rustls with no OpenSSL. |
| Process hardening | The systemd unit uses `NoNewPrivileges`, `ProtectSystem=strict`, `PrivateTmp` and an unprivileged user. The Docker agent runs non-root with a read-only root filesystem, all capabilities dropped and `no-new-privileges`. |

## Residual risks

- A sufficiently clever injection could still bias a draft's wording. This is
  mitigated because every draft is human-reviewed before sending, and flagged
  drafts are marked for review.
- `auto` submit mode acts on the user's behalf. Keep `review` until the
  answers have been checked.
- LinkedIn automation may conflict with LinkedIn's terms; it's off by default.
- A local process running as the same OS user can read `.env` and the
  database. This is inherent to local-first software.
