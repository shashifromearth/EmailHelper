# Installation Guide

EmailHelper has up to four local pieces. Only the first two are needed for
email drafting.

| Piece | Needed for | Install |
|---|---|---|
| Rust agent (`emailhelper`) | everything | build from source (below) or Docker |
| Thunderbird extension | email detection + draft saving | `dist/emailhelper-thunderbird.xpi` |
| Ollama + a model | local LLM (default) | https://ollama.com |
| Browser worker (Node + Playwright) | job applications, LinkedIn, career pages | `browser-worker/` |
| SearXNG | web research (optional) | Docker (`docker/`) |

## 1. Prerequisites

### Windows 10/11

```powershell
winget install Rustlang.Rustup          # then: rustup default stable-x86_64-pc-windows-msvc (needs VS Build Tools)
                                        # or the GNU toolchain + WinLibs GCC (no Visual Studio needed):
winget install BrechtSanders.WinLibs.POSIX.UCRT
rustup toolchain install stable-x86_64-pc-windows-gnu
rustup default stable-x86_64-pc-windows-gnu
winget install OpenJS.NodeJS.LTS        # only for the browser worker
winget install Ollama.Ollama
```

### Linux (Debian/Ubuntu)

```bash
sudo apt install build-essential pkg-config curl
curl https://sh.rustup.rs -sSf | sh
curl -fsSL https://ollama.com/install.sh | sh
# Node 20+ for the browser worker: https://nodejs.org or your distro packages
```

## 2. Build the agent

```bash
git clone <your repo> EmailHelper && cd EmailHelper
cargo build --release -p emailhelper
# binary: target/release/emailhelper(.exe)
```

## 3. Configure

```bash
cp .env.example .env
```

Then edit `.env`. At minimum, set `EH_API_TOKEN` to a long random string, for example:

```bash
python -c "import secrets; print(secrets.token_hex(24))"
```

Then create your private knowledge folder with `cp -r knowledge.example knowledge`. It is git-ignored, so your
personal data is never committed. Fill in your profile under `knowledge/`: `profile.json`,
`resume.json`, `skills.json`, `preferences.json` and the other files.
Replace `company_profile.md`, `tone.md` and `approved_responses/*.md` with
your own content.

## 4. Pull a model and run

```bash
ollama pull qwen2.5:7b        # or qwen2.5:3b on machines with < 16 GB RAM
./target/release/emailhelper migrate
./target/release/emailhelper serve
```

Check that it's running:

```bash
curl http://127.0.0.1:7800/health
```

To try it without Ollama, set `MODEL_MODE=MOCK`.

## 5. Install the Thunderbird extension

```bash
cd thunderbird-extension && npm run build     # creates dist/emailhelper-thunderbird.xpi
```

In Thunderbird, go to **Add-ons and Themes → ⚙ → Install Add-on From File…**
and choose the `.xpi`. For development, use **about:debugging → Load
Temporary Add-on** with `thunderbird-extension/manifest.json`.

Open the extension's options and set the backend URL
(`http://127.0.0.1:7800`) and the API token. Then click **Test connection**.

## 6. Telegram (optional)

1. In Telegram, talk to **@BotFather**, send `/newbot` and copy the token.
2. Send any message to your new bot. Then open
   `https://api.telegram.org/bot<TOKEN>/getUpdates` and copy
   `message.chat.id`.
3. Set `TELEGRAM_ENABLED=true`, `TELEGRAM_BOT_TOKEN` and `TELEGRAM_CHAT_ID`
   in `.env`, then restart the agent. Send `/status` to the bot to check it.

## 7. Job agent and browser worker (optional)

```bash
cd browser-worker
npm ci
npx playwright install chromium
BROWSER_WORKER_TOKEN=<same as .env> npm start
```

Put your resume at `knowledge/resume.pdf` (or set `JOB_RESUME_PATH`). Then
set `JOB_AGENT_ENABLED=true` and `BROWSER_WORKER_TOKEN` in `.env`. Keep
`JOB_SUBMIT_MODE=review` until you trust the answers it produces.

To log in to sites the agent should use (LinkedIn, ATS accounts), start the
worker once with `BROWSER_HEADLESS=false` and log in manually. The session
is saved in `BROWSER_PROFILE_DIR`. See `browser-worker/README.md`.

## 8. Research (optional)

Run SearXNG with `docker compose --profile research up -d searxng` (see
`docker/README.md`), or point `SEARXNG_URL` at an existing instance with
JSON output enabled. Set `RESEARCH_ENABLED=true`, and optionally add board
slugs and feeds. To run a pass right away:

```bash
emailhelper discover-now
```

## 9. Run as a service

- **Windows:** `deploy/windows/install.ps1` registers a per-user
  scheduled task that starts at logon (recommended, because Thunderbird runs
  in your session). `-AsService` uses NSSM instead.
- **Linux:** `deploy/linux/install.sh` installs systemd units. For desktop
  use, a `systemctl --user` unit is in `deploy/linux/user/`.
- **Docker:** see `docker/README.md`. All ports are published on 127.0.0.1
  only.

## 10. Monitoring

```bash
docker compose -f docker/docker-compose.monitoring.yml --env-file .env up -d
```

Grafana runs on http://127.0.0.1:3000 with prebuilt dashboards. See
[MONITORING.md](MONITORING.md).
