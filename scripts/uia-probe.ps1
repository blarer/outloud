# What does UI Automation report about the focused element right now?
#
# `mode_at_keydown` on Windows decides dictate-vs-edit purely from
# `GetFocusedElement` + TextPattern's selection. When a run reports plain
# dictation against a window that visibly has a selection, the question is
# whether UIA disagrees with the screen. This answers that directly, without
# involving the pipeline.
#
# Counts down first so the target window can be focused: querying focus from
# a console necessarily reports the console.
param([int]$DelaySeconds = 5)

$ErrorActionPreference = 'Stop'
Write-Output "focus your target window; querying in $DelaySeconds seconds..."
Start-Sleep -Seconds $DelaySeconds

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$element = [System.Windows.Automation.AutomationElement]::FocusedElement
if (-not $element) { Write-Output 'no focused element'; exit 1 }

Write-Output "name:        '$($element.Current.Name)'"
Write-Output "control:     $($element.Current.ControlType.ProgrammaticName)"
Write-Output "class:       $($element.Current.ClassName)"
Write-Output "process:     $($element.Current.ProcessId)"

$textPattern = $null
if ($element.TryGetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern, [ref]$textPattern)) {
    $ranges = $textPattern.GetSelection()
    Write-Output "TextPattern: yes, $($ranges.Count) selection range(s)"
    foreach ($r in $ranges) {
        $t = $r.GetText(-1)
        Write-Output "  selected: '$t' (len $($t.Length))"
    }
} else {
    Write-Output 'TextPattern: NO (this is why edit-by-voice sees no selection)'
}

$valuePattern = $null
if ($element.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$valuePattern)) {
    $v = $valuePattern.Current.Value
    Write-Output "ValuePattern: yes, value length $($v.Length)"
} else {
    Write-Output 'ValuePattern: NO'
}
