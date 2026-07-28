# UX Principles

This is the design thesis for the product. Every later document in `docs/ux/`
is an application of these principles to one surface. When a design decision is
contested, it gets resolved here, by principle, not by taste.

The product is a system-wide voice input and edit utility. It is not an app the
user opens, looks at, or thinks about. It is a capability of the computer, like
the keyboard. That single fact drives everything below.

## 1. Invisible by default

The user's attention belongs to the text they are writing, in the application
they are writing it in. Our UI budget is measured in *glances*, not screens.

Concretely:

- **No main window.** The product is a menu-bar / tray presence, a transient
  overlay while listening, and a settings surface you visit twice a year.
  There is no dashboard, no home screen, no stats page begging for attention.
- **The overlay earns every pixel.** It appears only while the microphone is
  hot or an edit is pending confirmation, and it disappears the instant the
  interaction ends. It never takes keyboard focus (see `02-core-interaction.md`).
- **Success is silent.** When dictation lands correctly, the only feedback is
  the text itself appearing where the cursor was. A tool that celebrates
  routine success is a tool that interrupts. Sound, badges, and "inserted!"
  toasts are reserved for *ambiguity and failure*, never for the happy path.
- **The invisible tool must still be findable.** Invisibility is a default,
  not a trap. A single always-working entry point (the tray icon, and the
  `hexavoice` CLI on headless systems) reaches status, settings, and help. If the
  user ever wonders "is it on?", the tray glyph answers without a click.

The test for any proposed UI element: *would a user who dictates 200 times a
day be glad this exists on interaction #200?* If it only helps on interaction
#1, it belongs in onboarding, not in the steady state.

## 2. Latency is the UX

There is no separate "performance work" and "UX work" on this product. The
felt speed of press → speak → text *is* the product experience, and it is the
axis on which Aqua Voice is beaten or not.

The numbers we design against, from `docs/M0-results.md`:

| Stage | Measured | Owner |
|---|---|---|
| Read focused field | 25–33ms | OS integration (proven) |
| Parse spoken command | 2–39µs | deterministic parser |
| Apply transformation | ~1µs | deterministic parser |
| Write back | 13.4ms | OS integration (proven) |
| **OS total** | **~47ms** | **~6% of budget** |
| Recognizer | the rest | **~94% of an 800ms budget** |

The OS half already costs ~47ms. The remaining ~750ms belongs to the
recognizer, and the research stack (Moonshine partials at 150–250ms, Parakeet
TDT finalizing) fits inside it. Hexavoice's cloud round-trip is ~450ms to insert.
Local-first means we can be *faster*, not merely more private, and the UX must
never squander that with animation, debounce, or confirmation theater.

Design consequences:

- **First partial under 250ms or show why not.** If the first visible feedback
  after the hotkey is slower than ~100ms (overlay appears) and the first text
  partial slower than ~250ms, the user perceives a broken hotkey. The overlay
  therefore appears on *key-down*, before any model has produced anything.
- **Never insert a synchronous step into the hot path.** Confirmations,
  previews, and disambiguation exist (see `03-edit-by-voice.md`) but they are
  triggered by *uncertainty*, not applied uniformly. The confident case is
  zero-question.
- **Degrade loudly in latency terms.** If a fallback path (clipboard paste,
  freeform LLM edit) is slower, the overlay says so while it happens
  ("thinking…" with elapsed time), so slowness reads as the machine working,
  not the machine hung.
- **Measure in the product, not the lab.** The per-stage timing breakdown the
  spike CLI prints stays in the shipping product, one keystroke away
  (overlay hover / `hexavoice doctor`). Users on old hardware deserve to see
  *which* stage is slow and what model tier would fix it.

## 3. Trust and privacy are product surfaces, not policy pages

Local-first is the entire differentiator against Hexavoice and Wispr Flow. Hexavoice is
cloud-only: audio always leaves the device, transcripts are retained unless
Privacy Mode is on, there is no offline mode and no Linux. Our counter-position
is worthless if it lives only in a README. It must be *visible and checkable*
in the product itself:

- **The network is off by default and the UI proves it.** The settings surface
  shows "Network: no connections. This app has made 0 network requests since
  launch" as a live counter, not a promise. Model downloads are the one
  network act, they are explicit, user-initiated, and itemized
  (see `01-onboarding.md`).
- **Offline is a first-class mode, not an error.** Unplug the network and
  nothing about dictation changes. The state machine has a `degraded-offline`
  state (see `05-settings-and-states.md`) that only applies to optional
  network features (model updates), never to core function.
- **Show what was heard, own what was written.** History (transcripts and
  audio) is local, inspectable in plain files, and deletable in one action.
  Hexavoice's history cannot be disabled; ours can, per-app or globally, and the
  setting is honored at the capture layer, not the storage layer.
- **The microphone state is never ambiguous.** Hardware truth: the overlay
  and tray glyph reflect the actual capture state, and there is no code path
  where audio is captured without the indicator. This is the one place where
  visibility beats invisibility.

## 4. Failure always names the next action

From `docs/macos-permissions.md`: "accessibility API error -25204" is useless;
"permission not granted, open this pane" is not. This is a platform lesson
promoted to a design law:

- Every error string is written as *situation → next action*, and the next
  action is a button or a command, not advice. "Accessibility permission was
  revoked by a rebuild. [Re-grant…]" not "AX call failed."
- Errors that the product can fix itself get fixed silently and logged, not
  surfaced. Surfacing is reserved for failures that need a human decision.
- When we genuinely cannot act (Wayland without an input-method protocol,
  a read-only field), the message states the boundary honestly and offers the
  best fallback: "This field can't be edited in place. Text is on your
  clipboard, press Ctrl+V to paste." Honesty about platform limits is a trust
  surface (see principle 3).
- No error is terminal without a retry path. The state machine
  (`05-settings-and-states.md`) has no absorbing error state; every error
  state has a named transition out.

## 5. One mental model everywhere, including where there is no screen

The user should carry exactly one model: *hold, speak, release, text appears
where my cursor is. Hold with something selected, speak a change, it changes.*

That model must survive every destination, which is our differentiator:

- GUI text field with full AX support: in-place read and rewrite.
- Field that exposes text but refuses writes: paste fallback, announced.
- Terminal: shell line-buffer editing over a different transport
  (see `04-terminal-and-headless.md`).
- SSH / headless / no compositor: same hotkey, same grammar, indicator moves
  into the terminal itself (OSC, status line, tmux widget).

Capability varies wildly under the hood (three rewrite strategies on macOS
alone, per the README). The user never chooses a strategy. The product detects
what the focused destination supports, uses the best available, and only
mentions it when the difference is user-visible (undo semantics, paste
fallback). Per-destination *behavior* differences (formatting, casing) are
deliberate and configurable; per-destination *interaction* differences are a
bug.

## Anti-goals

Named explicitly so they can be cited in review:

- **Not a companion app.** No feed, no streaks, no "time saved" splash on
  launch. (A stats page may exist, buried, for the curious.)
- **Not a launcher or assistant.** We transform text at the cursor. We do not
  open apps, answer questions, or chain agent actions. Scope creep here
  destroys both trust and the latency budget.
- **Not configurable into a puzzle.** Defaults must be excellent. Settings
  exist in two tiers (see `05-settings-and-states.md`), and the first tier
  fits on one screen.
