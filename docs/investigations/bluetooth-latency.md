# Bluetooth first-word clipping: what is actually happening

Investigation of the "Bluetooth microphones clip the first word" gap that
[`README.md:413`](../../README.md) and
[`docs/input-latency.md:27`](../input-latency.md) record as *expected 200-600ms
and unmeasured*.

**Status:** the mechanism is now measured, and it is not the one the docs
predicted. No Bluetooth device was connectable on this machine, so the
Bluetooth row is still honestly blank. What the measurement found instead is
that **the built-in microphone already has the bug**, at 238ms, and that the
daemon's own watchdog cannot see it.

Nothing in the shipped behaviour was changed. This adds a measurement
(`crates/audio/examples/device_latency.rs`) and a regression test
(`crates/audio/tests/stale_prefill.rs`).

| | |
|---|---|
| Machine | Apple M4 Pro, macOS 26.5.2 (25F84) |
| Audio stack | cpal 0.18.1, CoreAudio backend |
| Device measured | MacBook Pro Microphone (built-in), 48kHz, 1ch, 512-frame buffers |
| Bluetooth present | **No.** All paired headsets show `Not Connected` |

## Reproduce

```bash
# Every input device, with an acoustic reference tone. Turn the volume up.
cargo run --release -p audio --example device_latency

# One device; substring match, so `airpods` is enough.
cargo run --release -p audio --example device_latency -- airpods
```

Measuring a Bluetooth input needs one manual step: set **Sound > Output** to
the *built-in speakers* and **Sound > Input** to the headset. A tone played
through the headset has already paid the hands-free profile switch that is
being measured, and the run comes back flatteringly fast.

## Headline numbers

Built-in microphone, n=10 cold opens, reproduced across four independent runs:

| Measure | p50 | What it is |
|---|---|---|
| First callback | **58-62ms** | What the daemon believes and what the old probe reports |
| **Device really hearing the room** | **233-238ms** | Sustained reference tone in the capture |
| Same open, device already warm | **23-40ms** | Another stream open on the device first |

The 150ms pre-roll window is
[`SegmenterConfig::pre_roll_frames`](../../crates/audio/src/segment.rs#L66).
**238ms > 150ms**, so the built-in microphone is already past the line, on the
one device the docs currently certify as safe.

## The mechanism

Not CoreAudio device-start latency in the way the docs assumed, and not the
ring buffer or the segmenter. Three findings, in order of importance.

### 1. The first buffer after a cold open is stale

After a cold open the first buffer routinely contains audio from **before the
open**. Proven, not inferred: the reference tone was muted for 2.5 seconds and
then the stream was opened, and buffer 1 still arrived full of tone in **14 of
15 runs**. Tone that had not been playing for 2.5s cannot have been captured
after the open.

The shape of one cold open, one column per ~10.7ms buffer:

```
run 1: #               +#######################
run 2: #               ++######################
run 3: #.              ++######################
       ^               ^
       stale prefill   live audio starts (~230ms)
       (~60ms)
       legend: '#' full tone  '+' partial  '.' faint  ' ' nothing
```

So the sequence is: one buffer of pre-open audio, then a **~150ms dead notch**
where the device delivers punctual buffers of nothing, then live audio.

### 2. The daemon's watchdog stops its clock on that stale buffer

[`StartupWatch::on_first_audio`](../../crates/outloud/src/devlatency.rs#L84)
measures open to *first chunk*, and
[`pipeline.rs:396`](../../crates/outloud/src/pipeline.rs#L396) calls it on the
first `FrontendEvent::Chunk`. That first chunk is the stale buffer.

The watchdog therefore records **58ms** where the truth is **238ms**, compares
58ms against its 150ms threshold at
[`devlatency.rs:89`](../../crates/outloud/src/devlatency.rs#L89), concludes
`Verdict::Fine`, and stays silent. The safeguard built for exactly this failure
is being fed the one number that cannot detect it, and it fails *quiet*.

This is the most important finding: it means the existing mitigation
(docs/input-latency.md option 1, "measure it and warn") is already shipped and
already not working.

### 3. Latency is device startup, not stream setup

Phase decomposition of a cold open, cumulative from T0:

| Phase | Cumulative | Call |
|---|---|---|
| enumerate | 0.5-1.6ms | `device.description()` |
| config | 0.7-2.3ms | `default_input_config()`, [`capture_cpal.rs:198`](../../crates/audio/src/capture_cpal.rs#L198) |
| build | 15-22ms | `build_input_stream()`, [`capture_cpal.rs:211`](../../crates/audio/src/capture_cpal.rs#L211) |
| play | 45-55ms | `stream.play()`, [`capture_cpal.rs:164`](../../crates/audio/src/capture_cpal.rs#L164) |
| first callback | 55-62ms | stale |
| **live audio** | **~235ms** | |

Everything this process controls is done by 55ms. The remaining ~180ms is the
hardware capture chain converging, and it belongs to the device.

The warm comparison isolates it: with another stream already open, a second
cold open reaches live audio in **23-40ms**. So **~198ms of the 238ms is the
device starting up**, and it is exactly the part a warm-held stream removes.

### What it is not

- **Not the ring buffer.** [`ring.rs`](../../crates/audio/src/ring.rs) only
  drops on overrun, counts what it drops, and is 10s deep as configured at
  [`source.rs:122`](../../crates/outloud/src/source.rs#L122). It never saw
  this audio.
- **Not the segmenter's pre-roll.** Pre-roll recovers audio captured late. This
  audio was never captured. `docs/input-latency.md:39` already makes exactly
  this distinction, and it is correct.
- **Not HFP profile switching**, at least not here: this is a built-in
  microphone with no Bluetooth involved. Profile switching is likely to *add*
  to this on a headset, not replace it.

### Why users see it as bad recognition

`docs/input-latency.md:52` already measured the consequence through Apple
`SpeechTranscriber`: 200ms of lost head turns "quick" into "**Like**". A
missing word is visible; a plausible wrong word survives proofreading. The
built-in microphone sits at 238ms, which is in that band.

## Proposals, ranked by user-visible benefit against risk

### 1. Fix the watchdog to measure audio, not buffers — *do this first*

**Benefit:** high. **Risk:** low. **Changes shipped behaviour:** only by making
an existing warning fire when it should.

The watchdog is already wired, already has a per-device warn-once policy, and
already has the right user-facing message. It is simply reading the wrong
signal. Options, cheapest first:

- Have `StartupWatch` stop its clock on the first chunk whose RMS is above the
  VAD's silence floor, rather than the first chunk of any kind. This costs one
  RMS over a buffer already in cache.
- Or discard the first chunk of an utterance from the measurement, since it is
  demonstrably stale in 10/10 runs.

The first is more honest: it measures "audio with content in it", which is what
the threshold at
[`devlatency.rs:89`](../../crates/outloud/src/devlatency.rs#L89) was always
meant to compare against.

**Verification.** `crates/outloud/src/devlatency.rs` already has unit tests
with synthetic timings; add cases feeding a stale-then-silent-then-live chunk
sequence and assert `SlowFirstSample`, where today the same sequence yields
`Fine`. Then run the daemon on the built-in microphone and confirm the warning
appears once, quoting a number near 238ms rather than 58ms. The measurement in
`device_latency.rs` supplies the expected value.

### 2. Widen the pre-roll — *does not fix this, and the docs already say so*

**Benefit:** none for this bug. **Risk:** low but pointless.

`docs/input-latency.md:88` already rejects this and is right: pre-roll cannot
buffer audio that was never captured. Listed only so it is not re-proposed.

**Verification.** None needed; already disproven. If someone wants the
evidence, `crates/audio/examples/latency_impact.rs` models it.

### 3. Device-class-aware pre-roll — *wrong shape for the problem*

**Benefit:** none, for the same reason as 2. Also worth recording that the
enabling mechanism does not exist: cpal 0.18.1's CoreAudio backend only ever
sets `InterfaceType::Aggregate`
(`cpal-0.18.1/src/host/coreaudio/macos/device.rs:424`) and never
`InterfaceType::Bluetooth`, so a device class would have to be inferred from
the device *name* today. Any per-class policy would rest on string matching
against "AirPods".

**Verification.** `device_latency.rs` prints the reported `interface_type` per
device, so this claim is checkable in one command and will self-correct if cpal
improves.

### 4. Warm-hold, per device, opt-in and visible — *the only real fix, and it has a real cost*

**Benefit:** high; 238ms falls to ~23-40ms, measured. **Risk:** high, and the
risk is not technical.

[`crates/outloud/src/mic.rs:1-20`](../../crates/outloud/src/mic.rs) documents
the deliberate choice this trades against: the microphone is opened on key-down
and closed on commit so that macOS's orange recording indicator means *exactly*
"dictating right now". The module is explicit that "trust me, the samples are
discarded" is the kind of claim this product refuses to make.

**This should not be turned on globally or silently.** If it is done at all, it
must be:

- **off by default**, so the shipped promise is unchanged for anyone who does
  not ask;
- **per device**, enabled only for devices *measured* to exceed the pre-roll
  window, rather than as a blanket setting;
- **visible in the UI while it is happening**, so the recording indicator's
  meaning is restated rather than quietly broken;
- **bounded**, e.g. released after a short idle period, so "warm" does not
  become "all day".

`docs/input-latency.md:85` already frames it this way. The new evidence is that
the benefit is real and large (~198ms), which was previously an assumption.

A cheaper variant worth considering first: keep the stream open only across the
*commit tail* of an utterance and the seconds immediately after, which covers
the common case of dictating several sentences in a row without holding the
device open while the user is idle.

**Verification.** Three checks, all mechanical:
- `crates/audio/examples/device_latency.rs` already prints the warm-versus-cold
  comparison per device; it is the benefit number.
- Extend `crates/audio/tests/shared_device.rs`, which already guards the
  device-sharing properties, with a test that a warm-held stream still does not
  take hog mode and still does not reconfigure the device's sample rate. Those
  are the two ways a longer-lived stream could break the Discord/FaceTime story
  in `README.md:390`.
- A test asserting the mic is closed within the bounded idle period after the
  last utterance, so the privacy property degrades by a stated amount rather
  than indefinitely. `mic.rs`'s existing tests are the right home.

### 5. Document the real number for the built-in microphone

**Benefit:** moderate; it corrects a table that currently certifies a device as
safe when it is not. **Risk:** none.

`docs/input-latency.md:26` says the built-in microphone is 71ms and "Inside the
pre-roll window. Safe." That is the first-callback number, and the callback is
stale. The honest row is 238ms to live audio, with 58ms noted as the buffer
time so the discrepancy is explained rather than looking like a contradiction.

**Verification.** Re-run `device_latency` and check the table matches. That is
the same standard `docs/input-latency.md:3` already sets for itself.

## Top recommendation

**Do proposal 1.** The project already decided how to handle slow devices,
already built the mechanism, already wrote the user-facing message, and shipped
it wired to a signal that cannot detect the failure. Fixing what the watchdog
measures turns an existing silent no-op into a working safeguard, changes no
shipped behaviour beyond making a warning correct, and is verifiable by unit
test plus one command on real hardware.

Proposal 5 should ride along with it, since the doc correction is the same
finding written down.

Proposal 4 is the only thing that removes the latency rather than reporting it,
and it should not be started until someone has decided how to pay for it in the
privacy story. That is a product decision, not an engineering one, and
`mic.rs` argues the other side well enough that it deserves an explicit answer.

## Confidence

**High** on the built-in microphone measurement. It reproduced across four
independent runs with n=10, p50 stable at 233-238ms and spread under 12ms, the
detector is calibrated per device against an A/B of the tone muted and playing
(tone/room ratio ~200:1), and the stale-prefill finding was confirmed by an
independent experiment (mute for 2.5s, then open) rather than only by
inference.

**High** on the diagnosis that the watchdog under-reports. It follows directly
from the stale-buffer result plus the code path at
`pipeline.rs:396` -> `devlatency.rs:84`, and both numbers were measured.

**None on Bluetooth**, and this must not be over-read. **No Bluetooth device
was connected to this machine**, so every headset figure in `README.md:413` and
`docs/input-latency.md:27` remains unmeasured. The claim supported here is
narrower and, for the project, more uncomfortable: the first-word problem is
not exclusively a Bluetooth problem, it is already present on the built-in
microphone, and the existing detector cannot see it. The Bluetooth number
should be expected to be *worse* than 238ms, since HFP negotiation adds to
device startup rather than replacing it, but that is a prediction and is
labelled as one.

### What was not checked

- Any Bluetooth, USB, or wired external device. None were available.
- Whether the stale prefill is a cpal artifact or CoreAudio's own behaviour. It
  was measured through cpal, which is what the daemon uses, so the number is
  right for this codebase either way, but the attribution is unconfirmed.
- Whether an aggregate or virtual device (Loopback, BlackHole) behaves
  differently.
- End-to-end recognition impact of the corrected number. The transcript table
  in `docs/input-latency.md:52` was taken from prior work and reused, not
  re-measured here.
- Whether a warm-hold actually survives a device hotplug mid-warm, which the
  supervisor loop at `capture_cpal.rs:176` would need to handle.
