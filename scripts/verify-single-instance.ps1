# Two daemons must not run at once: the second has to refuse.
#
# Both would bind the same hotkey and open the microphone, so one keypress
# records and types twice. The Windows guard is a `Global\` named mutex whose
# surrounding code carried a `std::mem::forget` on a Copy type that did
# nothing at all, and it had never been exercised on Windows hardware.
#
# DAEMON mode specifically: `--once` deliberately skips the guard (it is a
# one-shot measurement that neither binds the hotkey nor stays resident, and
# benchmarks run several at a time), so a pair of `--once` runs would report
# success while never touching the mutex.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root 'target\release\outloud.exe'
$env:OUTLOUD_NO_INJECT = '1'

$firstLog = Join-Path $env:TEMP 'outloud-instance-first.log'
$secondLog = Join-Path $env:TEMP 'outloud-instance-second.log'

Write-Output 'starting the first daemon...'
$first = Start-Process $exe -ArgumentList @('--no-overlay', '--asr', 'mock') `
    -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput $firstLog -RedirectStandardError "$firstLog.err"
Start-Sleep -Seconds 3

if ($first.HasExited) {
    Write-Output "INCONCLUSIVE: the first daemon exited on its own (code $($first.ExitCode))"
    Get-Content "$firstLog.err" -ErrorAction SilentlyContinue
    exit 1
}

Write-Output 'starting the second daemon, which must refuse...'
# Run through cmd and read $LASTEXITCODE rather than Start-Process's
# ExitCode: that property comes back EMPTY here even once the process has
# exited, which reads as "no exit code" and cannot be told apart from a
# failure. `timeout` bounds the wait so a second daemon that wrongly starts
# normally fails the check instead of hanging it forever.
$job = Start-Job -ScriptBlock {
    param($exe, $log)
    cmd /c "`"$exe`" --no-overlay --asr mock 2>`"$log`""
    $LASTEXITCODE
} -ArgumentList $exe, "$secondLog.err"

$refused = Wait-Job $job -Timeout 15
$code = if ($refused) { Receive-Job $job | Select-Object -Last 1 } else { $null }

Write-Output '--- second daemon stderr ---'
Get-Content "$secondLog.err" -ErrorAction SilentlyContinue

Write-Output '--- result ---'
if (-not $refused) {
    Write-Output 'FAIL: the second daemon is still running; two daemons share the hotkey and microphone'
    Stop-Job $job -ErrorAction SilentlyContinue
    Get-Process outloud -ErrorAction SilentlyContinue |
        Where-Object { $_.Id -ne $first.Id } |
        Stop-Process -Force -ErrorAction SilentlyContinue
} elseif ($code -eq 0) {
    Write-Output "FAIL: the second daemon exited 0, so it believed it started normally"
} else {
    Write-Output "PASS: the second daemon refused (exit $code)"
}

Remove-Job $job -Force -ErrorAction SilentlyContinue
Stop-Process -Id $first.Id -Force -ErrorAction SilentlyContinue
