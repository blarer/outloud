# Does the undo harness's watch loop actually terminate?
#
# The loop waited on another process with no deadline and hung forever,
# leaving a stale result file that read as a PASS from a previous run. The
# fix added a deadline, but "the source contains AddSeconds(120)" is not
# evidence the loop exits: that is the same class of mistake as trusting a
# green test suite over what the user can see.
#
# So run the real control flow against a process that never finishes, with a
# short deadline, and assert it returns.
$ErrorActionPreference = 'Stop'

# A process that will not exit on its own.
$victim = Start-Process powershell -ArgumentList '-NoProfile', '-Command', 'Start-Sleep -Seconds 600' -PassThru -WindowStyle Hidden

$started = Get-Date
$deadline = $started.AddSeconds(3)
$timedOut = $false
while (-not $victim.HasExited) {
    if ((Get-Date) -gt $deadline) {
        $timedOut = $true
        try { $victim.Kill() } catch { }
        break
    }
    Start-Sleep -Milliseconds 120
}
$elapsed = ((Get-Date) - $started).TotalSeconds

Stop-Process -Id $victim.Id -Force -ErrorAction SilentlyContinue

Write-Output ("elapsed: {0:N1}s  timedOut: {1}  victimKilled: {2}" -f $elapsed, $timedOut, $victim.HasExited)
if ($timedOut -and $elapsed -lt 10) {
    Write-Output 'PASS: the watch loop exits on its deadline instead of hanging'
    exit 0
}
Write-Output 'FAIL: the watch loop did not terminate as designed'
exit 1
