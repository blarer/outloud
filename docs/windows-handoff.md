# Windows handoff: state of the build as of 2026-08-04

Written on the Mac for whoever picks up work on the Windows machine. The
Windows session lost its context; this is the shortest path back to
productive, plus the things that are easy to get wrong.

Start with the "Windows status, precisely" table in the top-level
`README.md`: it lists every Windows backend and whether it has ever run on
hardware. `docs/build-and-release.md` covers producing artifacts, and
`docs/hotkeys.md` covers the UIPI trap. This file is the *current state and
open questions* on top of those.

## Where things stand

Windows dictation works. It has been used to dictate into Discord on real
hardware: whisper.cpp with CUDA on an RTX 5090, ~350ms end to end, routed to
clipboard-paste because Discord discards accessibility writes.

Landed since then, none of it exercised on a Windows machine yet:

| Change | Risk on Windows |
|---|---|
| Undo (`"scratch that"`) wired to the ring on both platforms | Never run on Windows |
| `--say` is repeatable: several utterances per process | Never run on Windows |
| Per-app transport rules (`accepts`) applied to the tier path | Was macOS-only before |
| Single-instance guard no longer calls a no-op `mem::forget` | Behaviour change |

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
edit command it now also reports the route it *would* have taken:

```
transcript [route: undo|rewrite|no-match|dictate|unsupported]
```

Use that before doing anything that types into a window.

## What is untested and most likely to be wrong

**1. Undo, all of it.** The decision half (`resolve_undo`) is unit-tested and
runs on macOS CI. The Windows-specific half is compile-checked only:

- `UiaTarget::read()` through TextPattern's `DocumentRange::GetText`, falling
  back to ValuePattern's `CurrentValue`. If a field implements neither, undo
  reports "could not read the field to undo into", which is correct but
  worth confirming reads as intended.
- The restore is written by `write_literal_via_tiers`, which deliberately
  skips intent parsing. See the note below; this was a real bug.

Try, in a Notepad window:

```powershell
outloud --once --say "change quick to slow" --say "scratch that"
```

after typing `the quick brown fox` and selecting it. Expect the original
text back. Both utterances share one process, which is required: the undo
ring is process-lifetime, so two separate `--once` runs cannot work.

**2. Whether ValuePattern's `SetValue` preserves the app's own undo stack.**
It probably does not. If Ctrl+Z in Notepad after a dictation does something
surprising, that is where to look, and it is a known design gap rather than
a regression.

**3. `accepts()` on the tier path.** Windows previously applied no per-app
rules at all. Now `Acceptance::ClipboardOnly` skips both UIA and SendInput.
If an app that used to work now takes the clipboard route unnecessarily,
check `crates/text-target/src/targets/keys.rs` for its process name.

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
- 17/17 CI jobs green, including `windows-2025` build and overlay smoke
- `scripts/ci-check.sh`, `scripts/ci-check-cfg.sh`, and
  `scripts/ci-check-windows.sh` all pass on the Mac

Nothing is unpushed. `main` is the truth.
