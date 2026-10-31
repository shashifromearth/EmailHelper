# One command to (re)start EmailHelper:
#   1. rebuilds the agent when its source changed (or with -Rebuild),
#   2. makes sure Ollama (local fallback model) is running, if installed,
#   3. stops any running copy and starts the browser worker, job-links + agent,
#   4. prints the health check.
#
#   run.cmd                   # from the repo root (or scripts\start.ps1)
#   run.cmd -Rebuild          # force a release build
#   run.cmd -NoBuild          # never build; use data\bin\emailhelper.exe as is
#   stop.cmd                  # stop everything
param(
    [switch]$Rebuild,
    [switch]$NoBuild,
    [switch]$WithMockSites
)
$ErrorActionPreference = 'Continue'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $root

# Toolchains installed for this project live outside PATH by default.
foreach ($dir in @(
        (Join-Path $env:USERPROFILE '.cargo\bin'),
        $env:EH_GCC_DIR,
        (Join-Path $env:USERPROFILE 'tools\winlibs\mingw64\bin'),
        'C:\Program Files\nodejs')) {
    if ($dir -and (Test-Path $dir) -and ($env:Path -notlike "*$dir*")) { $env:Path = "$dir;$env:Path" }
}

$bin = Join-Path $root 'data\bin\emailhelper.exe'
$linksBin = Join-Path $root 'data\bin\job-links.exe'
New-Item -ItemType Directory -Force (Split-Path $bin) | Out-Null

# 1. Build when needed.
$ksBin = Join-Path (Split-Path $bin) 'knowledge-service.exe'
$binaries = @($bin, $ksBin, $linksBin)
$needBuild = $Rebuild -or ($binaries | Where-Object { -not (Test-Path $_) })
if (-not $needBuild -and -not $NoBuild) {
    $built = $binaries | ForEach-Object { (Get-Item $_).LastWriteTime } | Sort-Object | Select-Object -First 1
    $newer = Get-ChildItem -Recurse -File (Join-Path $root 'rust-agent\src'), (Join-Path $root 'rust-agent\Cargo.toml'),
            (Join-Path $root 'knowledge-core\src'), (Join-Path $root 'knowledge-service\src'), (Join-Path $root 'job-links\src') |
        Where-Object { $_.LastWriteTime -gt $built } | Select-Object -First 1
    $needBuild = $null -ne $newer
}
if ($needBuild -and -not $NoBuild) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Write-Host 'cargo not found: install Rust (see documents\INSTALL.md) or use -NoBuild.' -ForegroundColor Red
        exit 1
    }
    Write-Host 'building the agent and services (release)... this can take a few minutes'
    cargo build --release -p emailhelper -p knowledge-service -p job-links
    if ($LASTEXITCODE -ne 0) { Write-Host 'build failed; nothing was restarted.' -ForegroundColor Red; exit 1 }
}

# 2. Local fallback model.
if (Get-Command ollama -ErrorAction SilentlyContinue) {
    $up = $false
    try { $up = (Invoke-WebRequest -UseBasicParsing 'http://127.0.0.1:11434/api/version' -TimeoutSec 3).StatusCode -eq 200 } catch {}
    if (-not $up) {
        Start-Process ollama -ArgumentList 'serve' -WindowStyle Hidden
        Write-Host 'started Ollama'
    }
}

# 3. Restart (the running binary is locked, so stop before replacing it).
& (Join-Path $PSScriptRoot 'stop-local.ps1') | Out-Null
# Copy each freshly built binary over the running copy; Windows may hold a
# just-stopped program's file for a moment, so retry briefly.
foreach ($dst in $binaries) {
    $src = Join-Path $root "target\release\$(Split-Path $dst -Leaf)"
    if (-not (Test-Path $src)) { continue }
    if ((Test-Path $dst) -and (Get-Item $src).LastWriteTime -le (Get-Item $dst).LastWriteTime) { continue }
    for ($i = 0; $i -lt 20; $i++) {
        try { Copy-Item $src $dst -Force -ErrorAction Stop; break }
        catch { Start-Sleep -Milliseconds 500 }
    }
    if ((Get-Item $src).LastWriteTime -gt (Get-Item $dst).LastWriteTime) { Write-Host "could not replace $dst (still in use)" -ForegroundColor Yellow }
}
$runArgs = @{ Binary = $bin }
if (Test-Path $linksBin) { $runArgs.LinksBinary = $linksBin }
if ($WithMockSites) { $runArgs.WithMockSites = $true }
& (Join-Path $PSScriptRoot 'run-local.ps1') @runArgs

# 4. Health.
Start-Sleep -Seconds 3
try {
    $h = Invoke-RestMethod 'http://127.0.0.1:7800/health' -TimeoutSec 5
    Write-Host ("health: {0} (model {1}, telegram {2}, browser worker {3})" -f $h.status, $h.checks.llm_provider, $h.checks.telegram, $h.checks.browser_worker) -ForegroundColor Green
} catch {
    Write-Host 'the agent did not answer on http://127.0.0.1:7800/health; see data\logs\agent.log' -ForegroundColor Yellow
}
