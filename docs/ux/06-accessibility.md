# Accessibility

This tool is not an app *with* accessibility features. It **is** assistive
technology. A meaningful fraction of users will arrive because typing hurts
(RSI), is impossible (motor impairment), or because writing by keyboard fights
their brain (dyslexia — Aqua's founder is dyslexic and built the product for
himself). For these users the tool is not a 4x-faster convenience, it is the
input method. Every design in `00`–`05` gets audited against that stake.

The bar this sets: features that are "nice to have" for a convenience user
are **load-bearing** for an AT user. Latched mode is convenient for everyone;
for someone who cannot sustain a key hold, it is the product. That reframing
is why several defaults elsewhere in these docs exist at all.

## Motor impairment and RSI

The user may have limited hand strength, tremor, limited range, or pain that
grows with each keystroke. Their goal is *fewer and easier* physical acts.

- **Every hold has a no-hold equivalent.** Tap-to-latch
  (`02-core-interaction.md`) is co-equal with push-to-talk, not buried.
  Latch stop works by voice ("stop listening"), by tap, or by silence
  timeout, so ending dictation never *requires* a physical act.
- **Activation inputs are pluggable.** Foot pedals, sip-and-puff switches,
  and Bluetooth accessibility buttons enumerate as keys/HID and the hotkey
  picker accepts them like any key. Tremor accommodation: the tap/hold
  discrimination threshold (300ms default) and a debounce for repeated
  unintended presses are both adjustable under Advanced, and `aqua doctor`
  includes a "hotkey timing test" that measures the user's own tap and
  recommends thresholds rather than making them guess numbers.
- **No interaction anywhere requires the mouse.** Onboarding, settings,
  disambiguation, previews: all fully keyboard-operable (standard tab order,
  visible focus) and all voice-operable (below). The one screen this is
  hardest for is the permission gauntlet, which necessarily visits System
  Settings; the wizard's instructions include the keyboard-only path for
  each OS pane.
- **Error recovery must not cost more motor acts than the error.** This is
  why disambiguation is spoken ("say two", "the last one") and why undo is a
  phrase. A design that recovers from a mis-recognition via mouse selection
  has failed this audience.

## Voice-only operation: zero keyboard

The strictest case: a user who cannot use a keyboard at all, for whom the
hotkey itself is a barrier. The product must close the loop with voice alone.

- **Wake word ("hey aqua") is the activation path**, which is exactly why
  voice activation exists despite being off by default
  (`02-core-interaction.md`). For this audience the setup wizard offers a
  "voice-only mode" preset during onboarding that enables the wake word,
  latched capture, and spoken confirmations in one choice.
- **A voice command grammar covers the control surface**: "hey aqua, stop
  listening / undo that / show commands / open settings / switch to accurate
  model / go to sleep" (arms a longer wake-word-only state). The grammar is
  the same intent pipeline, so it inherits deterministic parsing and the
  cheat-sheet card enumerates it.
- **Spoken and audible feedback**: an optional earcon set (start/stop/error
  chimes) and optional TTS confirmations ("replaced three", "no match for
  recieve") because the visual diff chip is useless if you cannot look, or
  cannot look *there*. Off by default, one switch in the voice-only preset.
- The one unavoidable seam: OS permission prompts cannot be granted by
  voice-only users without OS-level AT (Voice Control, VoiceOver). The
  wizard detects VoiceOver/Narrator running and phrases its instructions to
  cooperate with them rather than assuming a mouse.

## Dyslexia

Dictation is already the right modality; the danger is our *output* UI
punishing reading.

- **Preview diffs are word-level, not character-level**, with whole-word
  highlighting. Character-diff confetti is illegible to everyone and hostile
  to dyslexic readers.
- **Optional TTS read-back**: "read it back" speaks the last insertion or
  the pending freeform preview, so verification does not require careful
  re-reading. This is the dyslexic-user path through the preview gate in
  `03-edit-by-voice.md`.
- Overlay text uses generous size and spacing, never justified, never
  italic-for-emphasis; hint text does not time out faster than slow reading
  (all transient cards dismiss on action, not only on timers, and timer
  durations are adjustable).
- Freeform instructions like "fix the spelling in this paragraph" are a
  first-class demo case, not an afterthought: it is likely the single most
  valuable command for this audience.

## Screen-reader coexistence: VoiceOver, NVDA, JAWS, Orca

This is the hardest and least-designed-for intersection in the industry: a
blind or low-vision user who also needs dictation. Two real conflicts, both
ours to manage:

**1. We share the accessibility bus.** VoiceOver and our engine both use the
AX API; NVDA/JAWS and our UIA usage likewise. Risks: our synchronous AX
calls into an app while VoiceOver is mid-interaction can contend (mitigated
by our hard 500ms timeouts and read-only probes); more seriously, our
**writes** generate AX change notifications that screen readers announce.
That is *correct* behavior — a blind user must hear what changed — but a
naive `AXValue` full rewrite makes VoiceOver announce the *entire field*,
which for a paragraph is a disaster. Consequence, and it is a hard product
rule: **when a screen reader is running, prefer the narrowest possible write
(`AXSelectedText` range replace) and never the full-value strategy unless it
is the only path; if it is, announce our own concise summary ("replaced
quick with slow") via the screen reader's output API (AX announcements /
NVDA controller client / UIA notifications) instead of letting the blanket
re-read happen.**

**2. Keyboard and speech contention.** VoiceOver captures many chords;
default hotkeys must avoid VO modifiers (Ctrl+Option on macOS), which the
conflict detector (`02`) treats as reserved when VoiceOver is on. And when
the user's *own* TTS is speaking, an open microphone hears it: while a
screen reader is talking, we gate capture with echo suppression against the
loopback and, where the OS exposes it, pause-on-speech ("do not transcribe
while VoiceOver is speaking" is on by default when VO is detected, because
transcribing your screen reader is nonsense output at best).

Our own surfaces must also be readable: the overlay, wizard, settings, and
TUI carry correct roles/labels; every state the tray glyph shows also exists
as an announced, queryable text state (`aqua status` and AX label); the TUI
is a first-class path for screen-reader users on Linux consoles with
`espeakup`/`fenrir` (`04-terminal-and-headless.md`).

**Test matrix, not intentions**: CI-adjacent manual passes with VoiceOver +
Safari/TextEdit, NVDA + Chrome, Orca + Firefox before each release, run by
someone using the screen reader with the display off. "Works with
VoiceOver" claims without this ritual are marketing.

## Fatigue and voice diversity

- **Voice strain is real.** People who dictate all day get vocal fatigue,
  and users with dysarthria or non-standard speech have higher error rates
  on every ASR. UX consequences: correction must be cheap (edit-by-voice is
  itself the accommodation: fixing a word by voice costs one short phrase,
  not a re-dictation), the dictionary biasing must be aggressive and easy to
  feed (`05`), and per-user model choice matters (the accurate tier may be
  the only usable tier for a dysarthric speaker; never nag them toward
  "fast").
- Recognition confidence thresholds (silence detection, wake-word
  sensitivity) are tunable because atypical speech trips defaults.
- Local-first quietly matters here too: speech about one's own health,
  through one's own disability, never leaving the machine is a dignity
  property, not just a security one.

## Commitments

1. Full voice-only operation of every product function (post-permission).
2. Full keyboard-only operation of every product function.
3. Screen-reader-aware write strategy and self-announced edits, as specified
   above, on by default when a screen reader is detected.
4. All activation timings user-adjustable, with a measuring assistant.
5. The accessibility settings are not a ghetto page: the presets
   (voice-only, screen-reader, low-motor) live on the wizard's first screen
   and each just flips settings that all users can also reach individually.
