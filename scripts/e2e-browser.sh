#!/usr/bin/env bash
# Real browser end-to-end test: mock ATS sites + Playwright worker + Rust agent.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
mock_port=18080
worker_port=17801
token=e2e-worker-token
tmp="$(mktemp -d)"
fixtures="$root/tests/mock-sites/fixtures"

node tests/mock-sites/server.mjs --port "$mock_port" >"$tmp/mock.log" 2>&1 &
mock_pid=$!
(
  cd browser-worker
  BROWSER_WORKER_HOST=127.0.0.1 BROWSER_WORKER_PORT="$worker_port" BROWSER_WORKER_TOKEN="$token" \
  BROWSER_PROFILE_DIR="$tmp/profile" BROWSER_SCREENSHOT_DIR="$tmp/screenshots" \
  BROWSER_ALLOWED_UPLOAD_DIRS="$fixtures" BROWSER_HEADLESS=true \
  exec node src/index.js
) >"$tmp/worker.log" 2>&1 &
worker_pid=$!
trap 'kill $worker_pid $mock_pid 2>/dev/null || true' EXIT

for _ in $(seq 1 60); do
  curl -fs "http://127.0.0.1:$worker_port/health" >/dev/null && break
  sleep 0.5
done

EH_E2E_WORKER_URL="http://127.0.0.1:$worker_port" EH_E2E_MOCK_URL="http://127.0.0.1:$mock_port" \
EH_E2E_WORKER_TOKEN="$token" EH_E2E_RESUME="$fixtures/resume.pdf" \
  cargo test --test e2e_browser -- --nocapture
echo "E2E passed. Logs: $tmp"
