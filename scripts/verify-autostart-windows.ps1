# Verify launch-at-login on Windows end to end, against the REAL registry.
#
# What this proves (which `cargo test` cannot): the full chain
#   config.toml -> MenuHost::reload() -> autostart::sync() -> HKCU Run key
# works in the shipped binary, in both directions, with the value a user
# would actually see in Task Manager's Startup tab.
#
# Isolation: config comes from a throwaway config home (the config crate
# resolves XDG_CONFIG_HOME first, on every platform). The registry is the
# REAL one on purpose -- that is the thing
# being verified -- but the value name is the product's own ("OutLoud"), and
# the script restores whatever was there before it ran.
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\debug\outloud.exe"
if (-not (Test-Path $exe)) { throw "build first: cargo build -p outloud" }

$runKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$prior = (Get-ItemProperty -Path $runKey -Name OutLoud -ErrorAction SilentlyContinue).OutLoud
if ($null -ne $prior) { Write-Host "note: preexisting OutLoud entry: $prior" }

# Throwaway config home so the test never edits the user's real config.toml.
$tmp = Join-Path $env:TEMP "outloud-autostart-verify"
Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path (Join-Path $tmp "outloud") | Out-Null
$oldXdg = $env:XDG_CONFIG_HOME
$env:XDG_CONFIG_HOME = $tmp
$env:OUTLOUD_NO_INJECT = "1"   # belt and braces: nothing may type anywhere

function Run-Daemon {
    # A short-lived daemon run: start, give reload() time to fire (it runs in
    # MenuHost::new before the pipeline), then stop. Mock recognizer so no
    # model or mic is needed; --no-overlay so no window appears.
    $p = Start-Process -FilePath $exe -ArgumentList "--asr","mock","--no-overlay" `
        -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $tmp "err.txt") `
        -RedirectStandardOutput (Join-Path $tmp "out.txt")
    Start-Sleep -Seconds 3
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    Wait-Process -Id $p.Id -ErrorAction SilentlyContinue
}

$failed = $false
try {
    # Phase 1: launch-at-login = true must install the Run value.
    Set-Content -Path (Join-Path $tmp "outloud\config.toml") -Value "launch-at-login = true"
    Run-Daemon
    $entry = (Get-ItemProperty -Path $runKey -Name OutLoud -ErrorAction SilentlyContinue).OutLoud
    if ($entry -eq "`"$exe`"") {
        Write-Host "PASS: Run key installed: $entry"
    } else {
        Write-Host "FAIL: expected `"$exe`", got: $entry"
        $failed = $true
    }

    # Phase 2: launch-at-login = false must remove it again.
    Set-Content -Path (Join-Path $tmp "outloud\config.toml") -Value "launch-at-login = false"
    Run-Daemon
    $entry = (Get-ItemProperty -Path $runKey -Name OutLoud -ErrorAction SilentlyContinue).OutLoud
    if ($null -eq $entry) {
        Write-Host "PASS: Run key removed"
    } else {
        Write-Host "FAIL: Run key still present: $entry"
        $failed = $true
    }
} finally {
    # Restore the world: the user's config home, and whatever OutLoud Run
    # entry existed before (including none).
    $env:XDG_CONFIG_HOME = $oldXdg
    if ($null -ne $prior) {
        Set-ItemProperty -Path $runKey -Name OutLoud -Value $prior
    } else {
        Remove-ItemProperty -Path $runKey -Name OutLoud -ErrorAction SilentlyContinue
    }
}
if ($failed) { exit 1 }
Write-Host "autostart verification: all green"
