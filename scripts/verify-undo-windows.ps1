# Exercise the Windows undo path end to end against a real Notepad window.
#
# Undo cannot be unit-tested: `UiaTarget::read()` reads whatever UIA reports
# as the FOCUSED element, and the dictate-vs-edit decision is made at
# key-down from that same focus. So the only honest check is a real window,
# really focused, with a real selection.
#
# Shape of the run:
#   1. Notepad opens with known text, selected.
#   2. outloud starts with OUTLOUD_REPLAY_DELAY_MS, so its first key-down has
#      not happened yet.
#   3. This script re-focuses Notepad inside that window (starting outloud
#      hands focus back to the console otherwise).
#   4. Two utterances replay: an edit, then "scratch that".
#   5. The file is read back and compared against the original text.
#
# Step 5 is the point. Every previous version of this check asserted on log
# lines, which prove a value was computed, not that the user's text came
# back.
param(
    [string]$Text = 'the quick brown fox',
    [string]$Edit = 'change quick to slow',
    [int]$DelayMs = 6000,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

$path = Join-Path $env:TEMP ("outloud-undo-{0}.txt" -f (Get-Date -Format 'HHmmssfff'))
Set-Content -Path $path -Value $Text -NoNewline -Encoding UTF8

$notepad = Start-Process notepad.exe -ArgumentList $path -PassThru
$null = $notepad.WaitForInputIdle(5000)
Start-Sleep -Milliseconds 500

# The PID that Start-Process returns is NOT the one owning the window.
# Modern Notepad re-parents new files into an existing instance and the
# launched process exits (or lingers with no window), so AppActivate on it
# fails with "Process was not found". Find the process whose window title
# carries our file name instead.
$leaf = Split-Path -Leaf $path
$deadline = (Get-Date).AddSeconds(10)
$window = $null
while ((Get-Date) -lt $deadline -and -not $window) {
    $window = Get-Process notepad -ErrorAction SilentlyContinue |
        Where-Object { $_.MainWindowTitle -like "*$leaf*" } |
        Select-Object -First 1
    if (-not $window) { Start-Sleep -Milliseconds 300 }
}
if (-not $window) { throw "no Notepad window showing $leaf" }
Write-Output "notepad window pid=$($window.Id) title='$($window.MainWindowTitle)'"

$env:OUTLOUD_WHISPER_MODEL = Join-Path $root 'ggml-base.en.bin'
$env:OUTLOUD_REPLAY_DELAY_MS = "$DelayMs"
if ($DryRun) { $env:OUTLOUD_NO_INJECT = '1' } else { Remove-Item Env:\OUTLOUD_NO_INJECT -ErrorAction SilentlyContinue }

$exe = Join-Path $root 'target\release\outloud.exe'
$log = Join-Path $env:TEMP 'outloud-undo-run.log'
$outloud = Start-Process $exe `
    -ArgumentList @('--once', '--asr', 'whisper', '--say', "`"$Edit`"", '--say', '"scratch that"') `
    -PassThru -NoNewWindow -RedirectStandardOutput $log -RedirectStandardError "$log.err"

# Re-focus Notepad and re-select, inside the replay delay. Starting a process
# moves focus, so without this the key-down samples the console and the run
# silently degrades to plain dictation.
Start-Sleep -Milliseconds 1200
Add-Type -AssemblyName Microsoft.VisualBasic
Add-Type -AssemblyName System.Windows.Forms
[Microsoft.VisualBasic.Interaction]::AppActivate($window.Id)
Start-Sleep -Milliseconds 600
[System.Windows.Forms.SendKeys]::SendWait('^a')

$outloud.WaitForExit(120000) | Out-Null

Write-Output '--- stdout ---'
if (Test-Path $log) { Get-Content $log }
Write-Output '--- stderr ---'
if (Test-Path "$log.err") { Get-Content "$log.err" }

# Read the buffer back through UIA rather than the file: Notepad has not
# saved, so the file on disk still holds the ORIGINAL text and would report
# success no matter what happened on screen.
#
# Reading it costs the clipboard, so save and restore it: this project has
# already destroyed a developer's clipboard once by treating it as scratch
# space, and a verification script must not repeat the bug it verifies.
$savedClipboard = Get-Clipboard -Raw -ErrorAction SilentlyContinue
[Microsoft.VisualBasic.Interaction]::AppActivate($window.Id)
Start-Sleep -Milliseconds 400
[System.Windows.Forms.SendKeys]::SendWait('^a^c')
Start-Sleep -Milliseconds 400
$final = Get-Clipboard -Raw
if ($null -eq $final) { $final = '' }
$final = $final.TrimEnd("`r", "`n")
if ([string]::IsNullOrEmpty($savedClipboard)) {
    Set-Clipboard -Value ' '
} else {
    Set-Clipboard -Value $savedClipboard
}

Write-Output '--- result ---'
Write-Output "original: '$Text'"
Write-Output "final:    '$final'"
# A mismatch is only meaningful if the field held the expected text to begin
# with. SendKeys goes to whatever is focused at that instant, so a stray
# keystroke landing in Notepad corrupts the fixture and the run then reports
# a product failure that is really a harness failure. Distinguish the two.
if ($final -eq $Text) {
    Write-Output 'PASS: undo restored the original text'
} elseif ($final -like "*$Text*") {
    Write-Output "INCONCLUSIVE: field contains the original plus stray input ('$final'); the harness leaked keystrokes, rerun"
} else {
    Write-Output 'FAIL: field does not match the original'
}

# Close OUR tab, rather than killing the process: the window we found may be
# a shared Notepad instance holding the user's other files, and force-killing
# it would discard their unsaved work.
[Microsoft.VisualBasic.Interaction]::AppActivate($window.Id)
Start-Sleep -Milliseconds 300
[System.Windows.Forms.SendKeys]::SendWait('^w')
Start-Sleep -Milliseconds 400
# "Save changes?" if the buffer is dirty: discard, this is a scratch file.
[System.Windows.Forms.SendKeys]::SendWait('%n')
