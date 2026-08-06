# Synthesize text to a 16kHz mono 16-bit WAV using Windows' built-in
# speech synthesizer. The Windows counterpart of macOS `say -o`, used by
# `outloud --say` so the same harness works on both platforms.
param(
    [Parameter(Mandatory = $true)][string]$Text,
    [Parameter(Mandatory = $true)][string]$Out
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Speech

$synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
try {
    # 16kHz mono 16-bit is what the recognizers want, so ask the synthesizer
    # for it directly rather than resampling afterwards the way the macOS
    # path needs `afconvert` to do.
    $format = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(
        16000,
        [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen,
        [System.Speech.AudioFormat.AudioChannel]::Mono)
    $synth.SetOutputToWaveFile($Out, $format)
    $synth.Speak($Text)
} finally {
    $synth.Dispose()
}
