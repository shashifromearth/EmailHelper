# FAQ

**Can it send emails by itself?**
No. The backend has no SMTP or sending code. The Thunderbird extension only
calls `compose.saveMessage({mode: "draft"})`, and a test fails the build if
`sendMessage(` appears anywhere in the extension. You review and send every
draft yourself.

**Does my email leave my machine?**
Not by default. With `MODEL_MODE=LOCAL`, everything runs against Ollama on
localhost. Email content is sent to a cloud provider only if you choose a
remote mode and set its API key. Telegram messages contain the sender,
subject and IDs, not email bodies.

**What happens if Ollama is down?**
Classification falls back to deterministic rules. Drafting needs a model, so
the job retries with backoff and then fails with a Telegram alert. Nothing is
lost: re-submitting under a new `EH_WORKFLOW_VERSION` re-processes the
message.

**Will it apply to jobs without asking me?**
Only if you set `JOB_SUBMIT_MODE=auto`, and only for jobs scoring at or above
`JOB_AUTO_APPLY_THRESHOLD` (default 80) with every required answer known and
no CAPTCHA or OTP pending. The default `review` mode fills the form and then
asks on Telegram before submitting. `dry_run` never submits.

**Does it solve CAPTCHAs?**
No. CAPTCHAs, OTPs, MFA and login walls pause the application and ask you to
complete them in the worker's browser window. Then you reply `done`.

**How does it answer application questions?**
In this order, stopping at the first that works:

1. Deterministic mapping from your profile JSON.
2. Answers you gave before, remembered in `answer_memory`.
3. The LLM, using only your knowledge base and only when its confidence is
   0.7 or higher.

Sensitive questions are never guessed: salary (unless set in
`salary_preferences.json`), visa, criminal record, government IDs and
similar. They are asked on Telegram instead. EEO and self-identification
questions get "decline to answer" when that option exists.

**Is LinkedIn automation allowed?**
LinkedIn's User Agreement restricts automated access. The LinkedIn features
are off by default (`RECRUITER_LINKEDIN_ENABLED=false`). When enabled, they
only read search results through *your own* logged-in session, at most 20
searches a day with 20 s between page loads. They never send messages or
connection requests. You are responsible for deciding whether to use them.
Job discovery itself prefers official public APIs (Greenhouse, Lever, Ashby)
and RSS.

**Which Thunderbird versions are supported?**
115 ESR and newer (128 tested against the API surface). Gmail, Outlook,
Zoho and any IMAP/POP account configured in Thunderbird work, because the
extension works at the Thunderbird level.

**How much memory does it use?**
The Rust agent idles at roughly 10–25 MB RSS. Ollama and the model dominate
(qwen2.5:3b ≈ 2.5 GB, 7b ≈ 5 GB). The browser worker uses about 150–300 MB
while Chromium is open.

**Can I add my own agent?**
Yes. Add a queue kind in `rust-agent/src/queue.rs`, a handler module, and
enqueue it from the pipeline. Idempotency keys, retries, audit logging and
metrics come with the queue.
