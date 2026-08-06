# Probe UIA against a Notepad window this script focuses itself.
#
# The unattended counterpart of `uia-probe.ps1`: opens Notepad on known text,
# selects all, activates it, and reports what UI Automation sees. Removes the
# human from the loop so the answer is reproducible.
$ErrorActionPreference = 'Stop'

$path = Join-Path $env:TEMP ("uia-probe-{0}.txt" -f (Get-Date -Format 'HHmmssfff'))
Set-Content -Path $path -Value 'the quick brown fox' -NoNewline -Encoding UTF8
$leaf = Split-Path -Leaf $path

$null = Start-Process notepad.exe -ArgumentList $path
Start-Sleep -Milliseconds 1200

$window = Get-Process notepad -ErrorAction SilentlyContinue |
    Where-Object { $_.MainWindowTitle -like "*$leaf*" } | Select-Object -First 1
if (-not $window) { throw "no Notepad window showing $leaf" }

Add-Type -AssemblyName Microsoft.VisualBasic
Add-Type -AssemblyName System.Windows.Forms
[Microsoft.VisualBasic.Interaction]::AppActivate($window.Id)
Start-Sleep -Milliseconds 600
[System.Windows.Forms.SendKeys]::SendWait('^a')
Start-Sleep -Milliseconds 400

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$element = [System.Windows.Automation.AutomationElement]::FocusedElement
if (-not $element) { Write-Output 'no focused element'; exit 1 }

Write-Output "name:        '$($element.Current.Name)'"
Write-Output "control:     $($element.Current.ControlType.ProgrammaticName)"
Write-Output "class:       $($element.Current.ClassName)"
Write-Output "process:     $($element.Current.ProcessId) (notepad=$($window.Id))"

$textPattern = $null
if ($element.TryGetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern, [ref]$textPattern)) {
    $ranges = $textPattern.GetSelection()
    Write-Output "TextPattern: yes, $($ranges.Count) selection range(s)"
    foreach ($r in $ranges) {
        $t = $r.GetText(-1)
        Write-Output "  selected: '$t' (len $($t.Length))"
    }
} else {
    Write-Output 'TextPattern: NO'
}

$valuePattern = $null
if ($element.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$valuePattern)) {
    Write-Output "ValuePattern: yes, len $($valuePattern.Current.Value.Length)"
} else {
    Write-Output 'ValuePattern: NO'
}

[Microsoft.VisualBasic.Interaction]::AppActivate($window.Id)
Start-Sleep -Milliseconds 200
[System.Windows.Forms.SendKeys]::SendWait('^w')
Start-Sleep -Milliseconds 400
[System.Windows.Forms.SendKeys]::SendWait('%n')
