#!/usr/bin/env bash
# EmailHelper system-wide installer for Linux (systemd).
#
#   cargo build --release -p emailhelper
#   sudo deploy/linux/install.sh [--with-browser-worker] [--no-start] [--binary PATH]
#
# Idempotent: safe to re-run for upgrades. Existing configuration, knowledge
# files and data are never overwritten; the binary and unit files are.
#
# Layout:
#   /usr/local/bin/emailhelper            binary
#   /etc/emailhelper/emailhelper.env      environment (secrets; root:emailhelper 0640)
#   /etc/emailhelper/knowledge/           knowledge files (read-only for the service)
#   /etc/emailhelper/config/              config templates
#   /var/lib/emailhelper/                 database, browser profile, screenshots
#   /opt/emailhelper/browser-worker/      browser worker (optional)

set -euo pipefail

readonly SERVICE_USER="emailhelper"
readonly BIN_DIR="/usr/local/bin"
readonly ETC_DIR="/etc/emailhelper"
readonly DATA_DIR="/var/lib/emailhelper"
readonly OPT_DIR="/opt/emailhelper"
readonly UNIT_DIR="/etc/systemd/system"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
readonly SCRIPT_DIR REPO_ROOT

BINARY="${REPO_ROOT}/target/release/emailhelper"
WITH_WORKER=0
START=1

log()  { printf '==> %s\n' "$*"; }
warn() { printf 'WARNING: %s\n' "$*" >&2; }
die()  { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Usage: sudo install.sh [options]

  --binary PATH           emailhelper binary (default: <repo>/target/release/emailhelper)
  --with-browser-worker   also install the Node/Playwright browser worker + unit
  --no-start              enable units but do not (re)start them
  -h, --help              show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary)
      [[ $# -ge 2 ]] || die "--binary needs a path"
      BINARY="$2"
      shift 2
      ;;
    --with-browser-worker) WITH_WORKER=1; shift ;;
    --no-start) START=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown option: $1" ;;
  esac
done

[[ ${EUID} -eq 0 ]] || die "run as root (sudo)"
command -v systemctl >/dev/null 2>&1 || die "systemd (systemctl) is required"
[[ -x "${BINARY}" ]] || die "binary not found or not executable: ${BINARY} (run: cargo build --release -p emailhelper)"

# --- service account ---------------------------------------------------------
if ! getent group "${SERVICE_USER}" >/dev/null; then
  log "creating group ${SERVICE_USER}"
  groupadd --system "${SERVICE_USER}"
fi
if ! getent passwd "${SERVICE_USER}" >/dev/null; then
  log "creating user ${SERVICE_USER}"
  nologin_shell="$(command -v nologin || echo /usr/sbin/nologin)"
  useradd --system --gid "${SERVICE_USER}" --home-dir "${DATA_DIR}" \
          --no-create-home --shell "${nologin_shell}" \
          --comment "EmailHelper service" "${SERVICE_USER}"
fi

# --- directories -------------------------------------------------------------
log "creating directories"
install -d -m 0750 -o root -g "${SERVICE_USER}" "${ETC_DIR}"
install -d -m 0750 -o "${SERVICE_USER}" -g "${SERVICE_USER}" "${DATA_DIR}"
install -d -m 0750 -o "${SERVICE_USER}" -g "${SERVICE_USER}" \
  "${DATA_DIR}/browser-profile" "${DATA_DIR}/screenshots"

# --- binary (always replaced: this is how upgrades happen) --------------------
log "installing ${BIN_DIR}/emailhelper"
install -m 0755 -o root -g root "${BINARY}" "${BIN_DIR}/emailhelper.new"
mv -f "${BIN_DIR}/emailhelper.new" "${BIN_DIR}/emailhelper"

# --- knowledge / config templates (only when absent) --------------------------
copy_tree_if_absent() {
  local src="$1" dst="$2"
  if [[ -e "${dst}" ]]; then
    log "keeping existing ${dst}"
    return 0
  fi
  if [[ ! -d "${src}" ]]; then
    warn "${src} not found; creating empty ${dst}"
    install -d -m 0750 -o root -g "${SERVICE_USER}" "${dst}"
    return 0
  fi
  log "copying ${src} -> ${dst}"
  cp -R "${src}" "${dst}"
  chown -R root:"${SERVICE_USER}" "${dst}"
  find "${dst}" -type d -exec chmod 0750 {} +
  find "${dst}" -type f -exec chmod 0640 {} +
}
copy_tree_if_absent "${REPO_ROOT}/knowledge" "${ETC_DIR}/knowledge"
copy_tree_if_absent "${REPO_ROOT}/config" "${ETC_DIR}/config"

# --- environment file (only when absent) --------------------------------------
ENV_FILE="${ETC_DIR}/emailhelper.env"
if [[ ! -e "${ENV_FILE}" ]]; then
  if [[ -f "${REPO_ROOT}/.env.example" ]]; then
    log "creating ${ENV_FILE} from .env.example"
    install -m 0640 -o root -g "${SERVICE_USER}" "${REPO_ROOT}/.env.example" "${ENV_FILE}"
  else
    warn ".env.example not found; creating an empty ${ENV_FILE}"
    install -m 0640 -o root -g "${SERVICE_USER}" /dev/null "${ENV_FILE}"
  fi
  # Later assignments win, so these override relative paths from .env.example.
  cat >>"${ENV_FILE}" <<EOF

# --- set by deploy/linux/install.sh (system service paths) ---
EH_BIND_ADDR=127.0.0.1:7800
EH_DATA_DIR=${DATA_DIR}
EH_DB_PATH=${DATA_DIR}/emailhelper.db
EH_KNOWLEDGE_DIR=${ETC_DIR}/knowledge
EH_CONFIG_DIR=${ETC_DIR}/config
BROWSER_WORKER_HOST=127.0.0.1
BROWSER_PROFILE_DIR=${DATA_DIR}/browser-profile
BROWSER_SCREENSHOT_DIR=${DATA_DIR}/screenshots
BROWSER_ALLOWED_UPLOAD_DIRS="${ETC_DIR}/knowledge;${DATA_DIR}"
EOF
  NEW_ENV=1
else
  log "keeping existing ${ENV_FILE}"
  NEW_ENV=0
fi

for key in EH_API_TOKEN BROWSER_WORKER_TOKEN; do
  if ! grep -Eq "^${key}=.+" "${ENV_FILE}"; then
    warn "${key} is empty in ${ENV_FILE}; set it (e.g. openssl rand -hex 32)"
  fi
done

# --- browser worker (optional) ------------------------------------------------
if [[ ${WITH_WORKER} -eq 1 ]]; then
  [[ -d "${REPO_ROOT}/browser-worker" ]] || die "browser-worker/ not found in ${REPO_ROOT}"
  command -v node >/dev/null 2>&1 || die "node (>= 18) is required for the browser worker"
  command -v npm >/dev/null 2>&1 || die "npm is required for the browser worker"

  log "installing browser worker to ${OPT_DIR}/browser-worker"
  install -d -m 0755 -o root -g root "${OPT_DIR}" "${OPT_DIR}/browser-worker"
  # Copy sources but not local node_modules / data from the dev checkout.
  (cd "${REPO_ROOT}/browser-worker" && tar --exclude=./node_modules --exclude=./data -cf - .) \
    | tar -C "${OPT_DIR}/browser-worker" -xf -
  chown -R root:root "${OPT_DIR}/browser-worker"

  (
    cd "${OPT_DIR}/browser-worker"
    if [[ -f package-lock.json ]]; then
      npm ci --omit=dev
    else
      npm install --omit=dev
    fi
    PLAYWRIGHT_BROWSERS_PATH="${OPT_DIR}/ms-playwright" npx --yes playwright install chromium
  )
  chmod -R a+rX "${OPT_DIR}"
  log "if Chromium fails to start, install its system libraries:"
  log "  cd ${OPT_DIR}/browser-worker && sudo npx playwright install-deps chromium"
fi

# --- systemd units -----------------------------------------------------------
log "installing systemd units"
install -m 0644 -o root -g root "${SCRIPT_DIR}/emailhelper.service" "${UNIT_DIR}/emailhelper.service"
UNITS=(emailhelper.service)
if [[ ${WITH_WORKER} -eq 1 ]]; then
  install -m 0644 -o root -g root "${SCRIPT_DIR}/emailhelper-browser-worker.service" \
    "${UNIT_DIR}/emailhelper-browser-worker.service"
  UNITS=(emailhelper-browser-worker.service "${UNITS[@]}")
fi
systemctl daemon-reload
systemctl enable "${UNITS[@]}"

if [[ ${START} -eq 1 ]]; then
  if [[ ${NEW_ENV} -eq 1 ]]; then
    warn "new ${ENV_FILE} created: edit it, then run: systemctl restart ${UNITS[*]}"
  else
    log "restarting ${UNITS[*]}"
    systemctl restart "${UNITS[@]}"
    systemctl --no-pager --lines=0 status "${UNITS[@]}" || true
  fi
fi

log "done. Health: curl -s http://127.0.0.1:7800/health"
log "Logs:   journalctl -u emailhelper -f"
