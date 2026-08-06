@echo off
REM Dry-run the whisper pipeline against synthesized speech.
REM
REM Delivery is suppressed: this reports what WOULD have been written,
REM including the edit route, without typing into any window.
setlocal
set OUTLOUD_NO_INJECT=1
set OUTLOUD_WHISPER_MODEL=%~dp0..\ggml-base.en.bin
"%~dp0..\target\release\outloud.exe" --once --asr whisper %*
endlocal
