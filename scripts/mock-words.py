"""Count the voiced 100ms windows in a WAV, the way MockRecognizer does.

`--asr mock` emits one word per 10 voiced windows (one second of speech), so
a short utterance produces zero words and the run reports "heard nothing".
That looks exactly like a broken pipeline. This script tells the two apart.
"""
import struct
import sys
import wave

WINDOW = 1600  # 100ms at 16kHz
WINDOWS_PER_WORD = 10
VOICED_RMS = 0.01


def main(path: str) -> None:
    with wave.open(path, "rb") as w:
        raw = w.readframes(w.getnframes())
    samples = [s / 32768.0 for s in struct.unpack(f"<{len(raw)//2}h", raw)]
    voiced = 0
    for i in range(0, len(samples) - WINDOW + 1, WINDOW):
        win = samples[i:i + WINDOW]
        rms = (sum(s * s for s in win) / WINDOW) ** 0.5
        if rms > VOICED_RMS:
            voiced += 1
    print(f"{path}: {voiced} voiced windows -> {voiced // WINDOWS_PER_WORD} mock words")


if __name__ == "__main__":
    main(sys.argv[1])
