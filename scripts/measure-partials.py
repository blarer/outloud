#!/usr/bin/env python3
"""Measure when SpeechTranscriber actually emits partials.

The overlay can only be as fluid as the words reaching it. A previous
investigation recorded "first at ~1.3s of a 3.3s clip, then every ~1.3s",
which would mean the user watches an empty lane for over a second before
anything appears. That number decides whether the remaining un-fluid feeling
is ours to fix or Apple's, so it is worth measuring rather than inheriting.

Feeds a WAV to the helper at real-time pace (a burst would let the analyzer
run ahead of the clock and report timings no live speaker could produce) and
prints the wall-clock offset of every event.
"""

import json
import subprocess
import sys
import time
import wave

HELPER = "crates/asr/helper/outloud-speech-helper"
CHUNK_MS = 100


def main(path: str) -> int:
    with wave.open(path, "rb") as w:
        assert w.getframerate() == 16000, w.getframerate()
        assert w.getnchannels() == 1, w.getnchannels()
        raw = w.readframes(w.getnframes())
        duration = w.getnframes() / 16000.0

    # The helper's contract is little-endian f32 mono 16kHz, not the int16 a
    # WAV carries. Feeding int16 produces a silent, valid-looking stream: the
    # analyzer accepts it, hears nothing, and emits zero partials, which looks
    # exactly like a broken recognizer rather than a format mismatch.
    import array
    import struct

    pcm16 = array.array("h")
    pcm16.frombytes(raw)
    frames = struct.pack(f"<{len(pcm16)}f", *(v / 32768.0 for v in pcm16))

    print(f"clip: {duration:.2f}s, feeding at real-time pace\n")

    proc = subprocess.Popen(
        [HELPER],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=False,
    )

    start = time.monotonic()
    events = []

    # Reader thread: stdout is line-framed JSON, one event per line.
    import threading

    def read():
        for line in proc.stdout:
            t = time.monotonic() - start
            try:
                ev = json.loads(line)
            except Exception:
                continue
            events.append((t, ev))

    reader = threading.Thread(target=read, daemon=True)
    reader.start()

    # 4 bytes per sample, because the stream is f32. Using 2 here (the WAV's
    # int16 width) advanced half a chunk per tick and fed the clip at a
    # fraction of real time, which stretched every reported timing.
    bytes_per_chunk = int(16000 * CHUNK_MS / 1000) * 4
    # Pace against the wall clock rather than sleeping a fixed interval per
    # chunk: the write and flush themselves take time, so a flat sleep
    # accumulates drift and stretches a 5s clip to 11s, making every reported
    # latency look far worse than a live speaker would ever see.
    for n, i in enumerate(range(0, len(frames), bytes_per_chunk)):
        proc.stdin.write(frames[i : i + bytes_per_chunk])
        proc.stdin.flush()
        target = (n + 1) * CHUNK_MS / 1000.0
        behind = target - (time.monotonic() - start)
        if behind > 0:
            time.sleep(behind)

    spoke_until = time.monotonic() - start
    proc.stdin.close()
    proc.wait(timeout=30)
    reader.join(timeout=2)

    print(f"{'at':>8}  {'kind':<8} text")
    print("-" * 72)
    prev = None
    gaps = []
    for t, ev in events:
        kind = ev.get("type", "?")
        text = ev.get("text", ev.get("message", ""))
        if kind == "partial":
            if prev is not None:
                gaps.append(t - prev)
            prev = t
        print(f"{t:8.2f}s  {kind:<8} {text[:52]}")

    print()
    print(f"audio finished feeding at {spoke_until:.2f}s")
    firsts = [t for t, e in events if e.get("type") == "partial"]
    if firsts:
        print(f"FIRST partial at {firsts[0]:.2f}s  <-- the blank-overlay gap")
        print(f"partial count: {len(firsts)}")
        after = sum(1 for t in firsts if t > spoke_until - 0.05)
        print(f"partials arriving AFTER the audio ended: {after}/{len(firsts)}")
    if gaps:
        gaps.sort()
        print(f"gap between partials: min {gaps[0]:.2f}s  median "
              f"{gaps[len(gaps)//2]:.2f}s  max {gaps[-1]:.2f}s")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
