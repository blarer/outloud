# Whisper on CPU: where the time goes, and why `audio_ctx` is not the fix

Measured 2026-10-30 on an M-series Mac, `whisper-base.en` (148MB, the model
the daemon fetches on first use), release build with `--features whisper`.
Corpus of 8 synthesized utterances from 0.5s to 7s, `best-of-5` per utterance
so a scheduler hiccup does not become a finding.

This exists because the CPU path is the one a user without a GPU gets, and the
only number anyone had for it was **8970ms**, measured once on a Windows box
with a different model. That number is real but it is not the number to
optimize against on this machine, and optimizing against the wrong number is
how you ship a regression.

## The measurement that matters

| Utterance | Audio length | Finalize (best of 5) |
|---|---|---|
| "Yes." | 0.6s | 244ms |
| "A short one." | 1.0s | 249ms |
| "Change quick to slow." | 1.5s | 241ms |
| "The quick brown fox jumps over the lazy dog." | 2.7s | 254ms |
| "Please send the report to Sarah by Friday afternoon." | 3.4s | 259ms |
| "The meeting is at three o'clock in the main conference room." | 4.0s | 270ms |
| "I think we should refactor the authentication middleware..." | 5.2s | 271ms |

**Latency is flat.** A 0.6s utterance and a 5.2s utterance both finalize in
about 250ms, a 9x difference in audio for an 11% difference in time. That is
the whole finding: the cost is not proportional to what the user said.

The reason is whisper's architecture. The encoder operates on a fixed 30-second
mel window, so a 0.6s utterance is padded with 29.4 seconds of silence and the
encoder does the same work either way. For a dictation tool, where a typical
utterance is a sentence, **almost all of the encoder's work is on silence.**

Two consequences worth stating plainly:

- There is a ~240ms floor on this machine that no amount of threading touches,
  because it is not a parallelism problem.
- The headline 8970ms Windows CPU number is not this phenomenon. That is a
  different model on different hardware, and it should be re-measured the same
  way before anyone reasons about it.

## `audio_ctx` is the documented lever, and it fails badly

whisper.cpp exposes `audio_ctx` to shorten the encoder's context so it does not
process the full padded window. It is the known trick for exactly this problem,
and upstream uses it for streaming. It was tried here and **must not be
shipped.**

Scaling it to the utterance (`frames/2 + margin`, clamped) produced this:

| Utterance | Baseline | With `audio_ctx` | Text |
|---|---|---|---|
| "A short one." | 249ms | **2286ms** | `a a a a a a a a a A a A short` |
| "Yes." | 244ms | **1663ms** | `Yes Yes YES Yes Yes Yes yes Yesige` |
| "Change quick to slow." | 241ms | 74ms | `Change quick to slow. Change quick to slow.` |
| "Undo that." | 239ms | 69ms | `do that` |

So on the shortest utterances it is **9x slower**, not faster, and the model
degenerates into repeating a token until it hits a limit. On the ones that did
get faster, words were dropped (`Undo that.` became `do that`) or duplicated.

The mechanism: truncating the encoder context does not tell the decoder there
is less to decode. The decoder still expects a full window's worth of
alignment, finds nothing to attach to in the truncated region, and loops
emitting its best guess until a stop condition fires. The repetition and the
slowness are the same bug: a decoder that will not terminate.

**The trap worth recording.** The first measurement of this looked like a clear
3.5x win: 361ms before, ~100ms after, text apparently intact. Both halves of
that were wrong.

- The 361ms "baseline" was a **cold start**. First-ever run in a process pays
  model load and allocator warm-up; steady state on the same file and the same
  binary is ~85ms best-of-5. Comparing a cold baseline against a warm
  experiment manufactured most of the improvement.
- The "intact" text had silently lost its first word. `The quick brown fox`
  became `quick brown fox`, which reads as plausible output and would not be
  noticed by a human skimming a log line.

It took a multi-utterance corpus that asserts on **text as well as time** to
see either problem. A single-utterance latency check would have shipped it.
That is why `bench.sh` in this investigation prints the transcript next to the
milliseconds and flags any mismatch: a knob that halves latency and drops a
word is a regression wearing a win's clothing.

## What to try instead, in order

**Not trimming silence, which was the obvious idea and is already done.**
Writing this document, the first recommendation was to hand whisper a
VAD-trimmed buffer so it had less silence to encode. Measuring the corpus
killed that idea before it was written as advice: the test files carry only
**0.21-0.22s** of leading and trailing silence against 0.44-2.45s of speech.
There is almost nothing to trim, and the pipeline's segmenter is already
responsible for that. The 30-second padding whisper pays for is **internal to
the model**, added inside whisper.cpp after it receives the buffer, so no
amount of trimming at our end reduces it. Anything that changes it has to
change the encoder's context, which is `audio_ctx`, which is the thing that
does not work.

That leaves options which are all real work rather than a knob:

1. **Land Parakeet** (`crates/asr/src/backends/parakeet.rs`, still a stub).
   It is not a fixed-30s-window architecture, so the flat-cost problem does
   not exist there. The roadmap already picks it as the finalizer (R-03) for
   accuracy reasons; this is an independent argument for the same decision,
   and on this evidence it is the only one of these that addresses the cause.
2. **A smaller or more quantized model**, measured on this corpus rather than
   assumed. `base.en` is already small; `tiny.en` is the test. This does not
   fix the flat cost, it just lowers the floor, and the open question is
   whether the accuracy loss is visible on real dictation.
3. **Hide the latency instead of removing it.** 250ms is inside the roadmap's
   p50 budget of 500ms. If the floor cannot move, the streaming partials tier
   (R-05) is what makes it stop mattering, because the user sees words before
   the finalizer runs.
4. **Re-measure the Windows 8970ms number** with this method before treating
   it as the CPU story. One cold run with a different model is not a baseline,
   and cold start alone was a 4x error in this investigation.

## Reproducing

`cmake` is required to build `--features whisper` (whisper.cpp is compiled
from source) and is not installed on this Mac by hand, because software here
is nix-declared. The supported way in:

```bash
nix shell nixpkgs#cmake --command \
  cargo build --release -p outloud --features whisper
```

Then, with a model at `~/.outloud/models/whisper-base.en`:

```bash
OUTLOUD_NO_INJECT=1 ./target/release/outloud \
  --once --asr whisper --wav crates/asr/testdata/quick-brown-fox.wav --no-overlay
```

`OUTLOUD_NO_INJECT=1` matters: without it every measurement types into
whatever window has focus.

Two cautions for anyone repeating this:

- **Discard the first run.** Cold start is 2-4x steady state and will dominate
  a small sample.
- **Watch system load.** Four consecutive identical corpus runs on this machine
  totalled 2254, 2097, 2014 and 2008ms under a load average of 9.2: about 10%
  spread with nothing changed. A claimed improvement smaller than that is
  noise, and `best-of-N` rather than mean is what keeps a single descheduled
  run from inventing a regression.
