# Can a machine with no model at all start dictating?
#
# This is the check that would have caught the launch bug: a fresh Windows
# box has no ggml .bin anywhere, Explorer passes no arguments and no
# environment, and whisper is the default recognizer off macOS. Until the
# downloader was wired in, that combination produced a daemon that exited in
# under a second.
#
# Runs the exe from an empty directory with every relevant variable stripped,
# so neither the repo's own model nor a developer's OUTLOUD_WHISPER_MODEL can
# make a broken build look fine. The model cache is moved aside and restored,
# never deleted: it is 148MB the user already paid for.
param([switch]$KeepCache)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root 'target\release\outloud.exe'
if (-not (Test-Path $exe)) { throw "build it first: $exe" }

$cache = Join-Path $env:USERPROFILE '.outloud\models'
$stash = "$cache.stashed-$PID"
$sandbox = Join-Path $env:TEMP "outloud-firstrun-$PID"

foreach ($v in 'OUTLOUD_WHISPER_MODEL', 'OUTLOUD_NO_DOWNLOAD', 'OUTLOUD_NO_INJECT') {
    Remove-Item "Env:\$v" -ErrorAction SilentlyContinue
}

New-Item -ItemType Directory -Force -Path $sandbox | Out-Null
Copy-Item $exe $sandbox -Force
$moved = $false
if (-not $KeepCache -and (Test-Path $cache)) {
    Move-Item $cache $stash
    $moved = $true
    Write-Output "moved the model cache aside (restored at the end)"
}

$log = Join-Path $env:TEMP "outloud-firstrun-$PID.log"
$exitCode = 1
try {
    Write-Output 'launching with no arguments, no model, no environment...'
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process (Join-Path $sandbox 'outloud.exe') -WorkingDirectory $sandbox `
        -PassThru -WindowStyle Hidden -RedirectStandardError $log

    # Generous: the model is ~148MB, so a slow connection is not a failure.
    $ready = $false
    while ($sw.Elapsed.TotalSeconds -lt 600) {
        if (Test-Path $log) {
            if ((Get-Content $log -Raw -ErrorAction SilentlyContinue) -match 'recognizer ready') {
                $ready = $true
                break
            }
        }
        if ($p.HasExited) { break }
        Start-Sleep -Milliseconds 300
    }
    $elapsed = $sw.Elapsed.TotalSeconds
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue

    Write-Output '--- daemon stderr ---'
    Get-Content $log -ErrorAction SilentlyContinue |
        Where-Object { $_ -notmatch 'downloading model \d+%' } |
        ForEach-Object { Write-Output "  $_" }

    Write-Output '--- result ---'
    if ($ready) {
        Write-Output ('PASS: reached a working recognizer in {0:N1}s with nothing preinstalled' -f $elapsed)
        $exitCode = 0
    } else {
        Write-Output ('FAIL: no recognizer after {0:N1}s; a fresh machine cannot dictate' -f $elapsed)
    }
} finally {
    if ($moved) {
        # The run just downloaded a fresh copy; keep the ORIGINAL, which the
        # user already had, and discard the duplicate.
        if (Test-Path $cache) { Remove-Item $cache -Recurse -Force -ErrorAction SilentlyContinue }
        Move-Item $stash $cache -ErrorAction SilentlyContinue
        Write-Output 'restored the original model cache'
    }
    Remove-Item $sandbox -Recurse -Force -ErrorAction SilentlyContinue
}
exit $exitCode
