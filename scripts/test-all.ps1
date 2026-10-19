# Full QA run on Windows PowerShell 5.1+: format, lint, Rust tests,
# Thunderbird extension tests and browser-worker (Playwright) tests.
# Native tools (cargo, npm) write progress to stderr; Windows PowerShell 5.1
# would treat that as an error under 'Stop', so exit codes are checked instead.
$ErrorActionPreference = 'Continue'
Set-Location (Join-Path $PSScriptRoot '..')

function Invoke-Step([string]$Name, [scriptblock]$Block) {
    Write-Host "==> $Name" -ForegroundColor Cyan
    & $Block
    if ($LASTEXITCODE -ne 0) { throw "$Name failed (exit $LASTEXITCODE)" }
}

Invoke-Step 'cargo fmt --check' { cargo fmt --all -- --check }
Invoke-Step 'cargo clippy' { cargo clippy --all-targets -- -D warnings }
Invoke-Step 'cargo test' { cargo test --all-targets }

$node = Get-Command node -ErrorAction SilentlyContinue
if ($null -eq $node -and (Test-Path 'C:\Program Files\nodejs\node.exe')) {
    $env:Path = 'C:\Program Files\nodejs;' + $env:Path
    $node = Get-Command node -ErrorAction SilentlyContinue
}
if ($null -ne $node) {
    Push-Location thunderbird-extension
    try { Invoke-Step 'thunderbird-extension tests' { npm test } } finally { Pop-Location }
    if (Test-Path 'browser-worker\node_modules') {
        Push-Location browser-worker
        try { Invoke-Step 'browser-worker tests (Playwright)' { npm test } } finally { Pop-Location }
    } else {
        Write-Warning "browser-worker\node_modules missing: run 'npm ci; npx playwright install chromium' in browser-worker\"
    }
} else {
    Write-Warning 'node not found: skipping extension and browser-worker tests'
}
Write-Host 'All checks passed.' -ForegroundColor Green
