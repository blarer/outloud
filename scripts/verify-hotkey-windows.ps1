# Does the real hotkey path work? Press the chord, watch the daemon react.
#
# Everything else in this directory replays audio through `--say`, which
# enters the pipeline BELOW the hotkey and the microphone. That leaves the
# product's actual entry point (hold a key, speak, release) untested, and it
# is the half most likely to break on Windows: a low-level keyboard hook is
# subject to UIPI, to being silently unhooked after 300ms, and to the
# focus-stealing rules.
#
# This presses and releases the chord with SendInput (via SendKeys) and
# checks the daemon logged the state changes. It does NOT speak, so no text
# is written anywhere: the assertion is that the key edges were SEEN.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root 'target\release\outloud.exe'
$log = Join-Path $env:TEMP 'outloud-hotkey-probe.log'

# No injection: a stray recognition must not type into whatever is focused.
$env:OUTLOUD_NO_INJECT = '1'

Write-Output 'starting the daemon (mock recognizer, no overlay)...'
$daemon = Start-Process $exe -ArgumentList @('--no-overlay', '--asr', 'mock') `
    -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput $log -RedirectStandardError "$log.err"
Start-Sleep -Seconds 3

if ($daemon.HasExited) {
    Write-Output "INCONCLUSIVE: the daemon exited before the probe (code $($daemon.ExitCode))"
    Get-Content "$log.err" -ErrorAction SilentlyContinue
    exit 1
}

# Send the ACTUAL chord: `right-option` maps to VK_RMENU (0xA5) in
# winmatch.rs. SendKeys cannot express left-vs-right, and sending the wrong
# key would prove only that the hook is alive, not that chord matching works
# -- so drive keybd_event with the real virtual-key code and the extended
# flag that distinguishes right Alt from left.
Add-Type -Namespace Win32 -Name Key -MemberDefinition @'
[DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
'@
$VK_RMENU = 0xA5
$KEYEVENTF_EXTENDEDKEY = 0x1
$KEYEVENTF_KEYUP = 0x2

Write-Output 'holding the hotkey for 1.5s...'
[Win32.Key]::keybd_event($VK_RMENU, 0, $KEYEVENTF_EXTENDEDKEY, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 1500
[Win32.Key]::keybd_event($VK_RMENU, 0, $KEYEVENTF_EXTENDEDKEY -bor $KEYEVENTF_KEYUP, [UIntPtr]::Zero)
Start-Sleep -Seconds 2

Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500

Write-Output '--- daemon stderr ---'
$err = Get-Content "$log.err" -ErrorAction SilentlyContinue
$err | ForEach-Object { Write-Output $_ }

Write-Output '--- result ---'
$bound = $err -match 'hold .* to dictate'
# `state listening` is the proof that matters: it means the hook received the
# key-down, the matcher recognised the chord, and the pipeline opened the
# microphone. Binding alone proves only that nothing errored at startup.
$listened = $err -match 'state listening'
if ($listened) {
    Write-Output 'PASS: the hook saw the chord and the daemon started listening'
} elseif ($bound) {
    Write-Output 'FAIL: the hotkey bound but pressing it did nothing (hook not receiving, or UIPI)'
} else {
    Write-Output 'FAIL: the hotkey never bound'
}
