# EmailHelper job applier (Chrome)

Takes jobs one at a time from the shared job database (the job-links
service, fed by the email agent and the research agent), opens each one and
fills the application form. **You review and submit**, or turn on
[auto-submit](#autonomy-mode-auto-submit) for employer career sites.

```
job-links :7810 ── claim / result ──────────────┐
agent :7800 ── answers, escalations, events ────┤── extension ── fills the page in your Chrome
knowledge :7820 ── resume pick ─────────────────┘
```

- **Answers:** the agent answers every field with the rules it uses itself:
  profile data first, then answers you gave before, then knowledge search
  plus the LLM. Legal agreements and sensitive questions (salary,
  demographics, IDs) are left for you, outlined in amber.
- **Learns:** when you answer a question it left for you and submit, the
  answer is remembered (per person) and filled in next time. Sensitive and
  legal questions are never remembered.
- **Resume:** the knowledge service picks the resume from your stack
  folders (`resumes/dotnet`, `golang`, and so on) by the job's skills, and
  the extension uploads it.
- **Job boards** (LinkedIn, Naukri, Glassdoor, Wellfound, Indeed): it fills
  the application and sign-up forms. You sign in, create the account
  (password), solve any check and enter verification codes.

### Hard rules

These hold in every mode, including auto-submit:

- **Never submits on job boards**: LinkedIn, Naukri, Glassdoor, Wellfound
  and Indeed (job-links `Platform::is_assisted`) are always filled, then
  you submit.
- **Never types passwords.** Sign-in and account creation stay with you.
- **Never interacts with CAPTCHAs or verification codes** (OTP, MFA). It
  detects them and stops.
- **No bot-detection evasion of any kind**: no fingerprint spoofing, no fake
  mouse movement, no randomised typing to look human. It fills fields and
  clicks the page's own buttons, nothing more.

## Install

1. In `.env`, set `JOB_LINKS_ENABLED=true` and a `JOB_LINKS_TOKEN` (16 or
   more characters), then run `run.cmd`. That starts the agent, the
   knowledge service and job-links.
2. In Chrome, open `chrome://extensions`, turn on **Developer mode**, click
   **Load unpacked** and choose this `chrome-extension` folder.
3. Open the extension's **Settings** and paste the API token (`EH_API_TOKEN`,
   or the contents of `data/api-token.txt`) and the job-links token
   (`JOB_LINKS_TOKEN`). Optionally limit it to one person or to some
   platforms, set a separate knowledge service token, or turn on
   auto-submit.

## Use

- **Start:** the next queued job opens and the form is filled. Check it,
  answer the questions outlined in amber, and click the page's Submit button
  (outlined in green).
- **Applied / Skip / Failed:** records the outcome in job-links and opens
  the next job.
- **Fill this page** (Alt+Shift+F): fill the form on any open tab, for
  example the next step of a multi-page application, or a page you opened
  yourself.
- Closing the job's tab pauses the run. Its claim expires and the job goes
  back to the queue.
- If the Apply button opens the form in a new tab, the job follows it.
- Skip and Failed always work, even while a form is being filled.

## Autonomy mode (auto-submit)

Off by default. In **Settings**, tick **Auto-submit**: "Submit
automatically on employer career sites when every required field is filled
and nothing needs you."

- **What it claims:** with auto-submit on and no platforms set, it claims
  only employer sites (greenhouse, lever, ashby, workday, career_site,
  other). Job-board jobs stay queued for a run with you at the computer.
- **When it submits:** the platform is not a job board, nothing blocks the
  page, a submit button was found, no field failed to fill, and no required
  question is left (optional questions it could not answer do not stop it).
  It clicks the page's submit button, waits for the page to settle (a
  navigation, or about 6 s) and looks for a confirmation: "thank you",
  "application received/submitted" and similar text that was not on the
  form before, or the form gone with no validation errors showing. A new
  form in its place is treated as the next page of a multi-step
  application and filled in turn (up to 5 pages).
- **Confirmed:** recorded as applied; the next job opens.
- **Not confirmed:** a `submit_failed` event (with the validation messages)
  and a Telegram escalation. It never clicks submit again unless you reply
  "done".

### Telegram escalation and backoff

When an unattended run is stuck (sign-in, CAPTCHA or code needed, required
questions left, submit failed, no form or no submit button found, an error)
it asks the agent to message you on Telegram: fix it on the computer, or
reply **skip**. The popup shows "Waiting for you — asked on Telegram; next
check in Ns" (badge `TG`).

It checks back with `chrome.alarms` (the service worker may sleep in
between; all state is in `chrome.storage.session`) after 30 s, 60 s, 120 s,
240 s, 480 s, then every 15 minutes, and gives up after 2 hours per job.

- **skip** reply: the job is recorded as skipped ("skipped on Telegram").
- **done** reply: it fills the page again (sign-in or check cleared), or,
  for questions and submit problems, checks the fields you filled and
  submits.
- **No reply, but fixed:** on each check it also looks at the page itself.
  A cleared sign-in/check with the form showing, or every required question
  now answered, resolves the escalation and it carries on. A confirmation on
  the page (you submitted yourself) records the job as applied.
- **No response for 2 hours:** the escalation is closed, the job is
  recorded as failed ("no human response") and the next job opens.
- Pressing Applied, Skip, Failed or Fill this page in the popup also closes
  the escalation.
- If the agent cannot be reached to escalate, the job waits for you in the
  popup.

With auto-submit off nothing is escalated: it waits for you in the popup as
before, but still sends the feedback events.

### Circuit breaker

Three failures in a row on one platform (failed, submit failed, or gave up
waiting) pause that platform for 30 minutes: it is left out of claims and
listed in the popup. A successful application resets the count. If every
allowed platform is paused, it waits and claims again when the first pause
ends. The state is kept in `chrome.storage.local` (`breaker`).

### Feedback events

Every outcome is sent to the agent (`POST /api/v1/apply/events`) so the
fill rules can be improved: `fill_failed` (per field, with the reason),
`blocked` (sign-in, CAPTCHA, code, access denied, rate limit), `error`
(exceptions while filling, failed jobs, "no human response"),
`submit_failed`, `escalated`, `applied` and `skipped`. Each carries the
link id, URL, platform and person. Sending events never interrupts a run:
failures are only logged to the console.

## Develop

```bash
npm test
```

`src/plan.js` and `src/api.js` are pure and unit-tested (all decisions:
auto-submit, stuck reasons, backoff, circuit breaker, submit outcome). The
functions in `src/page/` run inside the page
(`chrome.scripting.executeScript`), so each one is self-contained.

`background.js` runs state-changing handlers (clicks, tab events, alarms)
one at a time. Answers are asked six fields per request so each request
ends well inside MV3's 30-second fetch limit; a fill left half-done by a
stopped service worker shows as an error ("try Fill this page again").
