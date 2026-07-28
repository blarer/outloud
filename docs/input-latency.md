# Input device latency

Measured, not estimated. Reproduce with:

```bash
cargo run --release -p audio --example first_sample_latency   # your device
cargo run --release -p audio --example latency_impact         # what it costs
```

## The thing that makes this matter

The daemon opens the microphone on key-down and closes it on commit
([`crates/outloud/src/mic.rs`](../crates/outloud/src/mic.rs)). That is a
deliberate privacy decision: macOS's orange recording dot then means exactly
what a user thinks it means, rather than being lit all day while the daemon
discards samples.

The cost is that **stream startup is paid on every utterance**, not once at
launch. Whatever a device takes to deliver its first sample lands directly
between the keypress and the first audio anything can see.

## Measured: time from stream open to first sample

| Device | p50 | Verdict |
|---|---|---|
| MacBook Pro Microphone (built-in) | **71ms** | Inside the pre-roll window. Safe. |
| Bluetooth (AirPods, hands-free profile) | not yet measured | Expected 200-600ms; see below |

The built-in number is n=10, min 67ms, max 74ms: tight and predictable.

The Bluetooth row is honestly blank. Opening a capture stream on an AirPod
forces the headset into its hands-free profile, which is a negotiated link
change rather than a buffer allocation, and published figures for that
negotiation are in the hundreds of milliseconds. It has not been measured on
this hardware, so it is not written down as though it had been.

## Why the pre-roll ring does not save us

The segmenter keeps 150ms of pre-roll
([`SegmenterConfig::pre_roll_frames`](../crates/audio/src/segment.rs)) so that
a word already in progress when speech is *detected* is still captured whole.

That solves a different problem. There are two failures and only one is
recoverable:

1. **Audio arrives late but exists.** The ring holds it. Nothing is lost.
2. **Audio was never captured.** The device had not started when the user began
   speaking. No downstream buffer can recover samples the hardware never took.

Device startup latency is case 2. The pre-roll is irrelevant to it.

## Measured cost, through the real recognizer

Same utterance, head truncated to model a device that started late, run through
Apple `SpeechTranscriber`:

| Lost at head | Transcript |
|---|---|
| 0ms | "The quick brown fox jumps over the lazy dog." |
| 100ms | "Quick brown fox jumps over the lazy dog." |
| 200ms | "**Like** brown fox jumps over the lazy dog." |
| 300ms | "Brown fox jumps over the lazy dog." |
| 500ms | "fox jumps over the lazy dog." |

The 200ms row is the worst outcome in the table, and it is not the one that
lost the most audio. A partially-captured word is not dropped, it is
**misrecognised**: "quick" became "Like". A user can see that a word is missing
and retype it. A plausible wrong word inserted into their document is the kind
of error that survives proofreading.

## Current handling: none

There is no device-latency handling anywhere in the daemon. No warm-up, no
per-device profile, no warning. On the built-in microphone this is invisible
and fine. On Bluetooth it will silently corrupt the first word of every
utterance, and the user will read it as "this thing mishears me" rather than
"my headset is slow".

## Options, cheapest first

1. **Measure the device once, warn if slow.** The probe already exists. Run it
   on device change; if first-sample latency exceeds the pre-roll window, tell
   the user their device clips word onsets and suggest holding the key briefly
   before speaking. Costs nothing and removes the mystery.
2. **Per-device warm-hold.** Keep the stream open between utterances *only* for
   devices measured as slow. This trades the privacy property above, so it must
   be per-device and visible in the UI, not a silent global default.
3. **Widen pre-roll.** Does not help. Pre-roll cannot buffer audio that was
   never captured.
4. **Pre-warm on hotkey press rather than on key-down commit.** Only helps if
   there is a detectable signal before the user starts speaking, which
   push-to-talk does not provide.

Option 1 is the honest minimum: the failure is currently silent, and making it
loud costs one probe.
