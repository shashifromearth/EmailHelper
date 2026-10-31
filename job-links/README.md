# job-links

A small local service that queues job links. The email agent (and later the
research agent, or you by hand) adds links; the Chrome extension claims them,
fills the application in your own browser and reports the result.

- Binds to `127.0.0.1` only (port `JOB_LINKS_PORT`, default `7810`).
- Every `/v1` call needs the header `x-joblinks-token: <JOB_LINKS_TOKEN>`;
  `/health` is open.
- CORS admits `chrome-extension://…` origins only.
- Links live in memory and are written to a JSON snapshot
  (`JOB_LINKS_SNAPSHOT`, default `data/job-links.json`) every
  `JOB_LINKS_SNAPSHOT_SECS` seconds when something changed, and on Ctrl+C.
  The file is replaced atomically (temp file + rename) and loaded at start-up.
  A snapshot that can't be parsed stops the service instead of being
  overwritten: move it aside to start empty.

## Run

```bash
JOB_LINKS_TOKEN=$(openssl rand -hex 24) cargo run -p job-links
```

`run.cmd` / `scripts\run-local.ps1` start it with the rest of the stack when
`JOB_LINKS_ENABLED=true` (or `JOB_LINKS_TOKEN` is set) in `.env`.

| Variable | Default | Meaning |
|---|---|---|
| `JOB_LINKS_TOKEN` | — (required, 16+ chars) | Shared secret for `x-joblinks-token`. The agent sends the same value. |
| `JOB_LINKS_PORT` | `7810` | Port on 127.0.0.1. |
| `JOB_LINKS_SNAPSHOT` | `data/job-links.json` | Snapshot file. |
| `JOB_LINKS_SNAPSHOT_SECS` | `5` | How often a changed store is written. |
| `JOB_LINKS_LEASE_SECS` | `900` | Lease for a claim that doesn't ask for one. |
| `RUST_LOG` | `info` | Log filter. |

## Records

```json
{
  "id": "6f1c…", "url": "https://www.linkedin.com/jobs/view/4480916162/",
  "original_url": "https://www.linkedin.com/comm/jobs/view/4480916162/?trackingId=…",
  "source": "email", "platform": "linkedin",
  "title": "Lead AI Engineer", "company": "Netrolynx AI", "location": "United States",
  "person_id": "shashi", "mailbox": "me@example.com", "email_id": "…",
  "status": "new", "status_reason": null, "claimed_by": null,
  "claimed_at": null, "lease_expires_at": null, "completed_at": null,
  "created_at": "…", "updated_at": "…", "attempts": 0
}
```

- `source`: `email` | `research` | `manual` (default).
- `platform`: `linkedin` | `naukri` | `glassdoor` | `wellfound` | `indeed` |
  `greenhouse` | `lever` | `ashby` | `workday` | `career_site` | `other`,
  detected from the URL (a given value is used only when the URL says nothing
  more specific than `career_site`/`other`).
- `status`: `new` → `claimed` → `applied` | `failed` | `skipped`.
- `url` is canonical: redirect wrappers (SafeLinks, Google, LinkedIn, Naukri)
  unwrapped, fragments and tracking parameters (`utm_*`, `trackingId`,
  `gh_src`, `lever-source`, …) removed, identifying ones (`gh_jid`, Indeed
  `jk`, Glassdoor `jl`) kept, the query sorted. LinkedIn links become
  `https://www.linkedin.com/jobs/view/<id>/`.
- Links are unique per canonical `url` + `person_id`. Adding one again merges
  metadata into the existing record (empty fields are filled, nothing is
  overwritten) and never changes its status.

## API

| Method | Path | Body / query | Result |
|---|---|---|---|
| `GET` | `/health` | — | `{"status":"ok","links":n}` (no token) |
| `POST` | `/v1/links` | one link, an array, or `{"links":[…]}` (max 500) | one: `201`/`200` `{"created":bool,"link":…}`; batch: `200` `{"created","updated","failed","results":[{"index","created","link"} \| {"index","error"}]}` |
| `GET` | `/v1/links` | `status`, `platform`, `person`, `source`, `limit` (100, max 1000), `offset`, `order=asc\|desc` (newest first by default) | `{"total","offset","links":[…]}` |
| `GET` | `/v1/links/{id}` | — | the link, or `404` |
| `PATCH` | `/v1/links/{id}` | any of `title`, `company`, `location`, `mailbox`, `email_id` (`""` clears), `status` (not `claimed`; `new` releases a claim), `status_reason` | the link |
| `DELETE` | `/v1/links/{id}` | — | `204` |
| `POST` | `/v1/links/claim` | `{"client":"ext-1","limit":1,"lease_secs":900,"platforms":["lever"],"person":"shashi"}` | `{"links":[…],"lease_expires_at":…}` — the oldest matching `new` links, claimed atomically |
| `POST` | `/v1/links/{id}/result` | `{"status":"applied"\|"failed"\|"skipped","reason":"…","client":"ext-1"}` | the link; `409` if another client holds the claim or the link already has a different outcome (the same outcome again is a no-op) |
| `GET` | `/v1/stats` | — | `{"total","by_status":{…},"by_platform":{…}}` |

A claim holds a lease (`lease_secs`, 30 s – 24 h). A link whose lease runs
out without a result goes back to `new` (with `status_reason: "lease
expired"`) and can be claimed again; `attempts` counts the claims. Errors are
`{"error":"…"}` with `400`, `401`, `404`, `409`, `413` or `422`.

```bash
H="x-joblinks-token: $JOB_LINKS_TOKEN"
curl -s -H "$H" -H 'content-type: application/json' localhost:7810/v1/links \
  -d '{"url":"https://jobs.lever.co/acme/1b2c?lever-source=mail","title":"Rust Engineer"}'
curl -s -H "$H" -H 'content-type: application/json' localhost:7810/v1/links/claim -d '{"client":"cli","limit":1}'
curl -s -H "$H" -H 'content-type: application/json' localhost:7810/v1/links/<id>/result -d '{"status":"applied","client":"cli"}'
```

## Tests

```bash
cargo test -p job-links
```

Unit tests cover canonicalisation, the store (dedupe, claims, leases,
results) and snapshots; `tests/http.rs` drives the router in-process.
