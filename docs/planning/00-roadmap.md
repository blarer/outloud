# Roadmap: M0 → 1.0

Every number here is grounded in something measured (the M0 spike) or in the
research (`../../../aqua-voice-research/`). Where a number is a target rather
than a measurement, it says so.

Team assumption: 4 people (ML/ASR, Rust systems lead, macOS platform,
Windows/Linux platform at 0.5 growing to 1.0), per `04-team-and-plan.md`.
Weeks are calendar weeks with ~20% slack already included.

## Where we stand: M0 (done, weeks 1-4)

M0 proved the thesis: in-place edit-by-voice through the macOS Accessibility
API works, measured, in real applications.

Measured results (see `docs/M0-results.md`):

| Metric | Measured | Budget share |
|---|---|---|
| Read focused field | 25-33ms | ~4% of 800ms |
| Parse spoken command | 2-39µs | negligible |
| Apply transformation | ~1µs | negligible |
| Write back | 13.4ms | ~2% |
| **OS-integration total** | **~47ms** | **~6%** |

Application matrix: TextEdit pass, Safari chrome pass, Safari web content pass,
Terminal paste-fallback as expected. Chrome/Electron unconfirmed only because of
a test-environment Space-visibility limitation, not a code failure. The
`AXManualAccessibility` opt-in is written but untested against a live window.

The consequence that shapes everything below: **94% of the 800ms latency budget
belongs to speech recognition and generation.** The remaining OS work is
breadth (more apps, more platforms, terminal/headless), not latency.

Sibling workstreams assumed to land during M1: `crates/text-target`
(cross-platform + terminal + headless injection), `crates/diag` (diagnostics,
the `doctor` command), CI/release infrastructure, and `docs/ux/**` (full UX
design). The roadmap schedules integration of those, not their creation.

## M1 — macOS alpha with voice (weeks 5-16, 12 weeks)

Goal: a stranger downloads a DMG, grants two permissions, holds a hotkey, and
dictates and edits by voice in their real apps. macOS only. English only.

Scope, in dependency order:

1. **Recognizer integration** (weeks 5-8). Parakeet TDT 0.6b v2 via ONNX as
   finalizer, following Handy's existing integration. Silero VAD for
   endpointing. Moonshine Small/Medium Streaming for partials in weeks 9-12.
   Research basis: Parakeet 6.05% Open-ASR avg WER, Moonshine 73-107ms
   incremental updates on M-series CPU.
2. **Close M0's open items** (weeks 5-6, parallel). Confirm Chromium
   `AXManualAccessibility` path against live Chrome and VS Code. Implement
   clipboard-paste fallback with save/restore. Both were explicitly deferred
   from M0.
3. **Edit-accuracy eval harness** (weeks 6-8). A corpus of ≥300 (utterance,
   before-text, expected-after-text) triples run in CI. Built now, before more
   commands are added, because the research names edit-accuracy plateau as a
   top-5 risk and says to build the harness at M0-M1, not later.
4. **Undo stack** (weeks 8-10). Client-side, because writing `AXValue` resets
   the host app's undo (measured in M0). "Undo that" reverts the last edit;
   stack depth ≥10 per field.
5. **Command grammar v1** (weeks 8-12). Extend `edit-intent` from 4 intents to
   ~15 (insert, recase span, new line, bullet, scratch-that, select). Grammar
   first, LLM second, per the M0 design decision.
6. **LLM fallback v1** (weeks 10-14). Qwen3-1.7B 4-bit resident via llama.cpp
   or MLX, KV-cached system prompt, GBNF-constrained output. Only `Freeform`
   intents escalate.
7. **Hotkey + overlay + settings** (weeks 10-14). Implement the `docs/ux/**`
   design: push-to-talk via CGEventTap (key-up detection), minimal partials
   overlay, settings for hotkey/model/dictionary.
8. **Onboarding flow** (weeks 12-14). Productize `grant-accessibility.sh`:
   guided permission grant, verified with a live AX probe before declaring
   success. Driven by the four M0 permission findings.
9. **Terminal insert-mode v1** (weeks 13-15). Dictation into Terminal.app and
   iTerm2 via the `text-target` paste path. Edit-by-voice in terminals lands
   in M2; insertion lands now because it is the wedge no competitor has
   locally.
10. **Signing + notarization** (week 5, one-time). Developer ID certificate
    ordered week 5, because M0 proved TCC pins ad-hoc grants to a per-build
    `cdhash`. This blocks every external tester.

Exit criteria (all measurable):

- End-to-end hold-key → speak → final text in field: **p50 ≤ 500ms, p95 ≤
  800ms** after end of speech, on M1 Pro, measured by the built-in timing
  instrumentation over ≥100 utterances.
- First visible partial **≤ 250ms** after speech onset (target from Moonshine's
  measured 73-107ms incremental + 80-160ms audio chunk).
- Edit-command success **≥ 90%** on the eval corpus (≥300 cases).
- Application matrix: read+write pass in **8 of 10** target apps (TextEdit,
  Notes, Mail, Safari chrome, Safari web content, Chrome, VS Code, Slack,
  Discord, Terminal-insert), zero hangs (500ms AX timeout enforced).
- WER within **15% relative** of whisper-large-v3 on a fixed 30-minute test
  set.
- **20 external daily users**; installable from DMG by a stranger with zero
  support messages about permissions (onboarding flow verified by ≥5 fresh
  machines/VMs).
- Survives 2h continuous dictation: RSS growth < 50MB, zero crashes.
- Memory budget: **≤ 4GB RSS** with all models resident (research budget:
  Silero 2MB + Moonshine ~300MB + Parakeet ~2GB + Qwen3-1.7B ~1.2GB).

## M2 — cross-platform beta + terminal/headless differentiator (weeks 17-30, 14 weeks)

Goal: three OSes, and the two capabilities nobody else has: edit-by-voice in
terminals and a headless daemon usable over SSH. Aqua and Wispr are GUI-only
cloud apps; no OSS tool does edit-by-voice at all. This milestone is where the
moat gets built, so it is scheduled as first-class scope with its own exit
criteria, not a stretch goal.

Scope:

1. **Headless daemon + client protocol** (weeks 17-22). Split engine into a
   daemon owning audio/ASR/LLM and thin clients over a versioned local
   socket protocol (JSON-RPC or similar, protocol doc published). Clients:
   macOS app, CLI. This is also the architecture the Windows/Linux ports
   attach to.
2. **Terminal edit-by-voice** (weeks 18-24). Line-editing integration:
   readline/zsh-line-editor aware rewrite of the current command line, tmux
   pane targeting, and a `text-target` terminal backend. Works over SSH via
   the daemon protocol (audio local, injection remote or local re-injection).
3. **Windows port** (weeks 17-26). UIA `TextPattern`/`ValuePattern` for
   read-back and in-place edit, `SendInput` unicode fallback, low-level
   keyboard hook for push-to-talk key-up. Authenticode signing for
   SmartScreen.
4. **Linux port** (weeks 19-28). X11 first-class (XTest + AT-SPI2). Wayland
   best-effort tier: `zwp_input_method_v2` IME for injection on wlroots
   compositors, portal GlobalShortcuts where available, honest capability
   matrix published. Read-back on Wayland via AT-SPI where it exists.
5. **Streaming partials injection** (weeks 20-26). Live ghost-text updates in
   the field (revise-in-place), not paste-on-release, on macOS first.
6. **Per-app profiles + destination-aware formatting v1** (weeks 22-28).
   Frontmost-app detection feeding formatting rules (code vs chat vs email),
   user-editable profiles.
7. **Vocabulary biasing v1** (weeks 24-28). Hotword/phrase-list biasing into
   the recognizer (not post-hoc find-replace), seeded from the focused field's
   identifiers.
8. **Auto-update, crash reporting (opt-in), plugin/IPC API surface** (weeks
   26-30).

Exit criteria:

- **500+ weekly actives** across 3 OSes.
- Latency parity: p50 finalization within **20%** of the macOS number on
  comparable hardware (Windows/NVIDIA and Linux/x86 CPU tiers each measured).
- Terminal: edit-by-voice succeeds on the current shell line in **≥ 95%** of
  eval-corpus commands in Terminal.app, iTerm2, and tmux; dictation works over
  SSH via the daemon on a remote Linux host (demo recorded, latency ≤ p50
  +100ms vs local).
- Windows: read+write pass in 8 of 10 Windows target apps (Notepad, Word,
  Chrome, Edge, VS Code, Slack, Discord, Windows Terminal-insert, Explorer
  rename field, PowerShell-insert).
- Linux X11: 8 of 10; Wayland: published support matrix with ≥3 compositors
  at insert-level support.
- Crash-free sessions **> 99%** (opt-in crash reporting).
- **≥ 10 external contributors** merged.
- Vocabulary biasing: ≥ 30% relative error reduction on an identifier-heavy
  test set (code dictation) vs unbiased baseline.

## M3 — 1.0 (weeks 31-42, 12 weeks)

Goal: hardening, distribution, published benchmarks, security posture.

Scope:

1. Accessibility audit: screen-reader coexistence (VoiceOver, NVDA, Orca).
2. Multilingual: top 8 languages (Parakeet v3 covers 25 European; Moonshine 8;
   per-language formatting rules).
3. Security review of the input-injection surface (external, scoped, budgeted
   ~$20k per the research). Findings triaged before release.
4. Packaging: Homebrew, winget, Flathub, AppImage, deb. MDM/offline-bundle
   install docs.
5. Performance passes: battery (menu-bar idle < 1% CPU), ANE/CoreML offload
   evaluation, low-RAM degrade path (drop finalizer tier below 8GB).
6. Published benchmark page: WER and latency vs Aqua, Superwhisper, built-in
   OS dictation, with reproducible methodology.
7. i18n of the UI (scaffolded in M2, ≥ 3 UI locales shipped).
8. Launch: HN, Product Hunt, r/LocalLLaMA.

Exit criteria:

- Zero open P0/P1 issues at release cut.
- Published, reproducible WER + latency benchmarks (scripts in-repo).
- **2,000+ weekly actives.**
- Security review complete, all high/critical findings fixed.
- ≥ 3 community-maintained plugins or profiles.
- Monthly release cadence sustained for the final 3 months.
- Latency regression gate green for 8 consecutive weeks (thresholds in
  `03-definition-of-done.md`).

## Timeline summary

| Milestone | Weeks | Cumulative | Headline exit number |
|---|---|---|---|
| M0 spike | 1-4 | 4 | done: 47ms OS integration, 6% of budget |
| M1 macOS alpha | 5-16 | 16 | p50 ≤ 500ms end-to-end, 20 daily users |
| M2 3-OS beta + terminal/headless | 17-30 | 30 | 500 WAU, terminal edit ≥95%, SSH demo |
| M3 1.0 | 31-42 | 42 | 2,000 WAU, published benchmarks, audit done |

~10 months to 1.0, consistent with the research's comparable-team analysis
(Talon, VoiceInk, Handy shipped comparable scope with 1-2 people; we have 4
and three OSes).
