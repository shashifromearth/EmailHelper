#!/usr/bin/env bash
# Full QA run: format, lint, Rust tests, extension tests, browser-worker tests.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> cargo fmt --check"
cargo fmt --all -- --check

echo "==> cargo clippy"
cargo clippy --all-targets -- -D warnings

echo "==> cargo test"
cargo test --all-targets

if command -v node >/dev/null 2>&1; then
  echo "==> thunderbird-extension tests"
  (cd thunderbird-extension && npm test)
  if [ -d browser-worker/node_modules ]; then
    echo "==> browser-worker tests (Playwright)"
    (cd browser-worker && npm test)
  else
    echo "!! browser-worker/node_modules missing: run 'npm ci && npx playwright install chromium' in browser-worker/"
  fi
else
  echo "!! node not found: skipping extension and browser-worker tests"
fi
echo "All checks passed."
