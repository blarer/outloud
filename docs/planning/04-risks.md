# Risk Register

Live document. Reviewed at every milestone boundary and whenever an
early-warning signal fires. Likelihood/impact: L/M/H. "Proven" risks have
already bitten during M0 or are documented behavior; "theoretical" risks are
anticipated from research.

## Proven

### R1. TCC fragility breaks Accessibility access

- **Status:** proven in M0 (`docs/macos-permissions.md`).
- **Likelihood:** H (during development, certainty; in the field, M via macOS
  updates).
- **Impact:** H. Without the AX grant the core product does nothing.
- **Owner:** MAC.
- **Mitigation:** Developer ID signing from M1 week 5 so grants pin to the
  team id, not a per-build cdhash (P-05). Onboarding flow verifies the grant
  with a live probe, never trusts the toggle state (P-06). `doctor` (P-07)
  reports responsible process, signature, and TCC state so support is a paste
  not a séance. Paste-injection fallback kept alive as the degraded mode.
  MAC engineer tests each macOS beta every June.
- **Early warning:** spike in issues containing error -25204 with the toggle
  on; any macOS beta release note touching TCC or AX; `doctor` telemetry-free
  self-checks failing on fresh installs during release testing.

### R2. Space/window visibility limits what tests can see

- **Status:** proven in M0 (Notes, Mail, Chrome rows unconfirmed because the
  window server does not expose windows on other Spaces).
- **Likelihood:** H for any automated matrix run; does not affect end users
  (product only acts on the focused app on the current Space).
- **Impact:** M. False "unconfirmed" rows erode trust in the matrix and can
  hide real regressions.
- **Owner:** SYS (CI), MAC.
- **Mitigation:** matrix runner script brings the target app to the current
  Space and frontmost before probing; matrix rows record "not reachable" as a
  distinct state from "fail"; self-hosted runners use a single-Space,
  auto-login desktop session.
- **Early warning:** matrix runs with > 0 "not reachable" rows on CI machines
  (should be exactly 0 there).

### R3. Chromium accessibility opt-in fails or changes

- **Status:** half-proven. M0 established Chromium exposes no AX tree until
  the private `AXManualAccessibility` attribute is set; the opt-in code is
  written but unconfirmed against a live window (P-01).
- **Likelihood:** M. Private attribute; Chromium can rename or gate it.
- **Impact:** H. Chrome + Electron (VS Code, Slack, Discord) is most of the
  matrix.
- **Owner:** MAC.
- **Mitigation:** confirm now (P-01/P-02, first week of M1). Detect
  opt-in failure at runtime and fall back to paste. Pin the behavior in the
  weekly matrix so a Chromium release that breaks it is caught within 7 days.
  Track upstream: Chromium a11y bug tracker, Electron release notes.
- **Early warning:** weekly matrix Chromium rows flip to paste-fallback;
  Chromium source removes/renames `AXManualAccessibility`.

### R4. Host-app hangs via synchronous AX IPC

- **Status:** proven risk class in M0; mitigated by the 500ms messaging
  timeout.
- **Likelihood:** M (spinning Electron renderers exist in the wild).
- **Impact:** M. User perceives the hotkey as dead.
- **Owner:** MAC.
- **Mitigation:** the 500ms timeout is load-bearing and covered by DoD gate
  (AX read p95 ≤ 50ms catches drift). Timeouts surface as a "target app busy"
  message naming the app, never silence.
- **Early warning:** AX read p95 trending up in nightly gates; user reports of
  dead-hotkey correlating with one app.

## Theoretical

### R5. Wayland injection stays blocked

- **Likelihood:** H. No injection or global-hotkey protocol by design;
  `zwp_input_method_v2` is wlroots-adjacent, GNOME support is spotty, and
  read-back outside AT-SPI is essentially impossible.
- **Impact:** H for Linux (which is also the audience most aligned with a
  local-first tool), nil elsewhere.
- **Owner:** LNX.
- **Mitigation:** ship an honest per-compositor capability matrix (L-02, L-04)
  rather than pretending parity. X11 stays first-class. IME approach first,
  ydotool/uinput as documented fallback. Engage wlroots/GNOME upstream early
  (the research notes NLnet funds exactly this kind of input work). Insert-
  only is an acceptable Wayland tier for 1.0; edit-by-voice on Wayland is not
  promised until read-back exists.
- **Early warning:** L-02 prototype cannot inject on 2 of the 3 target
  compositors by week 24; upstream protocol discussions stalling; portal
  GlobalShortcuts adoption not improving across compositor releases.

### R6. Edit-accuracy plateau

- **Likelihood:** M.
- **Impact:** H. Edit-by-voice is the differentiator; 80% success reads as
  broken.
- **Owner:** ML.
- **Mitigation:** constrained grammar first, LLM only for `Freeform` (already
  the M0 architecture). Eval corpus with a CI gate from M1 week 4 (E-01), so
  the plateau is visible the week it starts, not at beta. Over-edit rate gate
  of exactly 0. Opt-in failure-sample collection at M2. LoRA fine-tune of the
  fallback model on synthetic edit data as the escalation path.
- **Early warning:** eval success rate flat across 3 consecutive weeks while
  the corpus grows; rising share of commands escalating to `Freeform`;
  user-reported wrong-edit issues > 5/week.

### R7. Model licence drift

- **Likelihood:** M over a multi-year horizon.
- **Impact:** M-H. Parakeet weights are CC-BY-4.0, Moonshine is MIT today,
  but vendors have relicensed successors before (research notes newer
  Moonshine models need per-card checks; Llama-style community licences are
  already non-open).
- **Owner:** SYS (with legal budget line).
- **Mitigation:** record licence + version + checksum for every shipped model
  in-repo; pin exact model revisions; keep ≥ 2 independently-licensed backends
  per tier (Parakeet + whisper.cpp finalizers, Moonshine + sherpa-onnx
  Zipformer streamers) so any single relicensing is a swap, not a crisis.
  Licence review is part of the model-manager task (R-09) and release
  checklist.
- **Early warning:** new model release under a changed licence; HF model card
  edits to licence fields (watch the repos); any "community licence" language
  appearing in a successor model.

### R8. Latency disappoints on non-Apple-Silicon hardware

- **Likelihood:** H.
- **Impact:** M. The pitch survives, the reviews suffer.
- **Owner:** ML.
- **Mitigation:** tiered defaults (Moonshine-only below 8GB RAM or without
  GPU, Z-07); honest hardware-requirements page; Windows/NVIDIA path gets its
  own pinned-hardware gate (W-04: ≤ 300ms finalization on RTX 3060, CPU tier
  p50 ≤ 800ms).
- **Early warning:** W-04/Linux gate numbers > 20% off the macOS numbers on
  comparable hardware at M2 checkpoint; beta users on CPU-only machines
  churning in the first week.

### R9. Big-platform sherlocking (Apple/MS built-in dictation improves)

- **Likelihood:** M (macOS 26 SpeechTranscriber already exists).
- **Impact:** M.
- **Owner:** PD.
- **Mitigation:** differentiate where platforms will not: cross-app
  edit-by-voice, terminal + SSH + headless (no platform vendor ships this),
  Linux, extensibility, verifiably-offline stance. Treat SpeechTranscriber as
  a free backend behind our recognizer trait, not a competitor.
- **Early warning:** WWDC/Build sessions announcing system-level edit-by-voice
  or field read-back APIs.

### R10. Security incident in the injection surface

- **Likelihood:** L-M.
- **Impact:** H. The app is, structurally, a trusted keylogger-adjacent tool;
  one CVE with a bad story ends adoption.
- **Owner:** SYS.
- **Mitigation:** daemon socket auth from day one (D-04), secure-input-field
  refusal (P-04), external audit before 1.0 (Z-01, ~$20k budgeted),
  security.md + disclosure policy, RFC process for any change to the
  injection or protocol surface.
- **Early warning:** audit findings rated high; fuzzing the protocol suite
  finds crashes; dependency advisories against the FFI/injection crates.

### R11. Key-person risk / burnout

- **Likelihood:** H with 1 person per OS.
- **Impact:** M-H.
- **Owner:** PD + SYS.
- **Mitigation:** docs-as-you-go DoD rule, milestone pairing days, quarterly
  bus-factor review, grants pipeline (NLnet/STF applications at M1) so runway
  does not depend on heroics.
- **Early warning:** one subsystem where only one person has merged anything
  for a quarter; PR review latency > 72h sustained; a named owner's commit
  activity dropping > 50% month over month.

### R12. Recognizer integration underestimates streaming complexity

- **Likelihood:** M.
- **Impact:** M. The two-tier partial→final arbitration (R-06, S-01) has no
  OSS prior art to copy; hypothesis flicker and duplicate-word bugs are the
  known failure modes.
- **Owner:** ML.
- **Mitigation:** ship finalize-on-release first (works without streaming, and
  Handy proves the shape); partials are additive. Zero-duplicate/zero-drop
  acceptance test on the 30-min set (R-06). Kyutai-style fixed-delay decoding
  as fallback architecture if arbitration flickers.
- **Early warning:** R-06 acceptance test still failing 2 weeks after R-05
  lands; overlay flicker complaints from alpha users.
