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

# Announce before taking over the keyboard.
#
# The focus watchdog below is good at catching a window that STEALS focus
# mid-run, but it cannot help the person sitting at the machine who simply
# did not know a run was starting. On the Mac an equivalent script began
# while its user was mid-sentence in another app and put a test phrase into
# their window; the same script had also, on this machine, pasted into a
# live Discord chat.
#
# One prompt costs a keypress. Being surprised by your own keyboard costs
# trust, and both of those incidents were avoidable this way.
#
# -DryRun writes nothing, so it skips the prompt. OUTLOUD_LIVE_YES=1 skips
# it for unattended/CI runs that have already accepted the risk.
if (-not $DryRun -and $env:OUTLOUD_LIVE_YES -ne '1') {
    Write-Host ''
    Write-Host '  ABOUT TO DICTATE ON THIS MACHINE' -ForegroundColor Yellow
    Write-Host '  --------------------------------' -ForegroundColor Yellow
    Write-Host '  This replays speech and TYPES INTO NOTEPAD.'
    Write-Host '  Stop typing until it finishes (about 20 seconds).'
    Write-Host ''
    Write-Host '  Press Return to start, or Ctrl-C to cancel.'
    Write-Host ''
    [void](Read-Host)
}

# Tee everything to a file as well as the console. SendKeys steals focus
# while this runs, so a console redirect can end up mangled or lost; the
# result of a verification run must survive being run unattended.
$resultLog = Join-Path $env:TEMP 'outloud-undo-result.txt'
Set-Content -Path $resultLog -Value '' -Encoding UTF8
function Say([string]$line) {
    Write-Output $line
    Add-Content -Path $resultLog -Value $line -Encoding UTF8
}

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
Say "notepad window pid=$($window.Id) title='$($window.MainWindowTitle)'"

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

# Watch focus until the run finishes, and KILL it the moment the target
# stops being frontmost.
#
# Not a nicety. A real (non-dry) run writes into whatever is focused, so an
# app that steals focus mid-run receives the test sentence: on this machine
# Discord did exactly that, matched its own ClipboardOnly rule, and got
# "Change quick to slow." pasted into a live chat box. The window of exposure
# is the whole utterance, and no amount of care about the FIRST focus call
# closes it, because the theft happens later.
Add-Type -Namespace Win32 -Name Focus -MemberDefinition @'
[DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
'@
$stolenBy = $null
$reclaims = 0
while (-not $outloud.HasExited) {
    $fg = [Win32.Focus]::GetForegroundWindow()
    $fgPid = 0
    [Win32.Focus]::GetWindowThreadProcessId($fg, [ref]$fgPid) | Out-Null
    if ($fgPid -ne 0 -and $fgPid -ne $window.Id -and -not $DryRun) {
        # Try to take focus back ONCE before giving up. A window that flashes
        # to the front and yields again (an updater, a notification toast) is
        # common on a machine someone is using, and aborting on it makes the
        # check unrunnable. A window that holds focus is the dangerous case,
        # because the utterance would be written into it.
        $thief = (Get-Process -Id $fgPid -ErrorAction SilentlyContinue).ProcessName
        if ($reclaims -lt 1) {
            $reclaims++
            Say "note: '$thief' took focus; reclaiming once"
            [Microsoft.VisualBasic.Interaction]::AppActivate($window.Id)
            Start-Sleep -Milliseconds 400
            continue
        }
        $stolenBy = $thief
        $outloud.Kill()
        break
    }
    Start-Sleep -Milliseconds 120
}
if ($stolenBy) {
    Say "ABORTED: '$stolenBy' took focus mid-run; killed the run before it could type into it. Rerun with nothing else grabbing focus."
    exit 3
}

$outloud.WaitForExit(120000) | Out-Null

Say '--- stdout ---'
if (Test-Path $log) { Get-Content $log | ForEach-Object { Say $_ } }
Say '--- stderr ---'
if (Test-Path "$log.err") { Get-Content "$log.err" | ForEach-Object { Say $_ } }

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

Say '--- result ---'
Say "original: '$Text'"
Say "final:    '$final'"

# Confirm the readback actually came from OUR Notepad. SendKeys goes to
# whatever holds focus, so anything that steals it mid-run (a browser, an
# updater, a game) is read instead, and the run reports a product failure
# that is really a stolen-focus artifact. One such run came back holding a
# browser's URL bar.
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$focused = [System.Windows.Automation.AutomationElement]::FocusedElement
$focusedPid = if ($focused) { $focused.Current.ProcessId } else { 0 }
if ($focusedPid -ne $window.Id) {
    Say "INCONCLUSIVE: focus was stolen (read from pid $focusedPid, expected $($window.Id)); rerun"
    exit 2
}

# A mismatch is only meaningful if the field held the expected text to begin
# with. SendKeys goes to whatever is focused at that instant, so a stray
# keystroke landing in Notepad corrupts the fixture and the run then reports
# a product failure that is really a harness failure. Distinguish the two.
if ($final -eq $Text) {
    Say 'PASS: undo restored the original text'
} elseif ($final -like "*$Text*") {
    Say "INCONCLUSIVE: field contains the original plus stray input ('$final'); the harness leaked keystrokes, rerun"
} else {
    Say 'FAIL: field does not match the original'
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

if ($broken -eq {
