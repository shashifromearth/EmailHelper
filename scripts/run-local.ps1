# Start the EmailHelper stack locally from .env:
#   browser worker (if JOB_AGENT_ENABLED or RECRUITER_LINKEDIN_ENABLED), optional
#   mock ATS sites, and the Rust agent. Logs go to data\logs, PIDs to data\run.
#
#   scripts\run-local.ps1                 # agent + worker
#   scripts\run-local.ps1 -WithMockSites  # also serve tests\mock-sites on :18080
#   scripts\stop-local.ps1                # stop everything
param(
    [switch]$WithMockSites,
    [int]$MockPort = 18080,
    [string]$Binary = ''
)
$ErrorActionPreference = 'Continue'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $root

$envFile = Join-Path $root '.env'
if (-not (Test-Path $envFile)) { throw ".env not found. Copy .env.example to .env first." }
foreach ($line in Get-Content $envFile) {
    if ($line -match '^\s*#' -or $line -notmatch '=') { continue }
    $k, $v = $line -split '=', 2
    $k = $k.Trim()
    if ($k) { [Environment]::SetEnvironmentVariable($k, $v.Trim(), 'Process') }
}

if (-not (Get-Command node -ErrorAction SilentlyContinue) -and (Test-Path 'C:\Program Files\nodejs\node.exe')) {
    $env:Path = 'C:\Program Files\nodejs;' + $env:Path
}
$logs = Join-Path $root 'data\logs'
$run = Join-Path $root 'data\run'
New-Item -ItemType Directory -Force $logs, $run | Out-Null

function Start-Component([string]$Name, [string]$File, [string[]]$ArgList, [string]$Dir) {
    $pidFile = Join-Path $run "$Name.pid"
    if (Test-Path $pidFile) {
        $old = Get-Process -Id (Get-Content $pidFile) -ErrorAction SilentlyContinue
        if ($null -ne $old) { Write-Host "$Name already running (pid $($old.Id))"; return }
    }
    $p = Start-Process $File -ArgumentList $ArgList -WorkingDirectory $Dir -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput (Join-Path $logs "$Name.log") -RedirectStandardError (Join-Path $logs "$Name.err.log")
    Set-Content $pidFile $p.Id
    Write-Host "started $Name (pid $($p.Id)) - logs: data\logs\$Name.log"
}

function Wait-Http([string]$Url, [int]$Seconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    do {
        try { if ((Invoke-WebRequest -UseBasicParsing $Url -TimeoutSec 2).StatusCode -eq 200) { return $true } } catch {}
        Start-Sleep -Milliseconds 300
    } until ((Get-Date) -gt $deadline)
    return $false
}

if ($WithMockSites) {
    Start-Component 'mock-sites' 'node' @('tests/mock-sites/server.mjs', '--port', "$MockPort") $root
    if (Wait-Http "http://127.0.0.1:$MockPort/greenhouse/job.html" 15) { Write-Host "mock sites: http://127.0.0.1:$MockPort/" }
}

if ($env:JOB_AGENT_ENABLED -eq 'true' -or $env:RECRUITER_LINKEDIN_ENABLED -eq 'true') {
    if (-not (Test-Path (Join-Path $root 'browser-worker\node_modules'))) {
        throw "browser-worker\node_modules missing: run 'npm ci; npx playwright install chromium' in browser-worker"
    }
    Start-Component 'browser-worker' 'node' @('src/index.js') (Join-Path $root 'browser-worker')
    $port = if ($env:BROWSER_WORKER_PORT) { $env:BROWSER_WORKER_PORT } else { '7801' }
    if (Wait-Http "http://127.0.0.1:$port/health" 20) { Write-Host "browser worker: http://127.0.0.1:$port/health" }
    else { Write-Warning "browser worker not healthy yet; see data\logs\browser-worker.err.log" }
}

if (-not $Binary) {
    $Binary = Join-Path $root 'target\release\emailhelper.exe'
    if (-not (Test-Path $Binary)) { $Binary = Join-Path $root 'target\debug\emailhelper.exe' }
}
if (-not (Test-Path $Binary)) { throw "emailhelper binary not found; run 'cargo build --release -p emailhelper'" }
Start-Component 'agent' $Binary @('serve') $root
$bind = if ($env:EH_BIND_ADDR) { $env:EH_BIND_ADDR } else { '127.0.0.1:7800' }
if (Wait-Http "http://$bind/readiness" 20) {
    Write-Host ""
    Write-Host "EmailHelper is running:" -ForegroundColor Green
    Write-Host "  health     http://$bind/health"
    Write-Host "  dashboard  http://$bind/dashboard"
    Write-Host "  metrics    http://$bind/metrics"
} else {
    Write-Warning "agent not ready; see data\logs\agent.log and data\logs\agent.err.log"
}
