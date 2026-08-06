# Team and Onboarding

## Roles and ownership

Five roles, four people to start (WIN/LNX begins at 0.5 FTE contract and grows
to 1.0 for M2). Rationale and comparables in
`../../../aqua-voice-research/04-team-and-plan.md`.

| Role | Code | Owns | First 4 weeks |
|---|---|---|---|
| ML/ASR engineer | ML | Recognizer pipeline (Parakeet, Moonshine, whisper.cpp), VAD, endpointing, LLM fallback, vocabulary biasing, WER + edit-accuracy eval harnesses, latency budget for the ASR stages | R-01..R-07, E-01 |
| Rust systems lead (tech lead) | SYS | Engine architecture, `edit-intent` and command grammar, daemon + protocol, terminal integration, model manager, CI/release, undo stack, performance | E-02, E-03, R-08, R-09, P-05 |
| macOS platform engineer | MAC | `ax-edit`, `text-target` macOS backend, injection strategies, TCC/onboarding, hotkey event tap, overlay, notarization | P-01..P-04, P-06, U-01 |
| Windows/Linux platform engineer | WIN/LNX | UIA TextPattern, SendInput, keyboard hooks, Wayland IME, X11, AT-SPI, per-OS packaging and signing | Reads M2 sections; prototypes W-01 spike |
| Product/design player-coach | PD | Interaction model, `docs/ux` upkeep, onboarding UX, docs site, community/DevRel, analytics stance, launch | P-06, U-08, I-06, I-07 |

Cross-cutting rules:

- Every OS-specific crate keeps a platform-free API surface (the `ax-edit`
  pattern: `Unsupported` off-platform), so everyone can build and test
  everywhere.
- Bus factor: each owner writes docs as they go and pairs with one other person
  for a day per milestone on their scariest subsystem. Quarterly bus-factor
  review (research risk #7).
- The eval harnesses (WER, edit accuracy, latency) are the shared contract
  between ML and platform work. Nobody merges a change that moves those
  numbers without the gate seeing it.

## Week-1 onboarding path for a new engineer

Goal: by end of week 1 you have run the full pipeline on your machine, made a
change, and landed a PR. Budget one day for environment, not four. The four
traps below each cost the M0 team real hours; read them before touching a
terminal.

### The four environmental traps (from `docs/M0-results.md`)

1. **The system-wide AX element does not work.** Every tutorial shows
   `AXUIElementCreateSystemWide()` → `AXFocusedUIElement`. On current macOS it
   returns `kAXErrorCannotComplete` (-25204) even for a fully trusted process.
   Resolve the focused *application* first, then ask it for its focused
   element. `crates/ax-edit/src/macos.rs` does this correctly; do not
   "simplify" it back to the documented shortcut.
2. **TCC grants follow the responsible process, not the binary.** Run the
   binary from your shell and macOS checks your *terminal's* Accessibility
   permission, ignoring the app's own grant, while System Settings shows the
   toggle on. Launch through LaunchServices instead: `./scripts/run.sh` or
   `open -a dist/OutLoudSpike.app --args probe`. Output is mirrored to
   `OUTLOUD_SPIKE_LOG` because LaunchServices detaches from the terminal.
3. **Ad-hoc signatures silently revoke the grant on every rebuild.** TCC pins
   approval to the binary's `cdhash`. Rebuild → new hash → every call fails
   while the toggle still reads "on". After a rebuild:
   `tccutil reset Accessibility dev.hexavoice.spike`, then re-grant with
   `./scripts/grant-accessibility.sh`. This disappears once you use the team
   Developer ID profile (ask SYS for it).
4. **Applications hang windows off `AXWindows`, not `AXChildren`.** An app
   element's children are its menu bar. Walking children finds thousands of
   menu items and zero text fields. Use the existing traversal helpers.

Also know the error-code translations (`docs/macos-permissions.md`): -25204
almost always means "not trusted", -25211 means AX disabled for this process,
-25212/-25205 are normal absent-attribute results, not failures. And every AX
call is bounded by a 500ms messaging timeout because it is synchronous IPC
into another process; never remove that.

### Day-by-day

**Day 1 — environment and first run.**

```bash
git clone <repo> && cd outloud-spike
cargo test                       # edit-intent tests pass with no permissions
./scripts/bundle-macos.sh        # .app bundle: gives TCC a stable identity
./scripts/grant-accessibility.sh # opens the pane, waits for the toggle
BIN=dist/OutLoudSpike.app/Contents/MacOS/OutLoudSpike
$BIN dry-run "change quick to slow"   # no permissions needed
$BIN probe                            # via scripts/run.sh if it fails: trap 2
$BIN edit --after 5 "change hello to goodbye"  # click into TextEdit first
$BIN matrix                           # the guided application checklist
```

If `probe` fails with -25204, you have hit trap 2 or trap 3. Run `doctor`
(once P-07 lands) before asking for help.

**Day 2 — read the code.** Reading order below. Trace one `edit` invocation
end-to-end: main.rs → `edit_intent::parse` → `ax_edit` snapshot → apply →
write strategy.

**Day 3 — make a small change.** Pick a labeled good-first-issue, typically an
`edit-intent` grammar case (pure Rust, no permissions, fully unit-testable).
Add the eval-corpus entries with it.

**Day 4 — run the matrix on your machine.** Fill in a compat report
(`.github/ISSUE_TEMPLATE/app-compat.yml`) for one app not yet in the matrix.
This teaches you the strategy tiers (`set-selected-text` > `set-value` >
paste) and grows the coverage table at the same time.

**Day 5 — land the PR.** Through the full DoD (`03-definition-of-done.md`):
tests, eval gate, comment style, commit convention.

### Read these files in this order

1. `README.md` — what the project is, layout, design decisions.
2. `docs/M0-results.md` — what is proven, with numbers, and the four traps.
3. `docs/macos-permissions.md` — the TCC model and error-code table.
4. `docs/planning/00-roadmap.md` — where we are going and the exit criteria.
5. `crates/edit-intent/src/lib.rs` — the intent model; pure, testable core.
6. `crates/ax-edit/src/lib.rs` — the safe API surface and error taxonomy.
7. `crates/ax-edit/src/macos.rs` — the FFI reality; read alongside trap 1
   and trap 4.
8. `crates/spike-cli/src/main.rs` — how it all composes; the timing
   instrumentation you must preserve.
9. `docs/ux/**` — the interaction design you are building toward.
10. `docs/planning/01-backlog.md` — find your tasks.
11. Research pack `../aqua-voice-research/00-SYNTHESIS.md` first, then the
    file matching your role: `02-local-asr-tech.md` (ML),
    `03-oss-prior-art.md` (SYS/platform), `01-product-recon.md` (PD),
    `04-team-and-plan.md` (everyone, skim).

### Role-specific week-1 additions

- **ML**: reproduce a Parakeet ONNX transcription locally (sherpa-onnx or
  parakeet-mlx) and run the WER harness (R-07) once it exists. Target numbers
  to hold in your head: Parakeet 6.05% Open-ASR avg, Moonshine Medium
  Streaming 107ms incremental on M-series, budget p50 ≤ 500ms end-to-end.
- **MAC**: run `$BIN inspect <app>` against three Electron apps; read the
  `AXManualAccessibility` code path.
- **WIN/LNX**: no macOS needed; start with `edit-intent` (cross-platform) and
  the UIA TextPattern docs; prototype read-back in Notepad.
- **PD**: run the matrix as a naive user and file every friction point as an
  issue; you own the onboarding flow that removes them.

## Working agreements

- Weekly numbers review: p50/p95 latency, WER, edit-accuracy, matrix pass
  count. Trends, not vibes. The instrumentation exists precisely so this
  meeting is 15 minutes.
- Milestone exit criteria are binary. If a number is not met, the milestone is
  not done; scope moves, dates move, criteria do not soften.
- Anything that costs an engineer more than 2 hours of confusion gets written
  into the relevant doc the same day (that rule produced
  `docs/macos-permissions.md`, which pays for itself with every hire).
