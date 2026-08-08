@echo off
REM Run the undo verification unattended.
REM
REM The script prompts for Return before a live run, deliberately: it types
REM into Notepad and a person at the keyboard deserves warning. A background
REM run has no console to answer that prompt and hangs forever, so a wrapper
REM that sets the documented bypass is clearer than remembering the variable.
setlocal
set OUTLOUD_LIVE_YES=1
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0verify-undo-windows.ps1" %*
endlocal
