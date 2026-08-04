@echo off
REM Two daemons must not run at once: the second has to refuse.
REM
REM Both would bind the same hotkey and open the microphone, so one keypress
REM records and types twice. The Windows guard is a Global\ named mutex, and
REM it had never been exercised on Windows hardware -- the code around it
REM carried a `std::mem::forget` on a Copy type that did nothing at all.
REM
REM DAEMON mode specifically: `--once` deliberately skips the guard (it is a
REM one-shot measurement that neither binds the hotkey nor stays resident,
REM and benchmarks run several at a time), so a --once pair would report
REM success while never touching the mutex.
setlocal
set OUTLOUD_NO_INJECT=1
set EXE=%~dp0..\target\release\outloud.exe

echo starting first daemon in the background...
start "outloud-first" /min "%EXE%" --no-overlay --asr mock
REM Let the first one take the mutex before the second tries.
timeout /t 3 /nobreak >nul

echo.
echo starting second daemon, which must refuse:
"%EXE%" --no-overlay --asr mock
echo second daemon exit code: %ERRORLEVEL% (non-zero = guard held)

echo.
echo stopping the first daemon...
taskkill /f /fi "WINDOWTITLE eq outloud-first*" >nul 2>&1
taskkill /f /im outloud.exe >nul 2>&1
endlocal
