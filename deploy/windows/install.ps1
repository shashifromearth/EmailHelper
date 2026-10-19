#Requires -Version 5.1
<#
.SYNOPSIS
    Installs EmailHelper for the current Windows user.

.DESCRIPTION
    Copies target\release\emailhelper.exe, the knowledge\ and config\
    templates and the hidden-window launcher to %LOCALAPPDATA%\EmailHelper,
    creates .env from .env.example when missing, and registers a per-user
    scheduled task "EmailHelper Agent" (at logon, hidden, restarts on failure
    3 times, 1 minute apart). With -WithBrowserWorker it also installs the
    Node/Playwright browser worker and registers "EmailHelper Browser Worker".

    Re-running the script upgrades the binary/launcher in place. Existing
    .env, knowledge, config and data are never overwritten.

    -AsService installs real Windows services through NSSM instead (nssm.exe
    must be on PATH, needs an elevated shell). Services run in session 0
    without a desktop and as a service account, so they cannot show a headful
    browser and do not run as you: the scheduled-task mode is recommended for
    desktop use alongside Thunderbird.

.EXAMPLE
    cargo build --release -p emailhelper
    powershell -ExecutionPolicy Bypass -File deploy\windows\install.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File deploy\windows\install.ps1 -WithBrowserWorker

.EXAMPLE
    # elevated
    powershell -ExecutionPolicy Bypass -File deploy\windows\install.ps1 -AsService
#>
[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'EmailHelper'),
    [string]$SourceRoot = '',
    [switch]$WithBrowserWorker,
    [switch]$SkipNpmInstall,
    [switch]$AsService,
    [string]$ServiceAccount = 'NT AUTHORITY\LocalService',
    [switch]$NoStart
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$AgentTaskName = 'EmailHelper Agent'
$WorkerTaskName = 'EmailHelper Browser Worker'
$AgentServiceName = 'EmailHelper'
$WorkerServiceName = 'EmailHelperBrowserWorker'

function Write-Step([string]$Message) { Write-Host "==> $Message" -ForegroundColor Cyan }
function Write-Warn([string]$Message) { Write-Warning $Message }

function Test-IsAdmin {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

# Copies files from $Source to $Destination, skipping files that already
# exist at the destination (user edits are preserved).
function Copy-MissingFiles([string]$Source, [string]$Destination) {
    if (-not (Test-Path -LiteralPath $Source)) {
        Write-Warn "$Source not found; creating empty $Destination"
        New-Item -ItemType Directory -Path $Destination -Force | Out-Null
        return
    }
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
    $srcFull = (Resolve-Path -LiteralPath $Source).ProviderPath.TrimEnd('\')
    $copied = 0
    foreach ($item in Get-ChildItem -LiteralPath $srcFull -Recurse -Force) {
        $relative = $item.FullName.Substring($srcFull.Length).TrimStart('\')
        $target = Join-Path $Destination $relative
        if ($item.PSIsContainer) {
            if (-not (Test-Path -LiteralPath $target)) {
                New-Item -ItemType Directory -Path $target -Force | Out-Null
            }
        } elseif (-not (Test-Path -LiteralPath $target)) {
            Copy-Item -LiteralPath $item.FullName -Destination $target
            $copied++
        }
    }
    Write-Step "$Destination : $copied new file(s) copied"
}

# Copies a source tree, overwriting existing files but skipping the given
# top-level directory names (node_modules, data, ...).
function Copy-Tree([string]$Source, [string]$Destination, [string[]]$Exclude) {
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
    foreach ($item in Get-ChildItem -LiteralPath $Source -Force) {
        if ($Exclude -contains $item.Name) { continue }
        Copy-Item -LiteralPath $item.FullName -Destination $Destination -Recurse -Force
    }
}

function Get-Nssm {
    $cmd = Get-Command 'nssm.exe' -ErrorAction SilentlyContinue
    if ($null -eq $cmd) { return $null }
    return $cmd.Source
}

function Invoke-Nssm([string]$Nssm, [string[]]$Arguments) {
    & $Nssm @Arguments | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "nssm $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
}

# Runs `emailhelper migrate` with the install dir as working directory so
# relative paths from .env (./data, ./knowledge) resolve like they do at runtime.
function Invoke-Migrate {
    Push-Location $InstallDir
    try {
        & (Join-Path $InstallDir 'emailhelper.exe') migrate
        if ($LASTEXITCODE -ne 0) { Write-Warn "emailhelper migrate exited with $LASTEXITCODE" }
    } finally {
        Pop-Location
    }
}

function Stop-Existing {
    foreach ($name in @($AgentTaskName, $WorkerTaskName)) {
        $task = Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
        if ($null -ne $task) {
            Write-Step "stopping scheduled task '$name'"
            Stop-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
        }
    }
    foreach ($name in @($AgentServiceName, $WorkerServiceName)) {
        $svc = Get-Service -Name $name -ErrorAction SilentlyContinue
        if (($null -ne $svc) -and ($svc.Status -ne 'Stopped')) {
            Write-Step "stopping service '$name'"
            Stop-Service -Name $name -Force -ErrorAction SilentlyContinue
        }
    }
    # The exe is locked while running; make sure no copy from $InstallDir is left.
    $exePath = Join-Path $InstallDir 'emailhelper.exe'
    foreach ($proc in Get-Process -Name 'emailhelper' -ErrorAction SilentlyContinue) {
        $procPath = $null
        try { $procPath = $proc.Path } catch { $procPath = $null }
        if (($null -ne $procPath) -and ($procPath -eq $exePath)) {
            Write-Step "stopping emailhelper.exe (pid $($proc.Id))"
            Stop-Process -Id $proc.Id -Force
        }
    }
    Start-Sleep -Seconds 1
}

function Register-HiddenTask([string]$TaskName, [string]$Description, [string]$Program,
                             [string]$ProgramArgs, [string]$LogName) {
    $launcher = Join-Path $InstallDir 'run-hidden.ps1'
    $psArgs = '-NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass ' +
              ('-File "{0}" -FilePath "{1}" -WorkingDirectory "{2}" -LogName {3} -MaxRestarts 3 -RestartDelaySeconds 60' -f
                  $launcher, $Program, $InstallDir, $LogName)
    if ($ProgramArgs) { $psArgs += (' -ArgumentList {0}' -f $ProgramArgs) }

    $userId = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument $psArgs -WorkingDirectory $InstallDir
    $trigger = New-ScheduledTaskTrigger -AtLogOn -User $userId
    $principal = New-ScheduledTaskPrincipal -UserId $userId -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries `
        -DontStopIfGoingOnBatteries `
        -StartWhenAvailable `
        -Hidden `
        -MultipleInstances IgnoreNew `
        -ExecutionTimeLimit ([TimeSpan]::Zero) `
        -RestartCount 3 `
        -RestartInterval (New-TimeSpan -Minutes 1)

    Register-ScheduledTask -TaskName $TaskName -Description $Description -Action $action `
        -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
    Write-Step "registered scheduled task '$TaskName' (runs at logon as $userId)"
}

function Install-NssmService([string]$Nssm, [string]$Name, [string]$DisplayName, [string]$Description,
                             [string]$Program, [string]$ProgramArgs, [string]$LogName) {
    $existing = Get-Service -Name $Name -ErrorAction SilentlyContinue
    if ($null -eq $existing) {
        Invoke-Nssm $Nssm @('install', $Name, $Program)
    } else {
        Invoke-Nssm $Nssm @('set', $Name, 'Application', $Program)
    }
    $logDir = Join-Path $InstallDir 'logs'
    # PS 5.1 drops empty-string arguments to native programs, so only set
    # AppParameters when there is something to pass.
    if ($ProgramArgs) { Invoke-Nssm $Nssm @('set', $Name, 'AppParameters', $ProgramArgs) }
    Invoke-Nssm $Nssm @('set', $Name, 'AppDirectory', $InstallDir)
    Invoke-Nssm $Nssm @('set', $Name, 'DisplayName', $DisplayName)
    Invoke-Nssm $Nssm @('set', $Name, 'Description', $Description)
    Invoke-Nssm $Nssm @('set', $Name, 'Start', 'SERVICE_AUTO_START')
    Invoke-Nssm $Nssm @('set', $Name, 'ObjectName', $ServiceAccount)
    Invoke-Nssm $Nssm @('set', $Name, 'AppStdout', (Join-Path $logDir ($LogName + '.out.log')))
    Invoke-Nssm $Nssm @('set', $Name, 'AppStderr', (Join-Path $logDir ($LogName + '.err.log')))
    Invoke-Nssm $Nssm @('set', $Name, 'AppRotateFiles', '1')
    Invoke-Nssm $Nssm @('set', $Name, 'AppRotateOnline', '1')
    Invoke-Nssm $Nssm @('set', $Name, 'AppRotateBytes', '10485760')
    Invoke-Nssm $Nssm @('set', $Name, 'AppExit', 'Default', 'Restart')
    Invoke-Nssm $Nssm @('set', $Name, 'AppRestartDelay', '60000')
    Invoke-Nssm $Nssm @('set', $Name, 'AppStopMethodConsole', '15000')
    Write-Step "installed service '$Name' (account: $ServiceAccount)"
}

# ---------------------------------------------------------------- checks ---
if (-not $SourceRoot) {
    $SourceRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).ProviderPath
}
$sourceExe = Join-Path $SourceRoot 'target\release\emailhelper.exe'
if (-not (Test-Path -LiteralPath $sourceExe)) {
    throw "Binary not found: $sourceExe. Build it first: cargo build --release -p emailhelper"
}

$nssm = $null
if ($AsService) {
    if (-not (Test-IsAdmin)) { throw '-AsService needs an elevated PowerShell (Run as administrator).' }
    $nssm = Get-Nssm
    if ($null -eq $nssm) { throw '-AsService needs nssm.exe on PATH (https://nssm.cc). Without it, omit -AsService to use scheduled tasks.' }
}

$node = $null
if ($WithBrowserWorker) {
    $nodeCmd = Get-Command 'node.exe' -ErrorAction SilentlyContinue
    if ($null -eq $nodeCmd) { throw '-WithBrowserWorker needs Node.js (>= 18) on PATH.' }
    $node = $nodeCmd.Source
    if (-not (Test-Path -LiteralPath (Join-Path $SourceRoot 'browser-worker\src\index.js'))) {
        throw "browser-worker\src\index.js not found under $SourceRoot"
    }
}

# ------------------------------------------------------------------ files ---
Write-Step "installing to $InstallDir"
foreach ($dir in @($InstallDir, (Join-Path $InstallDir 'data'), (Join-Path $InstallDir 'logs'))) {
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
}

Stop-Existing

Copy-Item -LiteralPath $sourceExe -Destination (Join-Path $InstallDir 'emailhelper.exe') -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'run-hidden.ps1') -Destination (Join-Path $InstallDir 'run-hidden.ps1') -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'uninstall.ps1') -Destination (Join-Path $InstallDir 'uninstall.ps1') -Force
Write-Step 'copied emailhelper.exe, run-hidden.ps1 and uninstall.ps1'

Copy-MissingFiles (Join-Path $SourceRoot 'knowledge') (Join-Path $InstallDir 'knowledge')
Copy-MissingFiles (Join-Path $SourceRoot 'config') (Join-Path $InstallDir 'config')

$envFile = Join-Path $InstallDir '.env'
$newEnv = $false
if (-not (Test-Path -LiteralPath $envFile)) {
    $example = Join-Path $SourceRoot '.env.example'
    if (Test-Path -LiteralPath $example) {
        Copy-Item -LiteralPath $example -Destination $envFile
        Write-Step "created $envFile from .env.example"
    } else {
        New-Item -ItemType File -Path $envFile | Out-Null
        Write-Warn ".env.example not found; created an empty $envFile"
    }
    $newEnv = $true
} else {
    Write-Step "keeping existing $envFile"
}
foreach ($key in @('EH_API_TOKEN', 'BROWSER_WORKER_TOKEN')) {
    $hasValue = Select-String -LiteralPath $envFile -Pattern ('^' + $key + '=.+') -Quiet
    if (-not $hasValue) { Write-Warn "$key is empty in $envFile - set a long random value." }
}

if ($WithBrowserWorker) {
    $workerDir = Join-Path $InstallDir 'browser-worker'
    Write-Step "installing browser worker to $workerDir"
    Copy-Tree (Join-Path $SourceRoot 'browser-worker') $workerDir @('node_modules', 'data', '.env')
    if (-not $SkipNpmInstall) {
        $npm = Get-Command 'npm.cmd' -ErrorAction SilentlyContinue
        if ($null -eq $npm) { throw 'npm.cmd not found on PATH (use -SkipNpmInstall to skip).' }
        Push-Location $workerDir
        try {
            if (Test-Path -LiteralPath (Join-Path $workerDir 'package-lock.json')) {
                & $npm.Source ci --omit=dev
            } else {
                & $npm.Source install --omit=dev
            }
            if ($LASTEXITCODE -ne 0) { throw "npm install failed ($LASTEXITCODE)" }
            $npx = Join-Path (Split-Path $npm.Source -Parent) 'npx.cmd'
            & $npx --yes playwright install chromium
            if ($LASTEXITCODE -ne 0) { throw "playwright install chromium failed ($LASTEXITCODE)" }
        } finally {
            Pop-Location
        }
    }
}

# --------------------------------------------------------- registration ---
$exe = Join-Path $InstallDir 'emailhelper.exe'
# Relative to the working directory (= $InstallDir) to avoid quoting issues.
$workerScript = 'browser-worker\src\index.js'

if ($AsService) {
    Write-Warn 'Services run in session 0 under a service account: no desktop, no headful browser, separate profile. Scheduled-task mode is recommended for desktop use.'
    # Let the service account read/write the install dir.
    & icacls.exe $InstallDir /grant ('{0}:(OI)(CI)M' -f $ServiceAccount) /T /Q | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "icacls failed to grant $ServiceAccount access to $InstallDir" }

    if (-not $newEnv) { Invoke-Migrate }


    Install-NssmService $nssm $AgentServiceName 'EmailHelper Agent' 'EmailHelper orchestrator (127.0.0.1:7800)' $exe 'serve' 'agent'
    if ($WithBrowserWorker) {
        Install-NssmService $nssm $WorkerServiceName 'EmailHelper Browser Worker' 'EmailHelper Playwright worker (127.0.0.1:7801)' $node $workerScript 'browser-worker'
    }
    if (-not $NoStart) {
        if ($newEnv) {
            Write-Warn "Edit $envFile first, then: Start-Service $AgentServiceName"
        } else {
            if ($WithBrowserWorker) { Start-Service -Name $WorkerServiceName }
            Start-Service -Name $AgentServiceName
            Write-Step 'services started'
        }
    }
} else {
    Register-HiddenTask $AgentTaskName 'EmailHelper orchestrator (127.0.0.1:7800)' $exe 'serve' 'agent'
    if ($WithBrowserWorker) {
        Register-HiddenTask $WorkerTaskName 'EmailHelper Playwright browser worker (127.0.0.1:7801)' $node $workerScript 'browser-worker'
    }
    if (-not $NoStart) {
        if ($newEnv) {
            Write-Warn "Edit $envFile first, then: Start-ScheduledTask -TaskName '$AgentTaskName'"
        } else {
            Invoke-Migrate

            if ($WithBrowserWorker) { Start-ScheduledTask -TaskName $WorkerTaskName }
            Start-ScheduledTask -TaskName $AgentTaskName
            Write-Step 'scheduled tasks started'
        }
    }
}

Write-Host ''
Write-Host "Installed to $InstallDir"
Write-Host '  Health : Invoke-RestMethod http://127.0.0.1:7800/health'
Write-Host "  Logs   : $(Join-Path $InstallDir 'logs')"
Write-Host "  Remove : powershell -ExecutionPolicy Bypass -File `"$(Join-Path $InstallDir 'uninstall.ps1')`""
