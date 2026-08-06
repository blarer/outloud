# Backlog

Prioritized within each milestone. Owner roles: **ML** (ML/ASR engineer),
**SYS** (Rust systems lead), **MAC** (macOS platform engineer), **WIN** /
**LNX** (Windows/Linux platform engineer), **PD** (product/design
player-coach). Sizes: S ≤ 2 days, M ≤ 1 week, L ≤ 3 weeks. Dependencies
reference task ids. Acceptance criteria (AC) are the checkable statement that
closes the task; the generic per-task Definition of Done in
`03-definition-of-done.md` applies on top.

## M1 — macOS alpha

### Platform close-out (from M0)

| id | title | owner | size | deps | acceptance criteria |
|---|---|---|---|---|---|
| P-01 | Confirm Chromium `AXManualAccessibility` path against live Chrome | MAC | S | — | `spike-cli probe` reads and rewrites the Chrome omnibox and a Gmail compose field on a machine with Chrome on the active Space; result recorded in the app matrix |
| P-02 | Confirm Electron path against VS Code and Slack | MAC | S | P-01 | Read + in-place write verified in VS Code editor and Slack message box; strategy reported per app |
| P-03 | Clipboard-paste fallback with save/restore | MAC | M | — | Read-only fields receive text via synthesized paste; prior clipboard contents (text + image) restored within 200ms; verified against a clipboard manager running |
| P-04 | Secure-input field detection | MAC | S | — | Password fields detected via `EnableSecureEventInput` state; app refuses to capture or inject and tells the user why |
| P-05 | Developer ID certificate + signed release pipeline | SYS | M | — | Rebuilt binary keeps its TCC grant across 5 consecutive rebuilds (team-id pinning verified); notarized DMG passes Gatekeeper on a fresh VM |
| P-06 | Onboarding permission flow | PD, MAC | M | P-05 | 5 fresh macOS VMs: user reaches first successful dictation with ≤ 4 clicks after install; flow verifies grant with a live AX probe before declaring success |
| P-07 | Integrate `crates/diag` doctor into the app | SYS | S | — | `doctor` output covers TCC state, responsible process, cdhash/signature, mic permission, model presence; required field in bug reports |
| P-08 | Integrate `crates/text-target` as the injection abstraction | SYS | M | — | `ax-edit` write strategies routed through `text-target`; all existing matrix rows still pass |

### Recognizer pipeline

| id | title | owner | size | deps | acceptance criteria |
|---|---|---|---|---|---|
| R-01 | Audio capture: cpal, 16kHz mono, device hot-swap | SYS | M | — | AirPods connect/disconnect mid-utterance does not crash or lose > 1s audio; verified with scripted device switch |
| R-02 | Silero VAD integration + endpointing | ML | M | R-01 | Endpoint detection ≤ 350ms after speech end (300ms hangover + ≤ 50ms compute) on the 30-min test set; false-endpoint rate < 2% |
| R-03 | Parakeet TDT 0.6b v2 ONNX finalizer | ML | L | R-01 | 5s utterance finalizes in ≤ 200ms on M1 Pro; WER on 30-min test set within 15% relative of whisper-large-v3 |
| R-04 | whisper.cpp backend behind the same trait | ML | M | R-03 | Recognizer trait supports Parakeet and whisper.cpp; model switch in settings without restart; per-backend WER/latency logged |
| R-05 | Moonshine streaming partials tier | ML | L | R-02 | First partial ≤ 250ms after speech onset (p50) on M1 Pro; incremental updates ≤ 150ms apart |
| R-06 | Two-tier arbitration (partials → final replace) | ML | M | R-03, R-05 | Final text replaces partial text with zero duplicated or dropped words on the 30-min test set |
| R-07 | Fixed 30-min WER test set + harness in CI | ML | M | — | `cargo run -p eval -- wer` reproduces the WER number ± 0.1 absolute; runs nightly in CI |
| R-08 | Latency instrumentation end-to-end | SYS | S | R-03 | Every dictation logs per-stage timings locally; `doctor --timings` prints p50/p95 over last 100 utterances |
| R-09 | Model manager: download, verify, quantize | SYS | M | R-03 | Models fetched from HF Hub with checksum verification; interrupted download resumes; total disk usage shown in settings |
| R-10 | Punctuation/casing for non-Whisper backends | ML | M | R-03 | Parakeet native punctuation passes through; streaming-tier text is recased/punctuated before display |

### Edit engine

| id | title | owner | size | deps | acceptance criteria |
|---|---|---|---|---|---|
| E-01 | Edit-accuracy eval corpus (≥ 300 triples) + CI gate | ML, PD | M | — | Corpus covers all intents, casing traps, non-ASCII, joiner-word collisions; CI fails if success rate drops > 1 point below baseline |
| E-02 | Command grammar v2: ~15 intents | SYS | L | E-01 | insert-after/before, scratch-that, new-line/paragraph, bullet, recase-span, select-X, undo/redo parsed deterministically; ≥ 90% success on corpus |
| E-03 | Client-side undo stack | SYS | M | E-02 | "undo that" reverts last edit; stack depth ≥ 10 per field; survives focus switching away and back; covered by unit tests |
| E-04 | LLM fallback: resident Qwen3-1.7B 4-bit | ML | L | E-01 | Freeform edit ("tighten this up") completes in ≤ 900ms p50 with KV-cached system prompt; output constrained to a replacement-span schema (GBNF); never touches text outside the selection |
| E-05 | LLM guardrails + refusal path | ML | M | E-04 | On low-confidence output the edit is not applied and the user is told; measured over-edit rate (changed chars outside requested span) = 0 on corpus |
| E-06 | Selection-scoped edits | MAC | M | E-02 | With a selection active, edits apply to selection only; without one, whole-field scope; both paths in the eval corpus |
| E-07 | Dictation-mode text insertion (non-edit path) | MAC | M | R-06 | Plain dictation inserts at caret via `text-target`, preserving surrounding text, in all matrix apps |

### App shell and UX

| id | title | owner | size | deps | acceptance criteria |
|---|---|---|---|---|---|
| U-01 | Push-to-talk hotkey via CGEventTap | MAC | M | — | Key-down starts capture, key-up finalizes; Fn/Globe and F13-F19 assignable; zero missed key-ups over 500 scripted presses |
| U-02 | Hotkey configuration + conflict detection | PD, SYS | M | U-01 | Up to 5-key combos; conflicts with known system shortcuts flagged at assignment time |
| U-03 | Partials overlay per `docs/ux` design | PD, MAC | L | R-05 | Overlay shows ghost text ≤ 250ms after onset; render at 60fps; visible on all Spaces and over full-screen apps |
| U-04 | Settings window | PD, SYS | L | R-09, U-02 | Hotkey, model, dictionary, per-app toggles all functional; settings persist and hot-reload |
| U-05 | Personal dictionary + replacements | SYS | M | U-04 | ≥ 800 entries supported; case-preserving replacement applied post-ASR in < 1ms; import/export as text file |
| U-06 | Menu-bar app lifecycle, login item | MAC | S | — | Idle CPU < 1%; RSS at idle without models loaded < 150MB |
| U-07 | Terminal insert-mode (Terminal.app, iTerm2) | MAC | M | P-03 | Dictated text lands on the shell line correctly with special chars (`"$'|`) escaped literally; zero mangled lines over the 30-min test set |
| U-08 | Alpha feedback channel + in-app issue capture | PD | S | P-07 | "Report a problem" attaches doctor output (with consent) and opens a prefilled GitHub issue |
| U-09 | 2h soak test automation | SYS | M | R-06 | Scripted 2h dictation run: RSS growth < 50MB, zero crashes, no AX timeout pile-ups; runs weekly in CI on self-hosted M-series |

## M2 — cross-platform beta, terminal + headless

### Daemon and protocol

| id | title | owner | size | deps | acceptance criteria |
|---|---|---|---|---|---|
| D-01 | Engine/daemon split with versioned local protocol | SYS | L | R-06, E-04 | Daemon owns audio/ASR/LLM; protocol documented (versioned JSON-RPC over unix socket); macOS app runs as client with no latency regression > 10ms p50 |
| D-02 | CLI client | SYS | M | D-01 | `outloud-cli dictate`, `outloud-cli edit "<cmd>"`, `outloud-cli status` work headless (no GUI session); exit codes documented |
| D-03 | Remote/SSH operation mode | SYS | L | D-02 | Audio captured locally, text injected on remote host via forwarded socket; end-to-end p50 ≤ local p50 + 100ms on LAN; documented recipe |
| D-04 | Daemon security: socket permissions + auth | SYS | M | D-01 | Socket is user-only (0600); protocol rejects other-uid peers; threat model documented for security review |
| D-05 | Protocol conformance test suite | SYS | M | D-01 | Every protocol message round-trips in tests; version-mismatch handshake yields a clear error, not a hang |

### Terminal edit-by-voice

| id | title | owner | size | deps | acceptance criteria |
|---|---|---|---|---|---|
| T-01 | Shell line-editor integration (zsh/bash/fish) | SYS | L | D-02 | Current command line read and rewritten in place via shell widget/binding; "change foo to bar" works on the live prompt in all 3 shells |
| T-02 | tmux pane targeting | SYS | M | T-01 | Edits target the active tmux pane's line; verified in nested tmux; ≥ 95% eval-corpus success inside tmux |
| T-03 | Terminal emulator matrix (Terminal.app, iTerm2, Alacritty, kitty, Windows Terminal) | SYS, WIN | M | T-01 | Insert + line-edit verified per emulator; published support table |
| T-04 | SSH end-to-end demo + docs | PD, SYS | S | D-03, T-01 | Recorded demo of edit-by-voice on a remote host over SSH; step-by-step doc reproduced by a non-author |
| T-05 | Terminal eval corpus (≥ 100 command-line edits) | ML | M | E-01 | Corpus of real shell commands (paths, flags, pipes); ≥ 95% success gate in CI |

### Windows

| id | title | owner | size | deps | acceptance criteria |
|---|---|---|---|---|---|
| W-01 | UIA TextPattern read-back | WIN | L | D-01 | Focused-field text + selection read in Notepad, Word, Chrome, VS Code within 50ms p95 |
| W-02 | UIA in-place write + SendInput unicode fallback | WIN | L | W-01 | In-place rewrite in ≥ 8 of 10 Windows matrix apps; fallback injection has zero dropped characters on a 1,000-char paste |
| W-03 | Low-level keyboard hook push-to-talk | WIN | M | — | Key-up detected reliably incl. Alt; zero missed releases over 500 scripted presses |
| W-04 | Windows recognizer paths (ONNX DirectML/CPU) | ML, WIN | M | R-03 | Parakeet finalization ≤ 300ms for 5s utterance on RTX 3060; CPU-only tier uses Moonshine with p50 ≤ 800ms |
| W-05 | Authenticode signing + installer | WIN | M | — | Signed installer passes SmartScreen without warnings on a clean Windows 11 VM |
| W-06 | Windows app matrix in CI (scripted) | WIN | M | W-02 | 10-app matrix run scripted on a Windows runner; results posted to the support table |

### Linux

| id | title | owner | size | deps | acceptance criteria |
|---|---|---|---|---|---|
| L-01 | X11 injection (XTest) + AT-SPI2 read-back | LNX | L | D-01 | Read + write in ≥ 8 of 10 Linux matrix apps under X11 |
| L-02 | Wayland IME injection via `zwp_input_method_v2` | LNX | L | D-01 | Text insertion works on sway, Hyprland, KDE (wlroots/kwin paths); support matrix published per compositor |
| L-03 | Wayland hotkey: portal GlobalShortcuts + evdev fallback | LNX | M | — | Push-to-talk works on GNOME, KDE, sway; evdev path documented with udev rule |
| L-04 | Wayland read-back via AT-SPI where available | LNX | M | L-02 | Field read-back verified in GTK4 and Qt6 apps under GNOME/KDE Wayland; honest "insert-only" labeling elsewhere |
| L-05 | Linux packaging: AppImage, deb, AUR; Flatpak eval | LNX | M | L-01 | AppImage runs on Ubuntu LTS + Fedora latest; Flatpak sandbox conflict decision documented |

### Streaming, context, biasing

| id | title | owner | size | deps | acceptance criteria |
|---|---|---|---|---|---|
| S-01 | Live partial injection (revise-in-place ghost text) | MAC | L | R-06, E-07 | Partials appear in-field and are revised without flicker; final replacement is atomic; measured zero residual ghost chars over test set |
| S-02 | Per-app profiles | SYS, PD | L | U-04 | Profile keyed on bundle id/exe: model, formatting, tone; switching apps switches profile in < 100ms; ≥ 5 built-in profiles (email, code, chat, terminal, docs) |
| S-03 | Destination-aware formatting v1 | ML | L | S-02, E-04 | Same utterance formats differently for code comment vs Slack vs email per profile rules; covered in eval corpus with per-destination expected outputs |
| S-04 | Vocabulary biasing into recognizer | ML | L | R-03 | Hotword boosting (phrase list) wired into Parakeet/sherpa decode; ≥ 30% relative error reduction on identifier-heavy test set |
| S-05 | Screen-context harvesting for biasing | MAC | M | S-04 | Focused-field identifiers extracted via AX and fed to biasing list, processed on-device only; measurable casing accuracy gain on code test set |
| S-06 | Correction loop → dictionary learning | ML | M | U-05 | A manual user correction of a mis-recognition is offered as a dictionary entry; accepted entries bias future decodes |

### Infrastructure and community

| id | title | owner | size | deps | acceptance criteria |
|---|---|---|---|---|---|
| I-01 | Auto-update (per-OS) | SYS | M | P-05, W-05 | Update applied without losing settings; rollback path tested; release channel (stable/beta) selectable |
| I-02 | Opt-in crash reporting | SYS | M | — | Off by default; no report leaves the machine without explicit consent; crash-free-session metric computable |
| I-03 | Latency regression gate in CI | SYS, ML | M | R-08 | Nightly benchmark on pinned hardware; thresholds per `03-definition-of-done.md`; regression fails the build |
| I-04 | Plugin/IPC API surface v0 | SYS | M | D-01 | Third party can subscribe to transcripts and register commands over the daemon protocol; 1 example plugin in-repo |
| I-05 | i18n scaffolding for UI strings | PD | M | U-04 | All UI strings externalized; pseudo-locale build renders without truncation |
| I-06 | Analytics stance: telemetry-free, opt-in pings only | PD | S | — | Published privacy doc: no telemetry by default, opt-in anonymous version ping only, verifiable by network inspection (documented test) |
| I-07 | Contributor pipeline: good-first-issues, triage rota | PD | S | — | ≥ 15 labeled good-first-issues; median first-response time on issues < 48h over a month |

## M3 — 1.0

| id | title | owner | size | deps | acceptance criteria |
|---|---|---|---|---|---|
| Z-01 | Security review of injection surface (external) | SYS | L | D-04 | Scoped audit complete; all high/critical findings fixed; report summary published |
| Z-02 | Screen-reader coexistence audit | MAC, WIN, LNX | M | S-01 | VoiceOver, NVDA, Orca each usable while the app runs; no AX event storms (measured event rate) |
| Z-03 | Multilingual: top 8 languages | ML | L | R-04, S-03 | 8 languages with WER published per language; per-language punctuation/formatting rules; language auto-detect or per-profile setting |
| Z-04 | UI localization ≥ 3 locales | PD | M | I-05 | 3 shipped locales, community translation workflow documented |
| Z-05 | Packaging: Homebrew, winget, Flathub | SYS, WIN, LNX | M | I-01 | `brew install`, `winget install`, Flathub listing all install a working build |
| Z-06 | Battery/perf pass | MAC, ML | M | U-06 | Idle < 1% CPU; 1h active dictation < 15% battery on M1 Air; ANE/CoreML offload decision documented with measurements |
| Z-07 | Low-RAM degrade path | ML | M | R-09 | On < 8GB machines the finalizer tier is dropped automatically; app functional at ≤ 1.5GB RSS with Moonshine-only |
| Z-08 | Published benchmark page + reproduction scripts | ML, PD | M | I-03 | WER + latency vs Aqua, Superwhisper, OS dictation; scripts in-repo; a third party has reproduced within 10% |
| Z-09 | Enterprise/MDM install docs + offline model bundle | PD, SYS | M | Z-05 | MDM deployment doc verified against one MDM; offline bundle installs with zero network |
| Z-10 | 1.0 launch execution | PD | M | Z-08 | HN + Product Hunt + r/LocalLLaMA posts live; launch-day on-call rota staffed; issue intake triaged < 24h for launch week |
| Z-11 | Docs site: user guide, per-OS capability matrix, FAQ | PD | L | T-03, L-04 | Every shipped feature documented; capability matrix auto-generated from the compat-report data |
| Z-12 | Release checklist automation | SYS | S | Z-05 | Checklist in `03-definition-of-done.md` runs as a scripted pre-release job; manual steps < 5 |

Task count: 8 + 10 + 7 + 9 (M1) + 5 + 5 + 6 + 5 + 6 + 7 (M2) + 12 (M3) = **80**.
