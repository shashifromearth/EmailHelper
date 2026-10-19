#Requires -Version 5.1
<#
.SYNOPSIS
    Launcher used by the EmailHelper scheduled tasks.

.DESCRIPTION
    Runs a console program without a visible window (the task starts
    powershell.exe -WindowStyle Hidden, and the child shares that hidden
    console), writes stdout/stderr to logs\<LogName>.out.log / .err.log
    (previous run kept as .1), and restarts the program
    up to -MaxRestarts times with -RestartDelaySeconds between attempts when
    it exits with a non-zero code. A run that lasted longer than
    -HealthyRunSeconds resets the restart counter.

    Task Scheduler's own "restart on failure" setting only fires when the task
    fails to launch, not when the program exits non-zero, so the retry loop
    lives here. Exits with the program's last exit code.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$FilePath,
    [string[]]$ArgumentList = @(),
    [Parameter(Mandatory = $true)][string]$WorkingDirectory,
    [Parameter(Mandatory = $true)][string]$LogName,
    [int]$MaxRestarts = 3,
    [int]$RestartDelaySeconds = 60,
    [int]$HealthyRunSeconds = 300
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$logDir = Join-Path $WorkingDirectory 'logs'
if (-not (Test-Path -LiteralPath $logDir)) {
    New-Item -ItemType Directory -Path $logDir -Force | Out-Null
}
$outLog = Join-Path $logDir ($LogName + '.out.log')
$errLog = Join-Path $logDir ($LogName + '.err.log')

# Start-Process truncates redirect targets, so keep the previous run's log
# as <name>.1 before every start. The current log can be tailed live.
function Backup-Log([string]$Path) {
    if (Test-Path -LiteralPath $Path) {
        Move-Item -LiteralPath $Path -Destination ($Path + '.1') -Force
    }
}

function Write-LauncherLog([string]$Message) {
    $line = "[{0:u}] launcher: {1}" -f (Get-Date), $Message
    Add-Content -LiteralPath (Join-Path $logDir ($LogName + '.launcher.log')) -Value $line
}

$failures = 0
$exitCode = 0
while ($true) {
    Backup-Log $outLog
    Backup-Log $errLog
    $started = Get-Date
    $startArgs = @{
        FilePath               = $FilePath
        WorkingDirectory       = $WorkingDirectory
        NoNewWindow            = $true
        PassThru               = $true
        Wait                   = $true
        RedirectStandardOutput = $outLog
        RedirectStandardError  = $errLog
    }
    if ($ArgumentList.Count -gt 0) { $startArgs['ArgumentList'] = $ArgumentList }

    try {
        $proc = Start-Process @startArgs
        $exitCode = $proc.ExitCode
        if ($null -eq $exitCode) { $exitCode = 1 }
    } catch {
        Write-LauncherLog ("failed to start {0}: {1}" -f $FilePath, $_.Exception.Message)
        $exitCode = 1
    }

    $ranSeconds = ((Get-Date) - $started).TotalSeconds
    Write-LauncherLog ("{0} exited with code {1} after {2:N0}s" -f $LogName, $exitCode, $ranSeconds)

    if ($exitCode -eq 0) { break }
    if ($ranSeconds -ge $HealthyRunSeconds) { $failures = 0 }
    $failures++
    if ($failures -gt $MaxRestarts) {
        Write-LauncherLog ("giving up after {0} restarts" -f $MaxRestarts)
        break
    }
    Start-Sleep -Seconds $RestartDelaySeconds
}

exit $exitCode
