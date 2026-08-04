# Open Notepad with known text, focused and fully selected.
#
# The undo path (`"scratch that"`) can only be exercised against a real
# focused field with a real selection: UIA's GetFocusedElement is what
# `UiaTarget::read()` calls, and there is no way to fake it from a test.
# This puts the machine in that state deterministically so the check is
# repeatable rather than a described manual ritual nobody performs.
param(
    [string]$Text = 'the quick brown fox'
)

$ErrorActionPreference = 'Stop'

# A fresh file per run: reusing one leaves the previous run's edits in the
# buffer, and "undo restored the text" is then unfalsifiable.
$path = Join-Path $env:TEMP ("outloud-undo-{0}.txt" -f (Get-Date -Format 'HHmmss'))
Set-Content -Path $path -Value $Text -NoNewline -Encoding UTF8

$proc = Start-Process notepad.exe -ArgumentList $path -PassThru
# Notepad creates its window asynchronously; typing into a window that does
# not exist yet silently goes nowhere.
$null = $proc.WaitForInputIdle(5000)
Start-Sleep -Milliseconds 400

Add-Type -AssemblyName System.Windows.Forms
[System.Windows.Forms.SendKeys]::SendWait('^a')
Start-Sleep -Milliseconds 200

Write-Output "pid=$($proc.Id) path=$path text=$Text"
