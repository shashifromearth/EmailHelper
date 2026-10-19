# Real browser end-to-end test: starts the mock ATS sites and the Playwright
# worker, then runs rust-agent/tests/e2e_browser.rs against them.
# Requires: cargo, node, and `npm ci; npx playwright install chromium` in browser-worker\.
# Native tools (cargo, npm) write progress to stderr; Windows PowerShell 5.1
# would treat that as an error under 'Stop', so exit codes are checked instead.
$ErrorActionPreference = 'Continue'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $root
if (-not (Get-Command node -ErrorAction SilentlyContinue) -and (Test-Path 'C:\Program Files\nodejs\node.exe')) {
    $env:Path = 'C:\Program Files\nodejs;' + $env:Path
}

$mockPort = 18080
$workerPort = 17801
$token = 'e2e-worker-token'
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ('eh-e2e-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $tmp | Out-Null
$fixtures = Join-Path $root 'tests\mock-sites\fixtures'

$mock = Start-Process node -ArgumentList @('tests/mock-sites/server.mjs', '--port', $mockPort) -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput (Join-Path $tmp 'mock.log') -RedirectStandardError (Join-Path $tmp 'mock.err')
$env:BROWSER_WORKER_HOST = '127.0.0.1'
$env:BROWSER_WORKER_PORT = "$workerPort"
$env:BROWSER_WORKER_TOKEN = $token
$env:BROWSER_PROFILE_DIR = Join-Path $tmp 'profile'
$env:BROWSER_SCREENSHOT_DIR = Join-Path $tmp 'screenshots'
$env:BROWSER_ALLOWED_UPLOAD_DIRS = $fixtures
$env:BROWSER_HEADLESS = 'true'
$worker = Start-Process node -ArgumentList @('src/index.js') -WorkingDirectory (Join-Path $root 'browser-worker') -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput (Join-Path $tmp 'worker.log') -RedirectStandardError (Join-Path $tmp 'worker.err')

try {
    $deadline = (Get-Date).AddSeconds(30)
    do {
        Start-Sleep -Milliseconds 500
        try { $ok = (Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:$workerPort/health").StatusCode -eq 200 } catch { $ok = $false }
    } until ($ok -or (Get-Date) -gt $deadline)
    if (-not $ok) { throw "browser worker did not start; see $tmp\worker.err" }

    $env:EH_E2E_WORKER_URL = "http://127.0.0.1:$workerPort"
    $env:EH_E2E_MOCK_URL = "http://127.0.0.1:$mockPort"
    $env:EH_E2E_WORKER_TOKEN = $token
    $env:EH_E2E_RESUME = Join-Path $fixtures 'resume.pdf'
    cargo test --test e2e_browser -- --nocapture
    if ($LASTEXITCODE -ne 0) { throw "e2e test failed (logs in $tmp)" }
    Write-Host "E2E passed. Worker logs: $tmp" -ForegroundColor Green
} finally {
    Stop-Process -Id $worker.Id -Force -ErrorAction SilentlyContinue
    Stop-Process -Id $mock.Id -Force -ErrorAction SilentlyContinue
}
