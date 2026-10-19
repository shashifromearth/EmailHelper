# EmailHelper on Docker

```
docker/
├── Dockerfile.agent                 # Rust agent image (build context = repo root)
├── Dockerfile.agent.dockerignore    # allow-list for that build context (keeps .env out)
├── docker-compose.yml               # full stack, profiles below
├── docker-compose.monitoring.yml    # only Prometheus + Grafana, for a natively running agent
└── searxng/settings.yml             # SearXNG config: JSON output on, limiter off
```

The browser worker image is built from `browser-worker/Dockerfile`, which is
maintained with the worker.

## Services and profiles

| Service | Profile | Host port (loopback only) | In-network URL |
|---|---|---|---|
| `emailhelper` (agent) | *(always)* | `127.0.0.1:7800` | `http://emailhelper:7800` |
| `emailhelper-migrate` (one-shot `emailhelper migrate`) | *(always)* | none | none |
| `ollama` | *(always)* | `127.0.0.1:11434` | `http://ollama:11434` |
| `ollama-init` (one-shot `ollama pull $OLLAMA_MODEL`) | *(always)* | none | none |
| `browser-worker` | `jobs` | `127.0.0.1:7801` | `http://browser-worker:7801` |
| `searxng` | `research` | `127.0.0.1:8888` | `http://searxng:8080` |
| `prometheus` | `monitoring` | `127.0.0.1:9090` | `http://prometheus:9090` |
| `grafana` | `monitoring` | `127.0.0.1:3000` | none |

The core stack (agent and Ollama) starts without a profile. Optional parts are
opt-in so a plain email-drafting setup doesn't pull Chromium, SearXNG or the
monitoring images. When the browser worker or SearXNG isn't running, the agent
reports it as `down`/`disabled` in `/health` and carries on.

Every published port is bound to `127.0.0.1`. Containers listen on `0.0.0.0`
only inside the compose network. Don't change the `ports:` entries to
`"7800:7800"`, because that exposes the API on every interface. Note that
Docker-published ports bypass host firewalls such as ufw.

## First run

1. At the **repo root**, run `cp .env.example .env` (Windows:
   `Copy-Item .env.example .env`) and edit it:
   - `EH_API_TOKEN`: long random value, also entered in the Thunderbird extension
   - `BROWSER_WORKER_TOKEN`: long random value (only needed with the `jobs` profile)
   - `OLLAMA_MODEL`: defaults to `qwen2.5:7b`. Use `qwen2.5:3b` on small machines.
   - `GRAFANA_ADMIN_PASSWORD`: **must be changed** from the `change-me-now`
     fallback before the first `monitoring` start
   - `SEARXNG_SECRET`: random value if you use `research`
   - Telegram and cloud LLM keys as needed

   Generate tokens with `openssl rand -hex 32`, or in PowerShell:
   `-join ((1..32) | % { '{0:x2}' -f (Get-Random -Max 256) })`.
2. Put your files in `knowledge/` and `config/` at the repo root. They are
   mounted read-only at `/app/knowledge` and `/app/config`.
3. Start the stack from `docker/`:

   ```sh
   cd docker
   docker compose --env-file ../.env up -d --build                     # core
   docker compose --env-file ../.env --profile jobs --profile research up -d
   docker compose --env-file ../.env --profile "*" up -d               # everything
   ```

   `--env-file ../.env` matters. Compose reads `${VAR}` substitutions (model
   name, Grafana password, SearXNG secret) from `docker/.env` by default, while
   the containers get `../.env` through `env_file:`. Passing it keeps the two
   consistent. You can also run `export COMPOSE_ENV_FILES=../.env` once
   (PowerShell: `$env:COMPOSE_ENV_FILES='../.env'`).
4. Pulling the model takes a few minutes the first time. Follow it with
   `docker compose logs -f ollama-init`. To pull another model:
   `docker compose exec ollama ollama pull qwen2.5:3b`.
5. Check health:

   ```sh
   docker compose ps
   curl -s http://127.0.0.1:7800/health
   ```

The agent's `EH_BIND_ADDR`, data paths and service URLs (`OLLAMA_BASE_URL`,
`BROWSER_WORKER_URL`, `SEARXNG_URL`) are overridden in `docker-compose.yml`,
so the native values in `.env` (`127.0.0.1:...`, `./data`) don't need editing.

## GPU

Ollama runs on CPU by default. For NVIDIA, install the NVIDIA Container Toolkit
(Linux) or use Docker Desktop with the WSL2 backend and a current driver
(Windows). Then uncomment the `deploy.resources.reservations.devices` block
under `ollama` in `docker-compose.yml`. For AMD, use `ollama/ollama:rocm` and
pass `/dev/kfd` and `/dev/dri`. To check: `docker compose exec ollama ollama ps`
shows `GPU` in the processor column while a model is loaded.

## Volumes

| Volume | Mounted at | Contents |
|---|---|---|
| `emailhelper_data` | `/data` (agent) | SQLite DB, FTS index, agent state |
| `ollama_models` | `/root/.ollama` | downloaded models (several GB) |
| `browser_profile` | `/data/browser-profile` (worker) | persistent Chromium profile: cookies and logins |
| `browser_screenshots` | `/data/screenshots` (worker) | application screenshots |
| `searxng_cache` | `/var/cache/searxng` | SearXNG cache |
| `prometheus_data` | `/prometheus` | metrics, 30 days retention |
| `grafana_data` | `/var/lib/grafana` | Grafana DB (users, admin password) |

Volume names get the project prefix `emailhelper_`, for example
`emailhelper_emailhelper_data`. To back up the agent database:

```sh
docker compose stop emailhelper
docker run --rm -v emailhelper_emailhelper_data:/data -v "$PWD":/backup debian:bookworm-slim \
  tar czf /backup/emailhelper-data.tgz -C /data .
docker compose start emailhelper
```

`docker compose down` keeps volumes. `docker compose down -v` **deletes them
permanently**, including the DB, the browser logins and the models.

## Upgrading

```sh
cd docker
docker compose --env-file ../.env pull                 # ollama, searxng, prometheus, grafana
docker compose --env-file ../.env build --pull         # agent (+ worker with --profile jobs)
docker compose --env-file ../.env --profile "*" up -d  # migrate runs before the agent
docker image prune -f
```

`emailhelper-migrate` runs `emailhelper migrate` and must exit successfully
before `emailhelper` starts. Prometheus and Grafana versions are pinned in the
compose files. Bump them on purpose. `ollama` and `searxng` follow `:latest`.

## Monitoring with a native agent

On Windows, when the agent runs natively (`deploy/windows/install.ps1`), run
only the monitoring pair:

```sh
cd docker
docker compose -f docker-compose.monitoring.yml --env-file ../.env up -d
```

Prometheus scrapes `host.docker.internal:7800`. See `monitoring/README.md`.

## Notes and assumptions

- The agent image reads `EH_DATA_DIR=/data`, `EH_DB_PATH=/data/emailhelper.db`,
  `EH_KNOWLEDGE_DIR=/app/knowledge` and `EH_CONFIG_DIR=/app/config`. If the
  agent's config uses different variable names for the DB and config paths,
  adjust `x-agent-env` in `docker-compose.yml` and the `ENV` in
  `Dockerfile.agent`.
- The worker gets the same `/app/knowledge` mount as the agent, so file paths
  the agent sends, such as `/app/knowledge/resume.pdf`, resolve in the worker
  and pass its `BROWSER_ALLOWED_UPLOAD_DIRS` check.
- The worker container has `shm_size: 1gb` because Chromium crashes with the
  64 MB default. If the worker image runs as a non-root user, make sure that
  user can write `/data/browser-profile` and `/data/screenshots`.
- Building the agent needs BuildKit, which is the default in current Docker.
  The cargo registry and `target/` are cache mounts, so rebuilds are
  incremental. To build on its own from the repo root:
  `docker build -f docker/Dockerfile.agent -t emailhelper:local .`
