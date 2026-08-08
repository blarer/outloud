# Launch outloud the way Explorer does: no env vars, no arguments.
#
# A shell inherits OUTLOUD_WHISPER_MODEL and whatever else the developer set,
# so running from a terminal cannot reproduce what a double-click does. This
# strips those variables and starts the exe from its own directory, which is
# how the "I launched the exe and it never works" bug hid: the daemon died
# instantly on a macOS-only default recognizer, and every terminal test passed
# because it was given --asr whisper.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root 'target\release\outloud.exe'
$log = Join-Path $env:TEMP 'outloud-doubleclick.log'

foreach ($v in 'OUTLOUD_WHISPER_MODEL', 'OUTLOUD_NO_INJECT', 'OUTLOUD_REPLAY_DELAY_MS') {
    Remove-Item "Env:\$v" -ErrorAction SilentlyContinue
}

Write-Output 'launching with no arguments and no environment...'
$p = Start-Process $exe -WorkingDirectory (Split-Path -Parent $exe) `
    -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput $log -RedirectStandardError "$log.err"

Start-Sleep -Seconds 12
$alive = -not $p.HasExited

Write-Output '--- stderr ---'
Get-Content "$log.err" -ErrorAction SilentlyContinue

Write-Output '--- result ---'
if ($alive) {
    Write-Output 'PASS: the daemon is still running, so a double-click works'
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
} else {
    Write-Output "FAIL: it exited on its own; a user double-clicking sees nothing happen"
}
