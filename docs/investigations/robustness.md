# Robustness investigation: silent failures and the stuck microphone

Read of `crates/outloud/src` (all), `crates/audio`, `crates/hotkey`,
`crates/asr`, `crates/text-target`, `crates/ax-edit`, `crates/config`,
`crates/diag`, `docs/macos-permissions.md`.

Ranked by likelihood x severity. Every finding carries `file:line` evidence.
Findings marked **REPRODUCED** were confirmed by running code, not by reading
it. All probes ran with `OUTLOUD_NO_INJECT=1` and none opened a microphone or
typed into a window.

The four bugs found the hard way this session (tail-drop, Discord AXValue,
`abs_range` off-by-one, VAD threshold) share a shape: **a component reports
success, or reports nothing, while losing user data.** The findings below were
selected for that same shape rather than for being merely untidy.

> **Status, checked 2026-08-01.** Most of these are fixed. This file is kept
> for the reproductions, which are the expensive part, not as a list of open
> work. Verified against the code rather than from memory:
>
> | Finding | Status |
> |---|---|
> | F-1 sub-threshold tap leaves the mic open | **Fixed** — `hot_mic_timeout_ms`, 60s cap |
> | F-2 `finalize()` blocks the event loop | Open, not reproduced since |
> | F-3 headless build does not compile | **Fixed** — `scripts/build-headless.sh` passes, and CI gates it |
> | F-4 `ignores_ax_value_writes` is unfalsifiable | **Fixed** — replaced by `accepts()`, one exhaustive decision |
> | F-5 focus changes mid-utterance, text lands elsewhere | **Fixed** — the overlay now names the app that took it |
> | F-6 device disconnect mid-utterance is silent | Open |
> | F-7 rapid key presses drop the second utterance | **Fixed** — key-down is refused while a commit is in flight, with a reason |
> | F-8 config reload mid-utterance is partly applied | Open |
>
> F-5 is worth reading if you touch text injection: it took four attempts to
> fix, and three of those were believed done while the warning was invisible.

---

## F-1. A sub-threshold tap leaves the microphone open forever — **REPRODUCED**

**Likelihood: high. Severity: critical. This is the stuck orange indicator.**

`TapHold` implements tap-to-latch: a press shorter than 300ms latches capture
on and waits for a second tap to end it.

- `crates/hotkey/src/taphold.rs:105-112` — key-up under `tap_threshold`
  transitions to `State::Latched` and emits `HotkeyEvent::Latched`.
- `crates/outloud/src/source.rs:80-81` — the bridge maps `Latched => None`,
  with the comment "capture simply continues; nothing to emit".
- `crates/outloud/src/pipeline.rs:301-313` — the mic was already opened on the
  `KeyDown` that `Pressed` produced.
- `crates/outloud/src/pipeline.rs:382-385` — `mic.close()` runs **only** on the
  `KeyUp` arm.

So a latch produces a `KeyDown` (mic opens) and then *no* bounding event. The
mic stays open, the orange indicator stays lit, and `listening` stays true
until the user happens to press the chord again.

Reproduced by driving the real `TapHold` and the real mapping from
`source.rs:75-86`:

```
frontend events after an 80ms tap: ["KeyDown", "(none)"]
capturing after tap: true
still capturing 10min later: true
```

The default chord is `right-option` (`main.rs:53`). A bare modifier is pressed
and released in well under 300ms constantly during ordinary typing —
option-shift-K, option-arrow word jumps, or simply brushing the key. Every such
brush that the matcher sees as a clean down/up pair opens the microphone and
never closes it. This is why it looked unreproducible: it is not triggered by
dictating, it is triggered by *not* dictating.

Three things make this worse than a stuck indicator:

1. **The safety net is not implemented.** `docs/ux/02-core-interaction.md:24`
   promises "in latched mode a hard silence timeout (default 60s, configurable)
   is a safety net against the forgotten hot mic", and
   `crates/config/src/schema.rs:269-274` declares `silence-timeout-ms`. That key
   is `wired: false`. `taphold.rs:126` explicitly defers the timeout to "the
   capture layer", and the capture layer never implemented it. Nothing anywhere
   in the workspace reads `silence-timeout-ms`; `grep` finds only schema, tests,
   and validation.
2. **The user is told the setting works.** The menu surfaces inert settings
   (`menuhost.rs:136-145`) only for keys the user explicitly *set*. A user
   relying on the documented default 60s timeout is never warned it does
   nothing.
3. **No state disagreement is detectable.** Nothing ever asserts
   `mic.is_open() == listening`. `Mic::is_open` (`mic.rs:77`) exists and has no
   non-test caller.

The `--once` path has the same hole from the other direction:
`pipeline.rs:425-433` closes the mic on the `auto_endpoint` commit, but the
`FrontendEvent::CaptureIssue` error arm (`pipeline.rs:439-450`) transitions to
`Error` while `listening` stays true and the mic stays open.

**Fix direction:** treat `Latched` as an opened-capture event with a deadline.
Either emit `KeyUp` on a wired `silence-timeout-ms`, or (smaller and safer) have
the pipeline own the invariant: any transition out of `Listening`, by any route,
closes the mic, plus a periodic assertion that `mic.is_open() == listening` that
closes and logs on disagreement. The invariant is one line and it makes the
entire class impossible, rather than fixing the one path we found.

---

## F-2. `finalize()` can block the event loop, so the mic stays open — **REPRODUCED**

**Likelihood: medium. Severity: critical. Second independent stuck-mic path,
and the most likely explanation for the 8-hour orphan helper.**

`AudioFeed::push` is carefully non-blocking with drop-and-count
(`recognize.rs:49-59`). `AudioFeed::finalize` is deliberately the opposite:

```rust
// crates/outloud/src/recognize.rs:61-67
/// Signal end of utterance. Uses a blocking send [...]
/// by key-release time the audio producer has already stopped, so this
/// send is off the capture-critical path.
pub fn finalize(&self) { let _ = self.tx.send(AudioMsg::Finalize); }
```

The justification is about the *producer*, and it is correct about the producer.
But the channel is `sync_channel(384)` (`recognize.rs:96`) and the relevant
question is whether the **consumer** is draining. If the recognizer thread is
blocked inside `feed()` — an Apple helper whose stdin pipe is full, a wedged OS
speech session, a stalled child — the queue stays full and this "blocking send"
blocks the supervisor task.

`commit()` calls `feed.finalize()` inline (`pipeline.rs:553`), on the event
loop, and `mic.close()` runs only *after* `commit()` returns
(`pipeline.rs:378-384`). So a wedged recognizer holds the microphone open for as
long as it stays wedged, with the overlay frozen mid-utterance.

Reproduced with a recognizer whose `feed()` never returns:

```
queued; dropped so far: 115
finalize() returned within 3s: false
elapsed 3.051245875s; mic.close() runs only after commit() returns
```

Note the counter: 115 chunks were dropped and counted honestly, and the queue
was still full, so the very next `finalize()` blocked. Drop-and-count protects
the push path and does nothing for the finalize path.

This also explains the unreproduced orphan helper documented at
`instance.rs:190-207` ("the trigger is unknown", "never reproduced across
sixteen signal-and-timing combinations"). Those experiments tested *signals*.
This path needs no signal: the daemon is alive and stuck, the helper is alive
and stuck, and the pair sits there until something kills one of them. The
`FINALIZE_TIMEOUT` guard in `apple.rs:212-243` cannot help, because the worker
thread never reaches `finalize()` — it is still inside `feed()`.

**Fix direction:** make `Finalize` undroppable *and* non-blocking, which the
current channel cannot do. Either a separate one-slot finalize signal (an
`AtomicBool` or a dedicated `sync_channel(1)` the worker checks between chunks),
or `try_send` with a bounded retry that reports and closes the mic on failure.
The invariant to preserve is the one the comment states: a lost finalize
silently swallows an utterance. The invariant it currently violates: the event
loop never blocks on a downstream stage.

---

## F-3. The headless build does not compile — **REPRODUCED**

**Likelihood: certain (it is broken right now). Severity: high.**

```
$ cargo check -p outloud --no-default-features
error[E0433]: cannot find `keys` in `targets`
   --> crates/outloud/src/inject.rs:472:52
    |
472 |  .is_some_and(text_target::targets::keys::ignores_ax_value_writes)
note: found an item that was configured out
   --> crates/text-target/src/targets/mod.rs:18:9
```

`git blame` attributes line 472 to `13b09ea`, the Discord fix. Every other
`text_target::targets::keys` reference in `inject.rs` is behind
`#[cfg(all(target_os = "macos", feature = "display"))]` (see `typing_strategy`
at `inject.rs:527-533`); this one call sits inside a `#[cfg(target_os =
"macos")]` block only, so a macOS headless build reaches it.

This matters beyond tidiness because the headless build is a *correctness gate*
that exists to keep ALSA and AppKit out of server builds
(`docs/build-and-release.md:287`, `scripts/build-headless.sh:48`), and
`docs/pre-release-audit.md:536` records this check as **pass**. The gate is
green in the audit and red on the machine, which means it stopped running or
stopped covering this crate. A gate that reports pass while broken is the same
failure class as the rest of this document.

**Fix:** widen the cfg on the guard to `all(target_os = "macos", feature =
"display")`, matching its sibling, and make the headless check cover `outloud`.

---

## F-4. `ignores_ax_value_writes` is an unfalsifiable list, and the failure is detectable

**Likelihood: high (any Electron app not on the list). Severity: high (produces
an unsendable message, silently).**

**How the list was derived:** from one user report. `git show 13b09ea` says
Discord was diagnosed from the symptom ("Enter inserted a newline instead of
sending"), and the other nine entries
(`text-target/src/targets/keys.rs:110-113`: slack, notion, obsidian, linear,
figma, spotify, signal, element, teams) were added by *category guess*, not by
testing. The commit message is candid about the tradeoff and about why it is not
"all Electron apps" (diverting an app that does not need it costs the host's
undo). But note the internal contradiction: `slack` is on the ignore list at
`keys.rs:111`, while the test at `keys.rs:516` asserts Slack takes **batched
typing** as a "GUI app", and the commit message says "VS Code and Slack handle
AXValue correctly today". Slack is simultaneously documented as fine and listed
as broken. At least one of those is wrong, and nobody can tell which without
testing.

**How a user discovers they need to be on it:** they do not. There is no
diagnostic, no menu row, no log line. The failure presents as "OutLoud typed my
message but I cannot send it", which the user will attribute to their chat app.
`docs/compat-matrix.md:67` documents Discord specifically; an unlisted app gets
nothing.

**Is detection possible instead of a list? Yes, and cheaply.** The comment at
`keys.rs:102-104` says "the accessibility API gives no way to ask 'will this
write reach your model'". That is true as a *query*. But the write is already
followed by everything needed for an *observation*:

1. `insert_with_fallback` already holds the pre-write snapshot
   (`inject.rs:444`), including `value` and `selection`.
2. It computes the exact expected post-write value (`spliced_at_caret`,
   `inject.rs:563-596`).
3. `ax_edit::snapshot_focused()` can be called again after the write for ~134us
   warm (`inject.rs:279`, `docs/latency.md`).

So: write, re-read, compare. Two signals distinguish "landed" from "swallowed",
and the Discord report names both:

- **Value mismatch.** Re-read `value` differs from the spliced value we wrote.
- **Caret at zero.** The commit message states the tell exactly: "the caret sat
  at offset zero". After a successful splice the caret should be at or after the
  insertion point. A caret that snapped to 0 while the value looks right is the
  React-reconciliation signature.

On mismatch, undo is not at risk (the write did not take effect in the app's
model), so falling through to `deliver_without_ax` is safe, and the app can be
*reported* rather than hardcoded: "Discord ignored the accessibility write;
typing instead" plus a suggestion to add it to the list. That converts a
maintained list into a self-maintaining one, keeps the list only as a
fast-path optimisation to skip the doomed first attempt, and — most importantly
— makes the failure *loud for apps nobody has tested yet*.

The cost is one extra AX read per dictation on the AX path, ~134us against a
~13ms write. That is under 1%.

---

## F-5. Focus can change mid-utterance and text lands in the wrong app

**Likelihood: medium. Severity: high (text goes somewhere the user did not
intend, possibly a password field or a chat window).**

The mode and snapshot are taken at key-down (`pipeline.rs:281-283`). The write
happens at commit, potentially seconds later, and re-resolves focus:

- Buffered dictation: `insert_with_fallback` calls `ax_edit::snapshot_focused()`
  fresh (`inject.rs:444`) — whatever is focused *now*.
- Edit mode: `replace_selection` (`inject.rs:619-628`) writes `AXSelectedText`
  to the currently focused element. The doc comment admits it: "writing
  `AXSelectedText` at commit time replaces whatever is selected *now* [...]
  Verifying the selection is unchanged before writing is future work".

The streaming path gets this **right** — `ax_stream.rs:18-21` resolves the
element once and holds it, explicitly because "text keeps landing where
dictation started rather than spraying into whatever window took focus
mid-sentence". The buffered path, which is the default and the fallback for
every error, does not.

Realistic trigger: hold the chord, speak, and a Slack notification steals focus,
or a build finishes and raises a window. Cmd-Tab while holding a bare modifier
is also entirely possible.

**Fix direction:** capture the focused element (not just its name) at key-down,
like `AxRegion` does, and refuse the write with a named error if focus moved.
Refusing is correct here: the text is recoverable from the clipboard fallback,
whereas writing into the wrong window is not undoable.

---

## F-6. Device disconnect mid-utterance is silent and loses the rest of the sentence

**Likelihood: medium (AirPods, USB hubs). Severity: medium.**

`CaptureIssue` reaches the pipeline (`pipeline.rs:439-450`) and only raises the
`Error` state when the message contains `"no input device"` **and** the state is
`Idle`. Both conditions fail in the case that matters:

- A mid-utterance disconnect produces `DeviceChanged` (`source.rs:135`), whose
  message is "input device changed (was X); rebuilding stream" — no substring
  match.
- The state is `Listening`, not `Idle`.

So the user keeps speaking into a dead stream, sees the overlay still showing
"listening", and gets a truncated transcript with no indication anything went
wrong. `capture_cpal.rs:176-182` polls for device changes every 500ms, so up to
half a second of speech is lost before the rebuild even starts, plus the new
device's startup latency.

The `StartupWatch` machinery (`devlatency.rs`) that would notice this is only
armed on key-down (`pipeline.rs:312`), not on a mid-utterance rebuild.

**Related, smaller:** the ring's overrun counter is never surfaced.
`ring.rs:110` exposes `dropped()`, and `grep` finds exactly one caller: its own
unit test at `ring.rs:134`. So ring-level audio loss — the loss that happens
when the *drain* falls behind, as opposed to the recognizer — is counted and
never read by anything. `UtteranceReport::dropped_chunks` (`pipeline.rs:81`)
reports only the recognizer's counter.

---

## F-7. Rapid key presses: the second utterance is dropped with only a log line

**Likelihood: high (users do this constantly). Severity: medium.**

`pipeline.rs:262-268`: a `KeyDown` arriving while `in_flight.is_some()` is
refused with `eprintln!("key-down ignored, previous utterance still
committing")`. Nothing reaches the overlay, the menu bar, or the user. A bundled
launch has no terminal at all (`main.rs:216-223` makes exactly this point about
another message), so the user presses the key, speaks a whole sentence, and gets
nothing, with no feedback of any kind.

The window is not small: it spans from key-up until the recognizer's `Final`
lands, which is the ~560ms-to-900ms measured in `apple.rs:26-30`, plus injection.
Speaking two sentences in quick succession is the normal way people dictate.

The same silent refusal is duplicated in the drain loop at `pipeline.rs:363-365`.

**Fix direction:** surface it. `engine.live_detail()` already exists
(`state.rs:135`) for exactly this kind of non-state-changing advisory, and it is
what the slow-device warning uses. A queued second utterance would be better
still, but even a visible "still finishing the last one" beats silence.

---

## F-8. Config reload mid-utterance is partially applied, and sensitivity is stale

**Likelihood: medium. Severity: low-medium.**

`MenuHost::poll_file_changes` runs every frame on the render thread
(`main.rs:524`) and calls `reload()` (`menuhost.rs:100`) with no regard for
pipeline state. `reload()` pushes `enabled` into the runtime
(`menuhost.rs:164`), which the hotkey bridge reads at `source.rs:94`.

Consequences:

- A reload that sets `enabled = false` mid-utterance suppresses future
  `KeyDown`s but deliberately lets `KeyUp` through (`source.rs:92-95`, correctly
  reasoned). Combined with F-1, though: if the mic was opened by a *latch*,
  there is no `KeyUp` coming, and pausing now cannot close it.
- `sensitivity` is read once into `pipeline::Config` at startup
  (`main.rs:286-289`) and never re-read. `Config`'s own doc comment
  (`pipeline.rs:45-48`) claims it is "carried here rather than read at the VAD so
  a config reload takes effect on the next utterance without restarting
  capture". That is not true: `MenuHost::sensitivity()` (`menuhost.rs:82`) has
  no caller after startup. The user changes the sensitivity dial, the menu
  redraws showing the new value, and nothing changes until restart. A settings
  UI that lies about having applied a setting is the exact failure
  `docs/ux/05` forbids, and it is the same shape as the VAD bug already found.
- `prefer_streaming` is likewise startup-only (`main.rs:280`).

---

## F-9. Key held during model load: the mic opens before readiness is known

**Likelihood: low-medium. Severity: low.**

The buffered-capture path (`pipeline.rs:318-327`) is well designed and its state
walk on readiness (`pipeline.rs:229-244`) correctly handles both "key still
held" and "whole utterance happened during load". Two holes remain:

- If the recognizer *fails* to load while a key is held, `pipeline.rs:246-252`
  bails out of `run()` entirely. The `Mic` is dropped, and `Drop for Mic`
  (`mic.rs:126-131`) does close it, so this one is covered — but only by the
  destructor, and only because the whole task unwinds.
- `pending_listen` is set but the mic is opened *before* the readiness branch
  (`pipeline.rs:301` precedes `pipeline.rs:315`). During a first-run OS model
  download, `apple.rs:134` allows a **120 second** readiness timeout. A user who
  presses the key during that window has the microphone open, and the orange
  indicator lit, for up to two minutes while the overlay says "loading model".
  That is defensible behaviour (audio is genuinely being buffered) but it is
  another way the indicator is on for far longer than a user expects.

---

## F-10. Smaller silent-failure paths

- **`segmenter.flush()` discarded.** `pipeline.rs:549`: `let _ =
  segmenter.flush();` The comment argues the audio was already streamed. That is
  true on the `Speech` path, but `flush()` returns `None` when the state is
  `Silence { run }` (`segment.rs:122-127`) — and `run` can be 1 or 2, i.e. up to
  60ms of confirmed-but-undebounced speech in `pending` that is dropped. A very
  short utterance ("yes", "no", "stop") is exactly what lives in that window.
- **`AsrEvent::Partial` for a finished utterance is silently applied.**
  `pipeline.rs:463-473` calls `engine.live()` unconditionally. If a late partial
  arrives after `in_flight` was taken, it writes ghost text over an Idle overlay.
  The `Final` arm guards this (`pipeline.rs:475-478`, "stray final transcript
  ignored"); the `Partial` arm does not.
- **`make_recognizer().ok()` twice.** `recognize.rs:138` and `recognize.rs:164`
  discard the construction error. The pre-warm failure at :164 is invisible until
  the *next* utterance fails, at which point the error reported is from a
  different construction attempt.
- **`clip.restore()` discarded on a detached thread.** `inject.rs:756-759`
  spawns a thread that sleeps 300ms then discards the restore result. A failed
  restore silently leaves the user's clipboard containing dictated text — a
  privacy consequence, not just a usability one.
- **Overlay render failure is logged, forever.** `main.rs:513-516` logs on every
  frame at 30Hz. A persistently failing overlay produces 30 lines/second of
  stderr and never escalates, which will fill a log file and bury every other
  message.
- **`saw_illegal_transition` is never read in production.** `state.rs:148`
  exists and only tests call it. An illegal transition logs "BUG:" to a stderr
  nobody reads in a bundled launch and is otherwise invisible.

---

# Proposed state-space and property tests

The four known bugs share a structure: they are all violations of an invariant
that holds across a *sequence* of events, and every existing test drives a
single happy-path sequence. The tests below are ordered by which known bugs they
would have caught.

## T-1. The event-sequence model check (catches tail-drop, F-1, F-6, F-9)

Drive `pipeline::run` with sequences generated from the alphabet
`{KeyDown, KeyUp, Chunk(voiced), Chunk(silence), CaptureUp, CaptureIssue,
ReadyOk, ReadyErr}`, in random permutations up to length ~12, and assert
invariants after every event rather than at the end:

1. **`mic.is_open() == listening`** — catches F-1 directly, and would have
   caught it the first time anyone generated a `Latched` sequence.
2. **Audio in implies audio out.** Total voiced samples sent must equal total
   voiced samples that reached the `AudioFeed`, minus explicitly counted drops.
   *This is the tail-drop bug stated as an invariant.* The existing regression
   test (`pipeline.rs:929-985`) pins the one interleaving that was reported; the
   invariant pins all of them, including "chunks queued behind `CaptureIssue`"
   and "chunks arriving between `Final` and the next `KeyDown`", which nobody
   has tested.
3. **No illegal transitions**: `engine.saw_illegal_transition()` stays false.
4. **Every state is escapable**: from any reachable state, some event sequence
   reaches `Idle` within N steps. This is the no-absorbing-states property that
   `overlay::state` already asserts on the *table* (`state.rs` docs) but nobody
   asserts on the *engine*.
5. **Termination**: every sequence settles within a bounded time. Catches F-2.

This needs `Mic` to be a trait or to accept an injectable backend, which it
nearly is already — `Mic` is a small struct with `open`/`close`/`is_open` and
the `#[cfg(feature = "display")]` slot is the only real coupling. A
`MockMic` counting opens and closes would make invariant 1 checkable in CI on
any platform.

## T-2. Round-trip property on the splice/stream offset math (catches `abs_range`)

The `abs_range` off-by-one was found by a user, then pinned by two hand-written
cases (`ax_stream.rs:445-466`). The property that subsumes both:

> For any field value, any caret position, any sequence of `WriteCommand`s,
> simulating the writes against an in-memory string using the offsets
> `abs_range` produces must yield exactly the string the session intended.

That is a model check with a trivial model (a `String` you can apply
`(loc, len, text)` to). It requires no AX, no macOS, no display. It would have
caught the off-by-one on the first generated case with a non-empty `lead` and an
empty `applied` — which is to say, immediately, since that is the *first write
of every utterance with a joining space*.

Extend the same model to `spliced_at_caret` (`inject.rs:563`) so the buffered
and streamed paths are proven to agree, which is a claim the code makes in prose
(`ax_stream.rs:70-72`: "mirrors the buffered path's `spliced_at_caret` so both
modes join text identically") and never checks.

## T-3. Corpus-based VAD regression with a floor (catches the VAD threshold)

`vad.rs:254-272` now pins 18 measured RMS values. That is the right instinct
applied at too small a scale, and it is a fixture, not a property. Instead:

> Given a labelled corpus of real utterances (`testdata/`), at the default
> sensitivity, the fraction of speech frames scored as silence must be below X%,
> **and** the fraction of noise-floor frames scored as speech must be below Y%.

Both bounds together are what matters: the original bug was fixed by lowering
the knee, and the only thing stopping an over-correction is the noise-floor test
at `vad.rs:275-280`, which uses one synthetic value. `crates/audio/tests/noise_floor.rs`
already derives a ceiling; this pairs it with a floor derived from real speech.

Additionally, assert the property the dial *promises*: for every sensitivity
step offered in `SENSITIVITY_STEPS`, the measured speech-recall must be
monotonically non-decreasing. Monotonicity of the *knee* is already tested
(`vad.rs:205-215`); monotonicity of the *outcome* is not, and the outcome is
what the user turns the dial for.

## T-4. Write-verification differential test (catches Discord, and the next one)

Turn F-4's detection into the test:

> For a given app and a given pre-write snapshot, after `deliver`, a re-read
> snapshot must show the value we intended and a caret consistent with it.

Run against real apps as an ignored-by-default integration test (the pattern
`apple.rs:283` already uses), one case per row of `docs/compat-matrix.md`. This
turns the compat matrix from prose into an executable claim, and it is the only
way the Slack contradiction noted in F-4 gets resolved.

## T-5. Fault-injection on every stage boundary (catches F-2, and the orphan helper)

For each of {recognizer wedged, recognizer panics, writer thread dies, AX call
hangs, mic fails to open, config file becomes malformed mid-run}, assert:

1. the pipeline terminates or recovers within a bounded time,
2. the mic ends closed,
3. the user-visible state is `Error` or `Idle`, never a stalled `Listening` or
   `Transcribing`,
4. a message naming a next action reaches the overlay, not just stderr.

The wedged-recognizer case is the probe used in F-2 above and is about fifteen
lines. Invariant 2 is the one that matters most, and it is currently untested on
every single error path.

---

# Most serious finding

**F-1** is the answer to the stuck-microphone report, and I have reproduced it.
A sub-threshold tap of the bound chord — which is a bare modifier that users
press constantly for unrelated reasons — opens the microphone and emits nothing
that can ever close it. The documented 60-second safety net for exactly this
case (`docs/ux/02-core-interaction.md:24`, `schema.rs:269`) is `wired: false` and
has no implementation anywhere in the workspace.

**Confidence: high** that this is a real defect (reproduced by executing the
crate's own state machine and the real event mapping, output above).
**Confidence: medium-high** that it is *the* bug the user saw, on the grounds
that it fits the reported symptom exactly (indicator on with no dictation
happening), it is triggered by ordinary typing rather than by dictating, and it
persists indefinitely — which together explain why it did not reproduce when
someone sat down and tried to dictate.

F-2 is an independent second path to the same symptom and is the better
explanation for the orphaned helper that `instance.rs:190` documents as
never-reproduced. Both should be fixed; neither subsumes the other. The single
highest-value change is not either individual fix but the invariant in T-1:
**`mic.is_open()` must equal `listening`, asserted continuously.** That one
assertion closes F-1, F-2, F-6, and F-9 as a class, and would have failed on the
very first generated event sequence.
