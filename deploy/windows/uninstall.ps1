#Requires -Version 5.1
<#
.SYNOPSIS
    Removes the EmailHelper scheduled tasks / services and program files.

.DESCRIPTION
    Stops and unregisters the "EmailHelper Agent" and "EmailHelper Browser
    Worker" scheduled tasks and, if present, the NSSM services "EmailHelper"
    and "EmailHelperBrowserWorker" (removing services needs an elevated shell).
    Then deletes the program files (emailhelper.exe, launcher, browser worker
    code).

    Your data is kept by default: .env, knowledge\, config\, data\ and logs\
    stay in the install directory. -RemoveData deletes the whole install
    directory, including the database and browser profile (asks first unless
    -Confirm:$false is given).

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\EmailHelper\uninstall.ps1"

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File deploy\windows\uninstall.ps1 -RemoveData
#>
[CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = 'Medium')]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'EmailHelper'),
    [switch]$RemoveData
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$taskNames = @('EmailHelper Agent', 'EmailHelper Browser Worker')
$serviceNames = @('EmailHelper', 'EmailHelperBrowserWorker')

function Write-Step([string]$Message) { Write-Host "==> $Message" -ForegroundColor Cyan }

function Test-IsAdmin {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

# --- scheduled tasks ---------------------------------------------------------
foreach ($name in $taskNames) {
    $task = Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
    if ($null -ne $task) {
        if ($PSCmdlet.ShouldProcess($name, 'Stop and unregister scheduled task')) {
            Stop-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
            Unregister-ScheduledTask -TaskName $name -Confirm:$false
            Write-Step "removed scheduled task '$name'"
        }
    }
}

# --- services (NSSM or otherwise) ---------------------------------------------
$services = @()
foreach ($name in $serviceNames) {
    $svc = Get-Service -Name $name -ErrorAction SilentlyContinue
    if ($null -ne $svc) { $services += $svc }
}
if ($services.Count -gt 0) {
    if (-not (Test-IsAdmin)) {
        throw 'EmailHelper services are installed; re-run this script from an elevated PowerShell to remove them.'
    }
    $nssmCmd = Get-Command 'nssm.exe' -ErrorAction SilentlyContinue
    foreach ($svc in $services) {
        if ($PSCmdlet.ShouldProcess($svc.Name, 'Stop and remove service')) {
            if ($svc.Status -ne 'Stopped') {
                Stop-Service -Name $svc.Name -Force -ErrorAction SilentlyContinue
            }
            if ($null -ne $nssmCmd) {
                & $nssmCmd.Source remove $svc.Name confirm | Out-Null
            } else {
                & sc.exe delete $svc.Name | Out-Null
            }
            if ($LASTEXITCODE -ne 0) {
                Write-Warning "Removing service '$($svc.Name)' returned exit code $LASTEXITCODE"
            } else {
                Write-Step "removed service '$($svc.Name)'"
            }
        }
    }
}

# --- leftover processes --------------------------------------------------------
$exePath = Join-Path $InstallDir 'emailhelper.exe'
foreach ($proc in Get-Process -Name 'emailhelper' -ErrorAction SilentlyContinue) {
    $procPath = $null
    try { $procPath = $proc.Path } catch { $procPath = $null }
    if (($null -ne $procPath) -and ($procPath -eq $exePath)) {
        if ($PSCmdlet.ShouldProcess("emailhelper.exe (pid $($proc.Id))", 'Stop process')) {
            Stop-Process -Id $proc.Id -Force
        }
    }
}
Start-Sleep -Seconds 1

# --- files ----------------------------------------------------------------------
if (-not (Test-Path -LiteralPath $InstallDir)) {
    Write-Step "$InstallDir does not exist; nothing else to remove"
    return
}

if ($RemoveData) {
    # High-impact: always confirm unless -Confirm:$false.
    $question = "Delete $InstallDir including .env, knowledge, the database and the browser profile?"
    if ($PSCmdlet.ShouldProcess($InstallDir, 'Delete install directory and ALL data') -and
        ($PSBoundParameters.ContainsKey('Confirm') -or $PSCmdlet.ShouldContinue($question, 'EmailHelper uninstall'))) {
        # Running from inside the install dir would lock it.
        Set-Location -LiteralPath $env:TEMP
        Remove-Item -LiteralPath $InstallDir -Recurse -Force
        Write-Step "deleted $InstallDir"
    }
} else {
    foreach ($item in @('emailhelper.exe', 'run-hidden.ps1', 'browser-worker')) {
        $path = Join-Path $InstallDir $item
        if (Test-Path -LiteralPath $path) {
            if ($PSCmdlet.ShouldProcess($path, 'Remove')) {
                Remove-Item -LiteralPath $path -Recurse -Force
                Write-Step "removed $path"
            }
        }
    }
    Write-Host ''
    Write-Host "Kept your data in $InstallDir (.env, knowledge, config, data, logs, uninstall.ps1)."
    Write-Host 'Re-run with -RemoveData to delete it as well.'
}
