# Stop components started by scripts\run-local.ps1.
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
$run = Join-Path $root 'data\run'
if (-not (Test-Path $run)) { Write-Host 'nothing running'; return }
foreach ($f in Get-ChildItem $run -Filter *.pid) {
    $procId = Get-Content $f.FullName
    $p = Get-Process -Id $procId -ErrorAction SilentlyContinue
    if ($null -ne $p) {
        Stop-Process -Id $procId -Force
        Write-Host "stopped $($f.BaseName) (pid $procId)"
    }
    Remove-Item $f.FullName -Force
}
