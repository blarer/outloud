"""Report peak/RMS amplitude of a 16-bit mono WAV.

Used to tell "the synthesizer produced silence" apart from "the segmenter
rejected audible speech", which look identical from `--say`'s exit message.
"""
import struct
import sys
import wave


def main(path: str) -> None:
    with wave.open(path, "rb") as w:
        n_channels = w.getnchannels()
        width = w.getsampwidth()
        rate = w.getframerate()
        frames = w.getnframes()
        raw = w.readframes(frames)

    print(f"{path}: {rate}Hz {n_channels}ch {width*8}bit {frames} frames "
          f"({frames / rate:.2f}s)")
    if width != 2:
        print("not 16-bit, no amplitude reported")
        return
    samples = struct.unpack(f"<{len(raw)//2}h", raw)
    if not samples:
        print("no samples")
        return
    peak = max(abs(s) for s in samples)
    rms = (sum(s * s for s in samples) / len(samples)) ** 0.5
    print(f"peak={peak} ({peak/32768:.3f} FS)  rms={rms:.1f} ({rms/32768:.4f} FS)")


if __name__ == "__main__":
    main(sys.argv[1])
