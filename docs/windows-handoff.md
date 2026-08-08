# Windows handoff: state of the build as of 2026-08-04

Written on the Mac for whoever picks up work on the Windows machine, then
updated ON that machine after the checks it recommended were actually run.
This is the shortest path back to productive, plus the things that are easy
to get wrong. Where the original guidance turned out to be wrong on real
hardware, it says so rather than being quietly corrected: the mistakes are
the useful part.

Start with the "Windows status, precisely" table in the top-level
`README.md`: it lists every Windows backend and whether it has ever run on
hardware. `docs/build-and-release.md` covers producing artifacts, and
`docs/hotkeys.md` covers the UIPI trap. This file is the *current state and
open questions* on top of those.

## Where things stand

Windows dictation works. It has been used to dictate into Discord on real
hardware: whisper.cpp with CUDA on an RTX 5090, ~350ms end to end, routed to
clipboard-paste because Discord discards accessibility writes.

**The list below has now been run on the Windows machine** (2026-08-04). Every
item found something, and the two most serious were invisible from macOS:

| Change | Result on hardware |
|---|---|
| Undo (`"scratch that"`) wired to the ring | **Was broken.** Typed the words into the document. Fixed and verified. |
| `--say` is repeatable | Works. Two utterances, one process, whisper via CUDA. |
| Per-app transport rules (`accepts`) on the tier path | Correct for all three tiers, checked with `--route`. |
| Single-instance guard | Refuses correctly, but the mutex was mis-scoped and leaked. Fixed. |
| The hotkey path itself (not on the original list) | Works: a real chord drives listening -> transcribing -> idle. |

What the run actually turned up, worst first:

1. **Double-clicking `outloud.exe` never worked at all.** The default
   recognizer was a hardcoded `"apple"`, which is Apple's `SpeechTranscriber`
   behind a Swift helper binary that cannot exist off macOS. The daemon died
   in under a second telling the user to run `swiftc`, so nothing appeared
   and the app looked like it simply failed to launch. Reported by the user,
   not by any check here, because every command in this document passes
   `--asr` explicitly and so could never see it. The model path was also
   environment-variable-only, and Explorer sets no variables, so fixing the
   default alone still left it broken. Both fixed; `scripts\verify-doubleclick-windows.ps1`
   reproduces the Explorer launch that a terminal cannot.
2. **`"scratch that"` was typed into the user's document instead of undoing.**
   Undo was gated on `Mode::Edit`, meaning on there being a selection. Our own
   write *replaces* the selection, so by the time anyone asks to undo there is
   only a caret: the gate made undo unreachable in exactly the sequence it
   exists for. Measured against Notepad: an edit landed, then the field read
   `the slow brown foxscratch that`.
3. **`OUTLOUD_NO_INJECT` silently did nothing under `cmd.exe`.** It was
   compared exactly against `"1"`, and `set VAR=1 && prog` assigns `"1 "`,
   trailing space included. The command you run specifically to avoid typing
   into your own windows was the one that typed into them. It pasted into a
   live terminal here before it was found.
4. **`--say` had no synthesizer on Windows**, so every check the previous
   version of this document recommended failed with `program not found`
   before reaching anything. Now goes through `System.Speech`.
5. **A dry run computed the edit route and threw it away**, so `[route: ...]`
   never reached the user. Same shape as the unreachable undo ring: a correct
   value nothing displays.
6. **`--permissions` reported a missing macOS grant on every Windows
   machine**, telling users to fix a permission that does not exist here.
7. **Nine tests had never passed on Windows**, all hardcoding macOS tier names
   or Linux session types. None found a real defect; together they made a real
   Windows failure impossible to spot.

The first one is the lesson: **every check in this file passed while the
product was unlaunchable.** They all invoked the binary the way a developer
does, with flags. Nobody ran it the way a user does, with none.

The workspace now passes on Windows: 0 failures, `clippy -D warnings` clean.

### Still not verified here

- **How the overlay looks.** Unchanged from `docs/plans/windows-overlay.md`:
  CI proves the Direct2D path executes, not that the pixels are right.
- **A real dictation into Discord since these changes.** The ROUTING is
  verified (`outloud --route discord` answers clipboard-only on this
  machine, and the same for Slack and Notepad), and dictation into Discord
  worked before them, but nobody has spoken into Discord since. Deliberately
  not tested by script: a run that writes goes into a live chat box, which
  happened once here by accident and is why the undo harness now aborts when
  focus moves.
- **Whether `ValuePattern::SetValue` preserves the app's own undo stack.**
  Still expected not to; still a known design gap.

### Verifying it yourself

```powershell
powershell -File scripts\verify-doubleclick-windows.ps1   # launches like Explorer does
powershell -File scripts\verify-undo-windows.ps1 -DryRun  # routing only, writes nothing
powershell -File scripts\verify-undo-windows.ps1          # real writes into Notepad
scripts\verify-undo-unattended.bat                        # same, no confirmation prompt
powershell -File scripts\verify-single-instance.ps1       # second daemon must refuse
powershell -File scripts\verify-hotkey-windows.ps1        # the real hotkey path
outloud --route discord                                   # a named app's transport
outloud --route 5                                         # or whatever you focus in 5s
```

`verify-hotkey-windows.ps1` is the only one that exercises the product's
actual entry point. Everything built on `--say` enters the pipeline BELOW the
hotkey and the microphone, so a hook that never fires would pass every other
check here. It sends the real `VK_RMENU` chord and asserts the daemon reached
`state listening`, which means the hook received the key-down, the matcher
recognised the chord, and capture opened. Confirmed on this machine:
`listening -> transcribing -> idle` from a synthetic keypress.

`--route NAME` answers without focusing the app, which matters because
Windows refuses programmatic foreground changes and because the alternative
way to find out is dictating into someone's chat window.

The live undo run asks for Return before it starts, on purpose: it types into
Notepad and whoever is at the keyboard deserves the warning. A background or
CI caller has no console to answer that and will appear to hang forever, so
use `verify-undo-unattended.bat` (or set `OUTLOUD_LIVE_YES=1`) there. That
exact hang cost time during this session, twice.

`verify-undo-windows.ps1` asserts on the FIELD CONTENTS, read back through the
clipboard, not on log lines: a log proves a value was computed, which is
precisely how an unreachable undo ring survived for weeks. It reports
INCONCLUSIVE rather than FAIL when focus is stolen mid-run, which happens on a
machine someone is using.

## Build it

```powershell
cargo build --release -p outloud --features display
```

The recognizer is whisper.cpp via whisper-rs. Building it needs LLVM on
`PATH` (for `libclang`, which bindgen requires) and, for GPU, the CUDA
toolkit with `CUDA_PATH` set. A first build compiles whisper.cpp itself and
is slow; that is expected once.

Measured on the 5090: **8970ms on CPU, 346-364ms with CUDA.** If a run takes
seconds rather than a third of one, CUDA is not actually being used, and
that is the first thing to check rather than a performance bug to chase.

## Verify before assuming a bug is yours

```powershell
outloud --permissions          # what the daemon can actually do
outloud --once --say "hello"   # one utterance, into whatever is focused
```

`OUTLOUD_NO_INJECT=1` runs the whole pipeline and writes nothing. For an
edit command it also reports the route it *would* have taken:

```
transcript [route: undo|rewrite|no-match|dictate|unsupported]
```

Use that before doing anything that types into a window.

**Set it on its own line.** `set OUTLOUD_NO_INJECT=1 && outloud ...` in
`cmd.exe` assigns `"1 "`, with the space before the `&&`, and until it was
fixed here that read as unset and the "safe" command typed into a live
terminal. The parse is forgiving now, but `scripts\dry-run.bat` and
`scripts\dry-whisper.bat` set it correctly and are less to remember.

`--say` uses `System.Speech` on Windows (`scripts\sapi-say.ps1`), so it needs
no extra install. `--asr mock` emits one word per SECOND of voiced audio, so
a short phrase commits nothing and reports "heard nothing?", which looks
exactly like a broken pipeline: `scripts\mock-words.py FILE` tells the two
apart, and `--asr whisper` avoids the question.

## What is untested and most likely to be wrong

**1. Undo: now verified, and it was broken.** `UiaTarget::read()` reads the
focused element through TextPattern, falling back to ValuePattern, and a
field implementing neither correctly reports "could not read the field to
undo into". All of that works. What did not was reaching it at all: see the
list above.

`scripts\verify-undo-windows.ps1` runs the whole thing unattended. Doing it
by hand needs two things that are easy to miss:

- **The target window must be focused before the first key-down**, because
  that is when dictate-vs-edit is decided. A run launched from a console
  samples the console and silently degrades to dictation, which is why this
  looked fine for so long. `OUTLOUD_REPLAY_DELAY_MS=6000` holds the first
  utterance so you can click the window.
- **Both utterances must share one process.** The undo ring is
  process-lifetime, so two separate `--once` runs can never work.

```powershell
$env:OUTLOUD_REPLAY_DELAY_MS=6000
outloud --once --asr whisper --say "change quick to slow" --say "scratch that"
```

**2. Whether ValuePattern's `SetValue` preserves the app's own undo stack.**
It probably does not. If Ctrl+Z in Notepad after a dictation does something
surprising, that is where to look, and it is a known design gap rather than
a regression.

**3. `accepts()` on the tier path.** Windows previously applied no per-app
rules at all. Now `Acceptance::ClipboardOnly` skips both UIA and SendInput.
If an app that used to work now takes the clipboard route unnecessarily,
check `crates/text-target/src/targets/keys.rs` for its process name, and ask
`outloud --route` what the rules actually decided rather than inferring it
from a dictation that went somewhere unexpected.

## Things that cost hours before, so they are worth stating plainly

**Never call `PeekMessageW` then `thread::sleep` on a hook thread.** A
low-level keyboard hook only dispatches while its thread is waiting on its
message queue. Sleeping there stalls every keystroke system-wide, and
Windows silently unhooks you after 300ms. This froze the user's physical
keyboard. The fix is `MsgWaitForMultipleObjects`; see `pump_with_watchdog`.

**Do not "probe" a hook by unhooking and reinstalling it.** That drops
key-ups, so the daemon believes a key is still held.

**UIPI silences dictation over elevated windows.** A non-elevated process
cannot read keys from, or inject into, an elevated window. Dictation appears
to stop working and then recovers when focus moves. This is Windows policy,
not a bug, and it is the single most likely explanation for "it randomly
stopped working". `docs/hotkeys.md` has the details.

**`HOME` is not set on Windows.** Config paths that resolve through it
silently point nowhere. Use the `config::paths` helpers, which use
`ProgramData` and `APPDATA`.

**A green test suite proves less than it looks like it does here.** Several
bugs this week were values computed correctly that never reached the user,
each behind passing tests. When something is wrong, check what the user can
*see*, not what the code computes.

**Anything that reads the focused element is racing the rest of the desktop.**
Verification on this machine kept being derailed by whatever held focus: a
fullscreen game reported its own window (with no TextPattern and no
ValuePattern) for every probe, and one undo run read back a browser's URL
bar and reported a product failure that was nothing of the kind. Windows also
refuses `SetForegroundWindow` from a process that does not already own the
foreground, and `AppActivate` fails silently against some windows, so a
harness cannot reliably focus its own target. Anything asserting on a focused
field must check it read the window it MEANT to, and report inconclusive
rather than failure when it did not.

**A `--once` run skips the single-instance guard deliberately.** It neither
binds the hotkey nor stays resident, and benchmarks run several at a time. A
pair of `--once` runs therefore "passes" a guard check while never touching
the mutex: test it with real daemons.

## Lint Windows from the Mac now

This is new and worth knowing about, because it shortens the loop a lot:

```bash
scripts/ci-check-windows.sh      # on the Mac, needs cargo-xwin + brew llvm
```

It lints the real `x86_64-pc-windows-msvc` target with `-D warnings`,
including `--features display`. Its first run found four defects that macOS
could not see, including a `std::mem::forget` on a `Copy` type in the
single-instance guard that did nothing at all.

There is a Linux equivalent, `scripts/ci-check-linux.sh`, added for the same
reason after the Linux job broke three times in one day.

So: compile errors and lints for Windows can be caught on the Mac. What
still needs the Windows machine is *behaviour* -- whether UIA actually reads
that field, whether the paste lands, whether the hotkey fires.

It does NOT run the Windows test suite, which turned out to matter: nine
tests had never passed here, and the lint script cannot see that because it
only checks. Running `cargo test --workspace --features display` on the
Windows box is a separate and worthwhile step.

If you change shared code on the Windows box, running these two on the Mac
(or asking for them to be run) is much faster than a CI round trip, and they
catch the entire class of "compiles on my platform, not on yours".

## One bug worth understanding before touching the undo path

The restore originally wrote through the same function that handles a spoken
utterance, which re-parsed the restored text as a command. Restored text is
whatever the user dictated earlier, so:

- `"change the plan to something else"` parsed as a Replace and rewrote the
  selection
- `"delete the second paragraph"` parsed as a delete and removed text
- `"scratch that idea, it was wrong"` parsed as `Delete { text: "that idea,
  it was wrong" }` and would have deleted from the field

I guessed wrong twice about which of these was worst before checking. That
is why `write_literal_via_tiers` exists and why a restore must never go
through the parser: the safe set of phrases is not enumerable.

## Current test and CI state

- 771 tests pass on macOS
- The workspace passes on Windows too, as of 2026-08-04: 0 failures,
  `clippy -D warnings` clean, `cargo fmt --check` clean. Before this session
  nine tests failed here, every one of them a macOS-shaped assertion rather
  than a real defect.
- 17/17 CI jobs green, including `windows-2025` build and overlay smoke
- `scripts/ci-check.sh`, `scripts/ci-check-cfg.sh`, and
  `scripts/ci-check-windows.sh` all pass on the Mac

The Windows fixes above are committed on the Windows machine and may not be
pushed yet; check `git log origin/main..HEAD` there before assuming `main`
has them. The macOS CI has never run the changed undo path against a real
focused field, because no CI can.
