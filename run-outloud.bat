@echo off
REM Start the dictation daemon with the whisper recognizer.
REM
REM Paths are relative to this script, so moving or renaming the checkout
REM does not break it. The previous version hardcoded C:\Users\blare\outloud
REM and stopped working the moment the repo moved, failing with a model-not-
REM found error that pointed at the model rather than at the stale path.
setlocal
set "ROOT=%~dp0"
set "OUTLOUD_WHISPER_MODEL=%ROOT%ggml-base.en.bin"

if not exist "%OUTLOUD_WHISPER_MODEL%" (
    echo Model not found: %OUTLOUD_WHISPER_MODEL%
    echo Download a ggml .bin from https://huggingface.co/ggerganov/whisper.cpp
    echo or point OUTLOUD_WHISPER_MODEL at one you already have.
    exit /b 1
)

"%ROOT%target\release\outloud.exe" --asr whisper > "%ROOT%outloud.log" 2>&1
endlocal
