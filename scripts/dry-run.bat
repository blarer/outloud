@echo off
REM Run a dry-pipeline check with delivery suppressed.
REM
REM `set VAR=1 && prog` on cmd.exe puts a TRAILING SPACE in the value, so
REM OUTLOUD_NO_INJECT becomes "1 ", the `== "1"` test fails, and the run
REM types its transcript into whatever window is focused. Setting the
REM variable on its own line is the only form that is safe here.
setlocal
set OUTLOUD_NO_INJECT=1
"%~dp0..\target\release\outloud.exe" %*
endlocal
