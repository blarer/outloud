# Run the live undo check until it produces a conclusive result.
#
# The check has to type into a real focused field, so it competes with
# whatever else is running. When something steals the foreground the harness
# aborts rather than typing into it, which is correct but means a single
# attempt on a machine in use is often inconclusive rather than wrong.
#
# Retrying is safe precisely because of that guard: an aborted attempt writes
# nothing anywhere. So retry a bounded number of times and report the first
# real PASS or FAIL, treating repeated aborts as "the machine is too busy to
# answer", which is honest and distinguishable from a product failure.
#
# An isolated Windows desktop would remove the race entirely, but
# SetThreadDesktop answers ERROR_BUSY (170) from any PowerShell host, since
# the console owns a window before a script runs. Left documented here so the
# next person does not spend the same hour discovering it.
param([int]$Attempts = 6)

$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$resultLog = Join-Path $env:TEMP 'outloud-undo-result.txt'

for ($i = 1; $i -le $Attempts; $i++) {
    Write-Output "attempt $i of ${Attempts}..."
    Remove-Item $resultLog -ErrorAction SilentlyContinue

    $env:OUTLOUD_LIVE_YES = '1'
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $here 'verify-undo-windows.ps1') *> $null

    $out = Get-Content $resultLog -ErrorAction SilentlyContinue
    if (-not $out) {
        Write-Output '  (no result written; treating as aborted)'
        Start-Sleep -Seconds 2
        continue
    }

    $verdict = $out | Where-Object { $_ -match '^(PASS|FAIL|INCONCLUSIVE|ABORTED|TIMEOUT)' } | Select-Object -Last 1
    Write-Output "  $verdict"

    if ($verdict -match '^PASS') {
        Write-Output ''
        $out | ForEach-Object { Write-Output $_ }
        exit 0
    }
    if ($verdict -match '^FAIL') {
        Write-Output ''
        $out | ForEach-Object { Write-Output $_ }
        exit 1
    }
    Start-Sleep -Seconds 2
}

Write-Output ''
Write-Output "INCONCLUSIVE: $Attempts attempts all lost the foreground. Nothing was typed anywhere."
Write-Output 'Close or minimise whatever keeps taking focus, then rerun.'
exit 2
