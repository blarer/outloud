# Report the transport decision for a named app, without a human clicking.
#
# `outloud --route` reads whatever holds the foreground window, so checking
# several apps by hand means focusing each in turn and hoping nothing steals
# focus mid-read. This activates the target itself, then reads.
#
# The apps that matter are the ones with rules: Discord is ClipboardOnly
# (it accepts an accessibility write, reports success, and reverts it a
# moment later), Slack and friends are TypingOnly, everything else ordinary.
param(
    [Parameter(Mandatory = $true)][string]$Process
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

# Filter on the HANDLE, not just a non-empty title: Electron apps run half a
# dozen helper processes, and the ones with no window report handle 0, which
# SetForegroundWindow accepts and silently does nothing with.
$target = Get-Process -Name $Process -ErrorAction SilentlyContinue |
    Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $target) { throw "no window found for process '$Process'" }

# AppActivate silently fails against some windows (it went to the previously
# focused Notepad instead of Discord), so drive the Win32 call directly and
# restore a minimized window first: SetForegroundWindow on a minimized
# window succeeds without actually showing it.
Add-Type -Namespace Win32 -Name Fg -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
[DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
[DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, IntPtr pid);
[DllImport("user32.dll")] public static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool fAttach);
[DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
'@
$SW_RESTORE = 9
[Win32.Fg]::ShowWindow($target.MainWindowHandle, $SW_RESTORE) | Out-Null
[Win32.Fg]::SetForegroundWindow($target.MainWindowHandle) | Out-Null
Start-Sleep -Milliseconds 900

# Windows refuses SetForegroundWindow from a process that does not already
# own the foreground, which is the anti-focus-stealing policy and is correct
# behaviour. The documented workaround is to attach to the current
# foreground window's input queue first, which makes the two threads share
# focus state and lets the call through.
if ([Win32.Fg]::GetForegroundWindow() -ne $target.MainWindowHandle) {
    $fgThread = [Win32.Fg]::GetWindowThreadProcessId([Win32.Fg]::GetForegroundWindow(), [IntPtr]::Zero)
    $thisThread = [Win32.Fg]::GetCurrentThreadId()
    [Win32.Fg]::AttachThreadInput($thisThread, $fgThread, $true) | Out-Null
    [Win32.Fg]::SetForegroundWindow($target.MainWindowHandle) | Out-Null
    [Win32.Fg]::AttachThreadInput($thisThread, $fgThread, $false) | Out-Null
    Start-Sleep -Milliseconds 700
}

if ([Win32.Fg]::GetForegroundWindow() -ne $target.MainWindowHandle) {
    Write-Output "WARNING: could not bring '$Process' to the foreground; the reading below is of some other window"
}

& (Join-Path $root 'target\release\outloud.exe') --route 2
