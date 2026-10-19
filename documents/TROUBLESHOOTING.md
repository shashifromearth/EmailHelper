# Troubleshooting

Start with the health endpoint and the logs:

```bash
curl -s http://127.0.0.1:7800/health          # database / llm / browser_worker / telegram
RUST_LOG=info,emailhelper=debug EH_LOG_FORMAT=pretty emailhelper serve
```

All logs are structured. Filter by `email_id`, `job_id`, `application_id`
or `kind`. The audit trail is at `GET /api/v1/audit?entity_id=<id>`.

| Symptom | Likely cause | Fix |
|---|---|---|
| `configuration error: invalid value for EH_BIND_ADDR` | Non-loopback address | Use `127.0.0.1:<port>`. Docker sets `EH_ALLOW_NON_LOOPBACK=true` inside its private network. |
| Extension shows **401 / invalid token** | Token mismatch | Copy `EH_API_TOKEN` (or `data/api-token.txt` if auto-generated) into the extension options. |
| No drafts appear | Extension not running, or long poll failing | Open the extension popup and check backend health. Look for `drafts_pending` in `/api/v1/status`. |
| Drafts are all `REVIEW_REQUIRED` with "LLM review unavailable" | Ollama not reachable | `ollama serve`, `ollama pull $OLLAMA_MODEL`, check `OLLAMA_BASE_URL`. `/health` shows `llm: degraded`. |
| Draft generation is slow (>15 s) | Model too large for the hardware | Use `OLLAMA_MODEL=qwen2.5:3b`, or a GPU. Watch `llm_request_duration_seconds`. |
| Remote model not used | API key missing | Remote modes fall back to Ollama without a key; a startup warning says so. |
| Email processed twice | Different `message_id`, or the workflow version was bumped | Idempotency is per `(account, Message-ID, version)`. Check the extension's message-id extraction. |
| `job failed permanently` | Repeated errors (LLM, worker, network) | Check the `last_error` column in `processing_jobs`, the audit `job_dead` record and the Telegram alert. |
| Telegram silent | Disabled or misconfigured | `TELEGRAM_ENABLED=true` plus token and chat id. `/health` shows `telegram: ok`. Check `failures_total{component="telegram"}`. |
| Bot ignores my replies | Wrong chat, or ambiguous question | Only `TELEGRAM_CHAT_ID` is accepted. Reply *to* the question message, or prefix the answer with `Q-XXXX`. `/pending` lists open questions. |
| Applications stuck in `blocked` (`captcha`, `login_required`) | Site needs a human | Restart the worker with `BROWSER_HEADLESS=false`, solve the challenge or log in in that window, then reply `done`. |
| `upload_failure: resume file not found` | No resume | Put it at `knowledge/resume.pdf`, or set `JOB_RESUME_PATH` / `resume.json.resume_file`. It must be inside `BROWSER_ALLOWED_UPLOAD_DIRS`. |
| `browser_worker: down` | Worker not started, or token mismatch | Start `browser-worker` (`npm start`). The tokens must match on both sides. |
| SearXNG 403 / `json format` error | JSON output disabled | Add `json` to `search.formats` in SearXNG `settings.yml` (the provided `docker/searxng/settings.yml` does this). |
| LinkedIn search returns `login_required` | Profile not logged in | Log in once in headed mode; the session is stored in `BROWSER_PROFILE_DIR`. |
| `database is locked` | Another process holds the DB | Run only one agent per database file. |

## Resetting state

- To re-process all mail, bump `EH_WORKFLOW_VERSION`.
- To retry one application, call `POST /api/v1/applications/{id}/resume`.
- To force a knowledge re-index, run `emailhelper reindex`, or just edit the
  files; they are re-indexed automatically by mtime.
- To start completely fresh, stop the agent and delete `data/emailhelper.db*`.
