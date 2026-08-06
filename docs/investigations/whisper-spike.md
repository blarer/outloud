# Whisper backend spike

Branch: `spike/whisper-backend`. Machine: M4 Pro, macOS 26.5.2, cargo 1.95,
`whisper-rs` 0.16 (whisper.cpp via Metal), model `ggml-base.en.bin`.

## Summary

The whisper.cpp finalizer works end to end and is now covered by a test that
transcribes a committed audio fixture and asserts on the words, instead of
only on buffering arithmetic. The measured finalize cost is ~205ms for a
2.53s utterance, which sits just outside the 200ms finalize budget in
`docs/asr-integration.md` and inside the 550ms speech-end-to-committed-text
budget. Streaming partials remain impossible with this engine, and the
measurements below say why in numbers rather than by assertion.

## What already existed

The backend was not a stub. `crates/asr/src/backends/whisper_cpp.rs`
already implements `Recognizer` over `whisper-rs`, behind the default-off
`whisper` cargo feature, with `whisper-cuda` / `whisper-vulkan` for GPU
builds. `whisper-rs` was the right dependency and stays: it exposes the full
`whisper_full` parameter surface (sampling strategy, thread count, segment
control, logging hooks) which is everything this seam needs, and hand-rolled
FFI would buy nothing, since the streaming limitation is in whisper's fixed
30s encoder window, not in the bindings.

What was missing was proof: no audio fixture, no test that ran the model, and
no measurements to compare against the budgets.

## What works

- `cargo build -p asr` (default features) — unchanged, no whisper code
  compiled, no new dependency reaches the default build.
- `cargo build -p asr --features whisper` — needs cmake; see the blocker
  below.
- `cargo test -p asr` — 14 passed, 3 ignored.
- `cargo test -p asr --features whisper` — 13 unit + the new integration test
  `tests/whisper_transcribes_testdata.rs`, which feeds
  `crates/asr/testdata/quick-brown-fox.wav` in 200ms chunks (the shape the
  segmenter delivers), asserts the transcript equals
  `the quick brown fox jumps over the lazy dog` case- and
  punctuation-insensitively, asserts no partials are emitted, asserts the
  reported `audio_secs`, and asserts `finalize` resets the instance.
- `cargo clippy -p asr --features whisper --all-targets` — clean.

Weights are not committed. The test resolves a model from
`$OUTLOUD_WHISPER_MODEL`, else `~/.outloud/models/whisper-base.en` (where
`asr::models::fetch` puts the `whisper-base.en` registry entry). With
neither present it prints the fetch command and passes, so a fresh clone
stays green:

```bash
mkdir -p ~/.outloud/models
curl -L -o ~/.outloud/models/whisper-base.en \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin  # 142MiB
cargo test -p asr --features whisper
```

The fixture was generated deterministically, per the benchmark methodology in
`docs/asr-integration.md`, and is 85KB of 16kHz mono 16-bit PCM:

```bash
say -o /tmp/fox.aiff "The quick brown fox jumps over the lazy dog."
afconvert -f WAVE -d LEI16@16000 -c 1 /tmp/fox.aiff \
  crates/asr/testdata/quick-brown-fox.wav
```

## Measured latency

Release build, `cargo run --release -p asr --features whisper --example
transcribe`, timestamps taken at the caller around `feed` + `finalize`.

| Input | Audio | Model load | Finalize | Realtime factor |
|---|---|---|---|---|
| `testdata/quick-brown-fox.wav` | 2.53s | 48-56ms | **203-211ms** | 12.0-12.4x |
| longer synthesized utterance | 7.19s | 49ms | **247ms** | 29.2x |

Debug build finalizes the same 2.53s clip in 280ms, so the release/debug gap
is small: the time is inside whisper.cpp, not in our Rust.

Against the budgets:

| Budget (`docs/asr-integration.md`) | Target | Measured |
|---|---|---|
| Endpoint -> final transcript (5s audio) | ≤ 200ms | 203-247ms — **marginally over** |
| Speech end -> committed text | ≤ 550ms | ~505ms (300ms hangover + ~205ms) — inside |
| Speech onset -> first partial | ≤ 250ms | **n/a, this backend emits none** |

`docs/asr-integration.md` quotes 222-233ms for base.en on an M4 Pro against a
4.96s recording; 203-211ms here for 2.53s is the same engine on the same class
of machine, so that table is confirmed rather than revised. Note the shape:
finalize cost is nearly flat in utterance length (2.53s and 7.19s differ by
40ms), because whisper always encodes a padded 30s window. Realtime factor is
therefore a misleading metric for short dictation; the fixed ~200ms floor is
the number that matters.

Compared with `docs/latency.md`, the OS-integration path is not the problem:
a warm `snapshot_focused` is ~155us, three orders of magnitude under this
backend's floor. The recognizer owns essentially the whole budget.

## Truncation at the 30s window, now asserted

Feeding 40s of audio (the 2.53s fixture plus silence) returns the fixture's
words, not silence: the backend keeps the START of an over-long utterance.
That was documented and unasserted until `an_over_long_utterance_keeps_the_beginning`
in `crates/asr/tests/`. `audio_secs` still reports the full 40s, because it
describes what the user said; the truncation is a separate fact on stderr.

## What is still missing for streaming partials

Whisper cannot do them, and the pseudo-streaming workaround (re-decode a
growing window on each chunk) fails on both cost and quality. Measured by
truncating the fixture and finalizing each prefix:

| Window | Finalize | Hypothesis |
|---|---|---|
| 0.6s | 205ms | `The Quick Brown.` |
| 1.2s | 202ms | `The Quick Brown Fox Church` |
| 1.8s | 238ms | `The quick brown fox jumps over the line.` |
| 2.5s | 224ms | `The quick brown fox jumps over the lazy dog.` |

Two conclusions:

1. **Cost.** Every re-decode costs the same ~200ms regardless of how much
   audio is in the window, because the encoder input is fixed-size. The
   partial cadence budget is 150ms, so pseudo-partials cannot keep up even in
   principle, and each one burns a full decode of work that the next one
   throws away.
2. **Quality.** Mid-window hypotheses invent endings (`Fox Church`,
   `over the line`) because the decoder completes a sentence it has not heard.
   Under the whole-hypothesis replace semantics the user watches words appear
   and then change to different words, which reads worse than a blank overlay.

So the streamer slot stays empty for whisper, exactly as
`docs/asr-integration.md` says: it belongs to Moonshine or a sherpa-onnx
Zipformer (M1 weeks 9-12). Off macOS, until that lands, dictation is
finalize-only: nothing appears until end of speech, then the whole utterance
at once. On macOS the Apple backend already streams partials (first partial
1.14s, ~1.0s bursts, `docs/partial-timing.md`), so whisper is the fallback,
not the default.

Also still open, in rough priority order:

- No word timings. `finalize` returns `words: []`; token-level timestamps
  cost another decode pass and no consumer needs them yet.
- The 30s encoder window truncates from the start with only a stderr line;
  the daemon's hot-mic timeout should close capture before that, but nothing
  asserts it.
- ~~`sha256` for `whisper-base.en` is still `None`~~ — pinned, along with
  silero-vad and parakeet, and cached files are now verified once against
  their pin rather than trusted for existing. CI fetches and verifies the
  model on every run (`scripts/ci-whisper.sh`).
- Non-Metal builds are the real latency cliff, not the model size: the
  Windows CPU measurement in `docs/asr-integration.md` is 8970ms for the same
  work. Nothing in the build output warns about it.

## Blocker hit, and how it was cleared

`cargo build -p asr --features whisper` failed because whisper-rs builds
whisper.cpp from source and cmake was not installed on this machine:

```
which cmake -> (nothing)
```

Cleared with `brew install cmake` (4.4.2); the feature build then succeeded in
18.9s with Metal enabled automatically. This is the documented cost of the
feature being off by default, and it is why the default build must stay
whisper-free. On Windows the equivalent list is longer (LLVM, MSVC Build
Tools, CUDA Toolkit) and is already documented in `docs/asr-integration.md`.
