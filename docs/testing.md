# Testing strategy

Every hard bug in this project so far has been environmental or a seam
problem, never a logic error inside one crate: the system-wide AX element
failing while trusted, TCC following the responsible process, cdhash
invalidation on rebuild, windows on `AXWindows` not `AXChildren`, Chromium
needing an opt-in, apps on another Space reporting zero windows, and a
transport selector consulting our own tty instead of the destination. The
test architecture is shaped by that history: the highest-value tests simulate
*environments* and assert on *destinations*, and every environmental
impossibility is a **skip with a stated reason**, never a failure.

This document covers the workspace-level tiers. Per-crate unit tests live in
each crate and are described by `docs/planning/03-definition-of-done.md`.

## The tiers and how to run them

| Tier | What it proves | Needs | Command |
|---|---|---|---|
| Unit | each crate's own logic | nothing | `cargo test -p <crate>` |
| Workspace integration | the seams between crates | nothing | `cargo test -p workspace-tests` |
| Everything simulated | both of the above + fmt + headless build | nothing | `./scripts/test-workspace.sh` |
| Real applications | text actually lands in real apps | macOS GUI session, permissions | `./scripts/test-real-apps.sh` |

`test-workspace.sh` is the pre-push command. It runs on any machine including
headless CI, because every environmental fact the integration tests consume
is scripted through `tests/tests/common/mod.rs::SimEnv` rather than read from
the machine.

## What lives in `tests/` (the `workspace-tests` crate)

An intentionally empty library crate whose `tests/` directory holds the
cross-crate suites. Unit tests belong in the crate that owns the logic; only
behavior that crosses a crate boundary belongs here.

- **`pipeline.rs`** - the full read -> parse -> apply -> write loop across
  `text-target` and `edit-intent`, against `StdioFilterTarget` over in-memory
  buffers. That target is used because its destination is fully observable:
  the tests decode the actual protocol bytes the transport emitted and assert
  on what the destination was told, not on our own return values. Includes
  the two refusal paths that protect users: an unmatched command must stop
  *before* the write, and a freeform utterance must stop at the
  language-model boundary.

- **`transport_matrix.rs`** - transport selection across named simulated
  environments: trusted desktop, GUI-daemon-launched-from-tmux (the tty
  bug), untrusted desktop, tmux, stale `$TMUX`, SSH, SSH+tmux, GNU screen,
  WezTerm, kitty, Wayland, X11-without-clipboard, and bare headless. Each row
  states the transport it must select, plus structural invariants: every
  selection is constructible, every reason string is actionable, and a GUI
  destination never receives a terminal transport (the
  inject-into-an-unwatched-shell bug, pinned forever).

- **`error_paths.rs`** - error identity across seams. `AxError::NotTrusted`
  must still be matchable as `NotTrusted` after crossing into
  `TargetError`, because diag's Environment/Permission/Configuration/Bug
  taxonomy (and therefore every remedy message) depends on it. Also proves
  recorded write failures keep their class through serialize/parse, and that
  recorded error details are scrubbed of home path and username.

- **`fuzz_edits.rs`** - property tests with a deterministic PRNG (seed
  printed on failure, no fuzz-framework dependency) over a hostile alphabet:
  characters whose lowercase changes byte length (İ, ß, ẞ, Σ), combining
  marks, ZWJ emoji, zero-width, RTL, embedded newlines. Properties: parse and
  apply never panic; replace never edits when the needle is absent; a
  matched replace confines its change to the needle's span (the over-edit
  rate = 0 gate from `03-definition-of-done.md`, checked geometrically via
  `diag::replay::edit_window`); delete never grows text, append never
  shrinks it. This suite found a real panic and a real silent over-edit in
  `replace_case_insensitive` on its first run, which is the strongest
  argument for its existence.

- **`replay_roundtrip.rs`** - the record/replay workflow end to end,
  including a reconstruction of the tty bug: a record with correct facts but
  the buggy conclusion diverges against the fixed selector, and the
  divergence names both transports. Also proves a record made from a session
  full of private content is safe to attach to a public issue.

## Session recording and replay (`diag::replay`)

`SessionRecord` captures a full pipeline run as *facts*: env-var presence
(whitelisted names only, never values), capability probe answers, transport
selected and why, focused-field shape, intent shape, transformation geometry,
write result, and per-stage timings. It serializes to a greppable
line-oriented text format (`outloud-replay v1`).

Redaction is by construction: the `record_*` methods take raw values and
store only what `diag::redact` leaves behind. There is no method that stores
a raw string verbatim, so a recording can never leak transcribed text, window
titles, or paths regardless of what callers do.

The transformation is stored as geometry (changed-window start, chars
removed, chars inserted, via `edit_window`'s common-prefix/suffix trim).
This is what lets a redacted record still prove or disprove an over-edit:
everything outside the window is untouched by construction, and a window
wider than the intent's operands is an over-edit visible without either
string being present.

## Turning a user bug report into a replayable test

1. Have the user attach the serialized `SessionRecord` (once `spike-cli`
   grows a `--record` flag, that artifact; today, the doctor bundle plus the
   record produced by whatever harness reproduced it). The artifact is safe
   to post publicly; `replay_roundtrip.rs::the_artifact_is_safe_to_attach_to_a_public_issue`
   is the gate on that claim.
2. `SessionRecord::parse` it, then `verify_consistency()` - a truncated or
   hand-edited record is rejected here instead of producing a wrong
   diagnosis.
3. Rebuild an environment from the record with
   `tests/tests/common/mod.rs::env_from_record` and run `select()` against
   it. `compare_selection` reports a divergence when current code disagrees
   with what the user's build did. A divergence localizes the bug to one
   decision; no divergence means the bug is below the facts (a probe lying
   about the world), which is itself a diagnosis: fix the probe, then the
   check belongs in `diag::checks`.
4. Encode the record's facts as a new `Row` in `transport_matrix.rs` (or a
   new case in the relevant suite) with the *correct* expectation. That row
   is the regression test; the fix makes it pass.

Step 4 is the point of the whole workflow: every environmental bug ends its
life as a named row in a matrix that runs on every machine forever.

## The real-application harness (`scripts/test-real-apps.sh`)

Drives TextEdit and Safari via AppleScript and asserts on the text that ends
up in the document, read back through a channel the pipeline does not
control. Return codes are explicitly not trusted: the tty bug reported
success while typing into a shell nobody was watching, and only independent
readback catches that class.

Degradation policy, in order of preflight:

| Situation | Outcome |
|---|---|
| Not macOS | skip everything |
| Automation permission ungranted | skip everything, remedy printed |
| No spike-cli binary / no AX trust | AppleScript-level checks still run, pipeline tests skip |
| App not installed | that app's tests skip |
| App did not become frontmost (another Space) | that test skips, naming the Space ambiguity |
| Safari JS-from-Apple-Events not enabled | Safari test skips, naming the Develop-menu opt-in |
| Pipeline claims success, document disagrees | **FAIL** - the only failure |
| Pipeline fails but document changed anyway | **FAIL** - severed seam |

The frontmost check exists because `activate` on an app whose windows live on
another Space can leave a different app focused, and the edit would then hit
the wrong destination, exactly the ambiguity that bit M0.

### Adding an application to the matrix

1. Find its scriptable readback channel: a `text of front document` (AppKit
   document apps), `do JavaScript` (Safari), or UI-element text via System
   Events (last resort, needs Accessibility for the *terminal*). If it has
   none, it belongs in the manual `spike-cli matrix` checklist instead.
2. Write `test_<app>_pipeline()` following the TextEdit one: create known
   content, verify the app is frontmost (skip if not, naming the Space
   trap), run `"$BIN" edit ...`, read the document back, compare text.
3. Every environmental precondition gets its own `skip` with the remedy in
   the message. The skip reasons are the harness's documentation of what a
   fully-green run requires.
4. Add the app to `scripts/run-matrix.sh`'s `TARGETS` too if it represents a
   new text-system family worth tracking in `docs/compat-matrix.md`.

## Rules

- **Skip, never fail, for environmental reasons.** A red test must mean the
  code is wrong. Anything else trains people to ignore red.
- **Assert on destinations, not return codes**, whenever a destination can
  be observed independently.
- **Simulated environments are stated, not sampled.** Tests build a `SimEnv`
  describing the world exactly; they never mutate the process's real
  environment (racy, global) or depend on what CI happens to have installed.
- **Every bug fix lands with the row or case that would have caught it**
  (per-task DoD rule 3). For environmental bugs that means a matrix `Row`;
  for parser/transform bugs a `known_hostile_inputs` pin plus, when the bug
  was found by fuzzing, leaving the fuzz property strengthened rather than
  weakened.
