# EmailHelper browser worker

A small Node.js + Playwright (Chromium) HTTP service that the Rust orchestrator
calls to:

- extract job pages (text, JSON-LD, links),
- inspect and fill ATS application forms (Greenhouse, Lever, Ashby, generic),
- search LinkedIn people results with the user's own logged-in session (read only).

The HTTP contract is in [`documents/API.md`](../documents/API.md), section 2.
That document is binding. This README only summarises it.

## Setup

Requirements: Node.js 20 or newer.

```sh
cd browser-worker
npm ci                    # installs the pinned playwright version
npm run install-browsers  # downloads Chromium (about 150 MB) from the Playwright CDN
BROWSER_WORKER_TOKEN=change-me npm start
```

On Windows PowerShell:

```powershell
$env:BROWSER_WORKER_TOKEN = "change-me"; npm start
```

Relative paths (profile, screenshots, upload dirs) resolve against the
process working directory. Start the worker from the repository root (for
example `node browser-worker/src/index.js`) if you want `./knowledge` and
`./data` to point at the shared project folders.

## Environment variables

| Variable | Default | Notes |
|---|---|---|
| `BROWSER_WORKER_HOST` | `127.0.0.1` | Any non-loopback address (for example `0.0.0.0`) is refused unless `BROWSER_ALLOW_REMOTE=true`. |
| `BROWSER_ALLOW_REMOTE` | `false` | Set to `true` only on an isolated internal network. The Dockerfile sets it because compose runs the worker on an internal network. |
| `BROWSER_WORKER_PORT` | `7801` | |
| `BROWSER_WORKER_TOKEN` | (required) | Every route except `/health` needs `X-Worker-Token: <token>`. It is compared in constant time. |
| `BROWSER_PROFILE_DIR` | `./data/browser-profile` | Persistent Chromium profile. Cookies and logins survive restarts. Only one process can use a profile at a time. |
| `BROWSER_HEADLESS` | `true` | Set to `false` for manual logins (see below). |
| `BROWSER_SCREENSHOT_DIR` | `./data/screenshots` | `<session_key>-<epoch seconds>.png` after every fill. |
| `BROWSER_NAV_TIMEOUT_MS` | `30000` | |
| `BROWSER_ALLOWED_UPLOAD_DIRS` | `./knowledge;./data` | `;`-separated. Files outside these folders are refused, and symlinks are resolved first. |
| `BROWSER_MAX_PAGES` | `3` | Concurrent browser operations and open pages. Further requests wait in a queue. |
| `BROWSER_IDLE_TIMEOUT_MS` | `600000` | Idle session pages are closed after 10 minutes. |
| `BROWSER_NETWORK_IDLE_CAP_MS` | `3000` | Upper limit on the "network idle" wait after navigation. |
| `BROWSER_CHANNEL` | (unset) | Optional Playwright channel, for example `chrome` to use an installed Google Chrome. |
| `BROWSER_LOG_LEVEL` | `info` | `debug`, `info`, `warn`, `error`, `silent`. |
| `LINKEDIN_MIN_INTERVAL_MS` | `20000` | Minimum time between LinkedIn navigations. Values below 20000 are raised to 20000. |
| `LINKEDIN_MAX_SEARCHES_PER_REQUEST` | `3` | One search runs per role. Only the first N roles of a request are searched. |
| `LINKEDIN_BASE_URL` | `https://www.linkedin.com` | Only for tests against a mock. |

Logs are JSON lines on stdout: `{"ts":…,"level":…,"msg":…,"fields":{…}}`.
Answers and tokens are never logged.

## API summary

| Route | Purpose |
|---|---|
| `GET /health` | `{status, browser, headless, version}` plus `browser_running`, `open_sessions`, `active_operations`, `queued_operations`. No token needed. |
| `POST /v1/extract` | `{url}` → `{final_url, title, text, json_ld, links, blocker}` |
| `POST /v1/apply/inspect` | `{session_key, url}` → `{final_url, ats, fields, blocker}`. Follows an Apply button or link if the page has no form. |
| `POST /v1/apply/fill` | `{session_key, url, answers, files, submit}` → `{status, final_url, missing_required, blocker, confirmation_text, screenshot_path}` plus `filled`, `skipped` and `error` |
| `POST /v1/linkedin/people` | `{company, roles, limit}` → `{people, blocker}` |
| `POST /v1/session/login` | **Not part of the contract.** `{url}` opens a visible window for a manual login. See below. |

Errors use `{ "error": "<code>", "message": "…" }`:

| Status | Code |
|---|---|
| 400 | `validation_failed` |
| 401 | `unauthorized` |
| 404 | `not_found` |
| 405 | `method_not_allowed` |
| 409 | `headless_mode` |
| 413 | `payload_too_large` (bodies are limited to 1 MiB) |
| 500 | `internal_error` |
| 503 | `browser_unavailable` |

Page-level problems such as a CAPTCHA, a login wall or HTTP 403 are not HTTP
errors. They come back as `200` with a `blocker` object.

### Field ids and answers

- `id` is the element `id`, else its `name`, else `field_<n>`. `n` is the
  0-based position of the field in the returned `fields` list. A radio group,
  or several checkboxes that share a `name`, becomes one field whose `id` is
  that `name` and whose `options` are the option labels. A duplicate id gets a
  `_2`, `_3`, … suffix.
- Answer keys are matched to fields by exact id first, then by id ignoring
  case, then by label. Label matching ignores case, `_`/`-`/spaces and a
  trailing `?`, `:` or `*`. `files` keys also match a file field whose id or
  label contains the key, and `resume` and `cv` count as the same word.
- Selects and radios pick the option whose label matches exactly (ignoring
  case), then the one whose value matches, then the shortest label that
  contains the answer. `true` means "Yes" and `false` means "No".
- A single checkbox takes a boolean or a string like `"yes"`. A checkbox group
  takes an array of option labels.
- `missing_required` lists the ids of required fields that are still empty
  after filling.

## Manual login (LinkedIn, ATS accounts)

The worker never types passwords. To give it a logged-in session:

1. Stop the worker. Start it on a machine with a display and set
   `BROWSER_HEADLESS=false`. Keep the same `BROWSER_PROFILE_DIR`.
2. Run:
   ```sh
   curl -X POST http://127.0.0.1:7801/v1/session/login \
     -H "X-Worker-Token: $BROWSER_WORKER_TOKEN" -H "Content-Type: application/json" \
     -d '{"url":"https://www.linkedin.com/login"}'
   ```
3. Log in by hand in the window that opens, including any 2FA, then close the
   tab.
4. Restart the worker headless. The cookies are stored in the profile.

If the worker is headless, `/v1/session/login` returns `409 headless_mode`.

With Docker, the container has no display. Do the login once on the host with
the same profile directory (a bind mount), then start the container.

## Safety

- The worker never sends LinkedIn messages or connection requests, never opens
  profiles in bulk, and only reads the first page of search results.
- LinkedIn navigations are at least 20 seconds apart across all requests.
  Requests with several roles therefore take about 20 seconds per extra role.
  Set the orchestrator's timeout accordingly, for example 20 s × roles + 30 s.
- CAPTCHAs are never solved. They come back as `blocker.kind = "captcha"` so
  a person can deal with them, and the same applies to OTP/MFA codes and login
  walls.
- The worker submits only when `submit` is `true`, nothing required is
  missing, and there is no blocker. After any submit attempt the page is
  closed and never reused.
- Only files inside `BROWSER_ALLOWED_UPLOAD_DIRS` can be uploaded. Downloads
  are disabled.

## Tests

```sh
npm test
```

The tests start the mock ATS sites in `../tests/mock-sites` (run
`node tests/mock-sites/server.mjs --port 8080` to browse them by hand) and an
in-process worker with a temporary profile. They drive real headless Chromium,
so run `npm run install-browsers` first.

## Docker

```sh
docker build -t emailhelper-browser-worker browser-worker
docker run --rm -e BROWSER_WORKER_TOKEN=change-me -p 127.0.0.1:7801:7801 \
  -v "$PWD/data:/app/data" -v "$PWD/knowledge:/app/knowledge:ro" emailhelper-browser-worker
```

The image is based on `mcr.microsoft.com/playwright` with the same version as
the pinned `playwright` package, and it runs as the non-root user `pwuser`.

## Troubleshooting

| Problem | Fix |
|---|---|
| `Executable doesn't exist at …` | Run `npm run install-browsers`. |
| `browser_unavailable` / profile errors | Another Chromium is using `BROWSER_PROFILE_DIR`. Stop it or use a different folder. |
| LinkedIn always returns `login_required` | The session expired. Repeat the manual login. |
| A site blocks headless Chrome | Try `BROWSER_CHANNEL=chrome` or a headed run. Some sites will still refuse automation, and that is shown as a blocker. |
| `upload_failure` "outside BROWSER_ALLOWED_UPLOAD_DIRS" | Move the file into `knowledge/` or `data/`, or extend the variable. Remember that relative paths resolve against the worker's working directory. |
| Fill returns `error` "no confirmation" | The site uses an unusual confirmation, a multi-step form or client-side validation. Check the screenshot in `BROWSER_SCREENSHOT_DIR`. |
