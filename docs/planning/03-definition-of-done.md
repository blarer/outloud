# Definition of Done

## Per-task DoD

A backlog task is done when all of the following hold:

1. Its acceptance criteria in `01-backlog.md` are demonstrably met, with the
   evidence (measurement, matrix row, CI run) linked from the closing PR.
2. Code builds and `cargo test` passes on all three OSes in CI (platform
   crates return `Unsupported` off-platform rather than failing to compile).
3. New behavior has tests at the appropriate tier (see test strategy below).
   A parser or transform change adds eval-corpus entries, not only unit tests.
4. No regression in any CI gate: WER, edit-accuracy, latency, soak.
5. Comments explain WHY, never narrate WHAT (see `CONTRIBUTING.md`). Public
   items have doc comments.
6. User-facing errors name the next action, never just a code
   (`docs/macos-permissions.md` rule 3).
7. Anything that confused the author for > 2 hours is written into the
   relevant doc in the same PR.
8. Reviewed by one other person; AX/injection/protocol changes reviewed by the
   subsystem owner.

## Per-milestone DoD

A milestone is done when:

1. Every exit criterion in `00-roadmap.md` is met, each backed by a recorded
   measurement (not an estimate). Criteria are binary; they do not get
   softened to hit a date.
2. All tasks tagged to the milestone are closed or explicitly re-milestoned
   with a written reason.
3. The application compatibility matrix for the milestone is fully filled in
   and published.
4. A tagged, signed, installable build exists and has been installed from
   scratch on a machine that has never seen the project.
5. The eval baselines (WER, edit-accuracy, latency) are re-pinned to the
   milestone's measured numbers, so the next milestone's regression gates
   compare against them.
6. A retro is written: what the numbers were, what was cut, what surprised us.

## Test strategy

### Tier 1 — unit tests (every PR, every OS, < 2 min)

- `edit-intent` and all pure logic: grammar cases including the known traps
  (joiner word inside the search text, case-insensitive matching, whitespace
  cleanup after deletion, non-ASCII where lowercasing changes byte length).
- Undo stack, protocol serialization, dictionary replacement, profile
  matching.
- Target: every bug fix lands with the test that would have caught it.

### Tier 2 — integration tests (every PR on the affected OS, < 10 min)

- AX/UIA/AT-SPI read-write round-trips against scriptable host apps
  (TextEdit via AppleScript, Notepad via UIA on Windows).
- Daemon protocol conformance suite (D-05): every message round-trips,
  version-mismatch handshake errors cleanly.
- Recognizer smoke test: 30s known audio through each enabled backend,
  transcript similarity ≥ 95% to reference.

### Tier 3 — real-application matrix (weekly, and before any release)

Run per OS on a real desktop session (self-hosted runners), scripted where
possible, manual checklist (`matrix` command) where not:

| OS | Applications |
|---|---|
| macOS | TextEdit, Notes, Mail, Safari (chrome + web content), Chrome, VS Code, Slack, Discord, Terminal.app, iTerm2 |
| Windows | Notepad, Word, Chrome, Edge, VS Code, Slack, Discord, Windows Terminal, PowerShell, Explorer rename field |
| Linux | gedit/TextEditor, LibreOffice Writer, Firefox, Chrome, VS Code, Slack, GNOME Terminal, Konsole, Alacritty, kitty |

Recorded per app: read works, write strategy used, in-place edit works,
selection-scoped edit works, undo works, secure-field refusal works. Pass bar
per milestone is in `00-roadmap.md` (8 of 10 read+write at M1 macOS, M2 per
OS). Any row that regresses from pass to fail is release-blocking.

### Tier 4 — accuracy and latency regression gates (nightly, pinned hardware)

Pinned hardware: one M1 Pro (macOS), one RTX 3060 + i7-class (Windows), one
x86 CPU-only box (Linux). Numbers below are the M1 gates; each milestone
re-pins baselines per milestone-DoD rule 5.

| Gate | Threshold | Fails the build when |
|---|---|---|
| End-to-end finalization p50 | ≤ 500ms | > 500ms, or > +10% vs pinned baseline |
| End-to-end finalization p95 | ≤ 800ms | > 800ms |
| First partial p50 | ≤ 250ms | > 250ms, or > +15% vs baseline |
| AX read p95 | ≤ 50ms | > 50ms (M0 measured 25-33ms; 50ms is headroom, not aspiration) |
| Write-back p95 | ≤ 30ms | > 30ms (M0 measured 13.4ms) |
| Intent parse p95 | ≤ 1ms | > 1ms (M0 measured microseconds; 1ms catches accidental allocation storms) |
| WER on 30-min set | within 15% relative of whisper-large-v3 | worse, or > +0.3 absolute vs baseline |
| Edit-accuracy on corpus | ≥ 90% | < 90%, or > 1 point below baseline |
| Over-edit rate (chars changed outside requested span) | 0 | any nonzero count |
| Terminal edit success (from M2) | ≥ 95% | < 95% |
| RSS with all models resident | ≤ 4GB | > 4GB |
| 2h soak RSS growth | < 50MB | ≥ 50MB or any crash |
| Idle CPU | < 1% | ≥ 1% |

Latency gates measure from end-of-speech (VAD endpoint) to final text visible
in the field, over the same ≥ 100-utterance scripted run, so numbers are
comparable week to week.

### What we deliberately do not test

- Live-microphone flakiness in CI: audio-path tests use recorded WAV fixtures.
  Real-mic behavior is covered by the weekly manual matrix and dogfooding.
- Cloud services: there are none. Any test that needs the network (other than
  model download in the model-manager test) is a bug.

## Release checklist

Run for every tagged release (automated via Z-12 where marked ☑):

1. ☑ All CI gates green on the release commit, including nightly tiers.
2. ☑ Real-application matrix run within the last 7 days with zero
   pass→fail regressions.
3. ☑ Version bumped, changelog generated from conventional commits, breaking
   protocol changes called out with the protocol version.
4. ☑ Builds signed: Developer ID + notarized + stapled (macOS), Authenticode
   (Windows); Gatekeeper/SmartScreen verified on clean VMs.
5. ☑ Fresh-machine install test per OS: download → install → onboarding →
   first successful dictation, no dev tooling present.
6. Upgrade test: previous release upgraded in place, settings and dictionary
   preserved, TCC grant survives (team-id pinning).
7. ☑ `doctor` reports correct version and healthy state post-install.
8. No open P0/P1 issues against the release milestone.
9. Rollback verified: previous version reinstalls cleanly over the new one.
10. Release notes drafted for humans (what changed, what to re-grant, known
    per-app limitations), reviewed by PD.
11. Tag pushed, artifacts uploaded, package managers updated (Homebrew /
    winget / Flathub from M3).
12. Post-release: monitor issue intake for 48h with a named on-call; any
    release-blocking report within 48h triggers the rollback path.
