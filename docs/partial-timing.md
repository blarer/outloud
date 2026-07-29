# Partial timing: what the overlay actually has to work with

Measured on an M4 Pro, macOS 26.5, with `scripts/measure-partials.py` feeding
a 4.96s synthesized clip to the helper at real-time pace.

## The numbers

```
      at  kind     text
    0.05s  ready
    1.14s  partial  The
    1.14s  partial  The dog
    1.14s  partial  The dog is
    1.14s  partial  The dog is brown
    2.06s  partial  The dog is brown and
    2.06s  partial  The dog is brown and has
    ...
    5.16s  partial  ...every single morning before breakfast
    5.23s  final
```

| Measure | Value |
|---|---|
| First partial | **1.14s** after audio starts |
| Burst interval | 0.92s, 0.99s, 1.04s, 1.07s |
| Words per burst | 4-5, arriving within **1ms** of each other |
| Partials after audio ended | 4 of 21 |

This confirms the figure recorded in `transcriber.swift` (~1.3s) and sharpens
it: the cadence is closer to **1.0s**, and the burstiness is total. Words do
not trickle in; four or five land in the same millisecond and then nothing
happens for a second.

## What this means for the overlay

**The word cascade is correctly tuned.** At 55ms per word a five-word burst
finishes staggering in 220ms, comfortably inside the ~1.0s gap before the next
batch, so the cascade always completes and never becomes a backlog. It was
built for exactly this shape of arrival, and the shape is now confirmed rather
than assumed.

**The remaining un-fluid feeling is the 1.14s to the first word, and it is not
ours.** Between key-down and the first partial the overlay has nothing to show
but the skull and the level meter. Our own contribution to that gap is
negligible: the segmenter's hangover is an *end*-of-speech tunable, and the
recognizer worker forwards chunks as they arrive.

## What was already tried

`transcriber.swift` requests both `.volatileResults` and `.fastResults`, and
its comment records the measurement that made `fastResults` non-negotiable:
without it, 17 partials arrived within 10ms of each other *after* the whole
utterance finished, so the overlay sat frozen and then dumped the sentence.
That is the catastrophic version of this same behaviour, already fixed.

There is no further public knob. The cadence is internal to
`SpeechTranscriber`.

## Options, honestly weighed

1. **Accept it.** 1.14s of skull-and-meter before words appear. The meter does
   move with the voice, so the overlay is not dead, it is just wordless.

2. **A faster streaming model in parallel.** The research recommended a
   two-stage pipeline for precisely this reason: something like Moonshine
   produces partials in 150-250ms and would fill the first second, with
   SpeechTranscriber still finalizing. `crates/asr` already has the two-stage
   seam. This is the real fix, and it is a model-integration project rather
   than a tuning change.

3. **Show something else during the gap.** Not more words, since there are
   none, but the overlay could make the wait legible instead of blank.

## Reproducing

```bash
say -o /tmp/clip.aiff "your sentence here"
afconvert -f WAVE -d LEI16@16000 -c 1 /tmp/clip.aiff /tmp/clip.wav
python3 scripts/measure-partials.py /tmp/clip.wav
```

Two traps the script now handles, both of which produced badly wrong numbers
before being fixed:

- The helper's stdin is **little-endian f32**, not the int16 a WAV carries.
  Feeding int16 yields a stream the analyzer accepts, hears nothing in, and
  returns zero partials from, which looks exactly like a broken recognizer.
- Pacing with a flat `sleep` per chunk accumulates the cost of each write, and
  stretched this 4.96s clip to 11s, roughly doubling every reported latency.
  The script now paces against the wall clock.
