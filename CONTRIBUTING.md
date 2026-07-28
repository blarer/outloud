# Contributing

Thanks for helping build local, private, edit-by-voice dictation. This file is
the practical how-to. The deeper context lives in `README.md`,
`docs/M0-results.md`, and `docs/planning/`.

## Build

```bash
cargo build            # works on any OS; platform crates degrade gracefully
cargo test             # edit-intent tests need no permissions at all
```

On macOS, to actually exercise the Accessibility path you need a bundled,
signed app. A bare binary will not get a stable permission identity:

```bash
./scripts/bundle-macos.sh          # produces dist/AquaSpike.app
./scripts/grant-accessibility.sh   # opens the right pane, waits for the toggle
```

## Run

```bash
BIN=dist/AquaSpike.app/Contents/MacOS/AquaSpike

$BIN dry-run "change quick to slow"   # intent parser only, no permissions
$BIN probe                            # read the focused field
$BIN watch 500                        # poll while you tab between apps
$BIN edit --after 5 "change hello to goodbye"
$BIN matrix                           # guided application checklist
```

`--after N` waits N seconds so you can click into the target application. A
dictation tool always acts on the app the user was already in, never on
itself; from a terminal you must reproduce that manually.

If a command that needs permissions fails, run it through `./scripts/run.sh`,
which launches via LaunchServices and tails the log file named by
`AQUA_SPIKE_LOG`. See the gotchas below for why.

## Test

- `cargo test` before every push. Unit tests are cross-platform by design;
  `edit-intent` is pure Rust with no OS dependency.
- If you change parsing or transformation logic, add cases to the
  edit-accuracy eval corpus in the same PR, not just unit tests. The corpus
  is the regression contract for the product's core promise.
- If you touch anything on the AX/injection path, run `$BIN matrix` and note
  the results in your PR. A row flipping from pass to fail blocks merge.
- Latency-sensitive paths must preserve the timing instrumentation. The
  numeric gates that CI enforces are listed in
  `docs/planning/03-definition-of-done.md`.
- `./scripts/verify-head.sh` before declaring a change done, and always after
  a large mechanical change such as a rename. It clones committed `HEAD` to a
  scratch directory and builds there, which is the only check that sees what a
  new contributor sees. A passing `cargo test` in your own tree does not prove
  `HEAD` compiles: if half of a two-file change is still uncommitted, your
  tree has both halves and `HEAD` has one. That has happened here, and every
  local checkout was green while `HEAD` was broken.

## Environmental gotchas (read before debugging permissions)

These four cost the M0 team real hours each. Full detail in
`docs/M0-results.md` and `docs/macos-permissions.md`.

1. **Run via LaunchServices, not from your shell.** macOS checks the
   Accessibility permission of the *responsible process*. From a terminal,
   that is the terminal, and your binary's own grant is ignored while the
   System Settings toggle sits there reading "on". Use `./scripts/run.sh` or
   `open -a dist/AquaSpike.app --args ...`.
2. **Rebuilds silently revoke the permission.** TCC pins the grant to the
   binary's `cdhash` under ad-hoc signing. After a rebuild, reset and
   re-grant: `tccutil reset Accessibility dev.aquaoss.spike`, then
   `./scripts/grant-accessibility.sh`. With the team Developer ID profile
   this problem disappears.
3. **Do not use `AXUIElementCreateSystemWide()` for focus.** It returns
   `kAXErrorCannotComplete` (-25204) on current macOS even when fully
   trusted. Resolve the focused application first, then ask it for its
   focused element. The code already does this; do not simplify it back to
   what the documentation shows.
4. **Windows hang off `AXWindows`, not `AXChildren`.** An application
   element's children are its menu bar. Walking children finds thousands of
   menu items and zero text fields.

Error-code cheat sheet: `-25204` almost always means "not trusted", `-25211`
means AX is disabled for this process, `-25212` and `-25205` are normal
absent-attribute results and are mapped to `Ok(None)`, not errors.

## Comment style: comments explain WHY, never narrate

A comment that restates what the code does is noise and rots. A comment that
explains why the code is shaped the way it is pays rent forever. This codebase
follows that rule strictly, and reviews enforce it.

Bad:

```rust
// Split the string on the joiner.
let idx = rest.rfind(joiner);
```

Good (from `edit-intent`):

```rust
// Split on the last occurrence so a joiner appearing inside the
// search text does not truncate it: "change to do to to-do".
let idx = rest.rfind(joiner);
```

Every public item gets a doc comment. Module-level docs explain the module's
reason to exist and the design decisions a reader needs, in the style of the
existing crates.

## Commit messages

Conventional Commits, matching the existing history:

```
feat: scan a named application for text fields
fix: reach the focused element through the focused application
docs: record the M0 result and what it cost to get there
```

- Types: `feat`, `fix`, `docs`, `test`, `refactor`, `perf`, `ci`, `chore`.
- Subject in the imperative, lower case, no trailing period, ≤ 72 chars.
- The body explains why, and records any measured numbers ("write-back p95
  went from 21ms to 13ms"). If the change closes a backlog task, reference
  its id (for example `P-01`).
- One logical change per commit. Commit as you go rather than in one heap.

## Pull requests

- Keep PRs reviewable: one backlog task or one coherent change.
- State how you validated it. "Ran matrix, all rows pass" beats "should
  work".
- AX, injection, and protocol changes need review from the subsystem owner
  (see `docs/planning/02-team-and-onboarding.md`).
- The full per-task Definition of Done is in
  `docs/planning/03-definition-of-done.md`.

## Reporting problems

Use the issue templates. Bug reports require `doctor` output; without it,
permission-related reports are guesswork. The application-compatibility
template grows the support matrix, and filling one in for an unlisted app is
a genuinely useful first contribution.
