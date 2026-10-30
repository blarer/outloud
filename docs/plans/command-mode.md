# Proposal: command mode (app and system actions by voice)

**Status: proposed, not scheduled. Nothing here is implemented.**

Written 2026-10-30 to capture the idea while the reasoning is fresh. This is
a design note arguing for a particular *shape*, not a commitment to build it.
If it is built, it is after 1.0.

## The idea

Today OutLoud puts text where the caret is. Dictation inserts, edit-by-voice
rewrites, and both act on one focused field. Command mode would let a spoken
phrase act on *the machine* instead of on the text:

```
"open Firefox"
"new tab"
"move this to the inbox"
"next song"
```

A phrase comes in, something decides what it means, and something performs
it. The user never touches the mouse.

## Why this is worth considering

Three reasons, in descending order of how much they justify the work.

**1. The infrastructure already exists and is pointed the right way.** The
hard parts of a voice command system are the parts OutLoud already has:
low-latency local recognition, a global hotkey that works on three platforms,
and an accessibility layer that can already read and manipulate other
applications' UI. `ax-edit` reads the focused element through macOS AX; the
same API opens buttons and menus. The Windows `UiaTarget` already walks a UI
tree. Command mode is mostly a new *consumer* of machinery that is built,
rather than new machinery.

**2. It is the natural answer to a question users will ask anyway, and a
decision has already been half-made.** Anyone who dictates into an app for a
week will try to say "send it". Aqua ships exactly that feature, and
`docs/competitive-analysis.md` item 14 already ruled on it:

> **Send It.** *Declined in the general case; already designed for terminals.*
> [...] Aqua's version relies on an LLM deciding whether the user *meant* the
> command, which a suffix match cannot replicate safely: "tell them I'll send
> it tomorrow" would fire.

That reasoning is the strongest argument in this document, and it was reached
before this proposal existed. Command mode is the general case that was
declined, and the thing that makes it declinable-no-longer is the explicit
mode in point 1 below: "tell them I'll send it tomorrow" cannot fire if
commands are only heard when the user asked to be heard. The terminal version
is already specified in `docs/ux/04-terminal-and-headless.md`, opt-in and off
by default, and is the shape to copy.

**3. The accessibility case is strong and is already a stated audience.**
`docs/ux/06-accessibility.md` opens with "No interaction anywhere requires the
mouse." For a user who cannot use one, "open Firefox" is not a convenience, it
is the difference between using the machine and not. The edit engine already
treats mouse-free operation as a first-class constraint; this extends it from
one field to the desktop.

## Why it is riskier than it looks

The honest version. Each of these is a reason the design below is shaped the
way it is.

**A misheard command does something, and the blast radius is not a field.**
Every existing failure mode is bounded: the worst a mis-parsed edit can do is
mangle text in one field, and the undo ring restores it. There is no undo ring
for "empty the trash" or for a mail message sent to the wrong person. The
crate that already governs this project's parsing has the right rule, written
for a much smaller stake:

> **A phrasing that could plausibly mean two different edits must not parse.**
> Escalating costs the user a rephrase; guessing costs them their words, and
> they may not notice.

For commands the second clause gets worse: guessing costs them an action they
cannot take back, and they *will* notice, after it is too late.

**"Routes somehow to a decision maker" is where the danger lives.** The
obvious implementation is to hand the transcript to a language model and let
it pick an action. That turns a speech recognition error into arbitrary action
selection, and it puts a model that cannot be audited in the path between a
noise and a side effect. `crates/llm` exists and already has the correct
instinct about this: it never applies model output directly, it previews. A
command system built on an unconstrained model, with no equivalent of that
preview, would be the least trustworthy component in the project by a wide
margin.

**The recognizer is tuned for prose, not for a command vocabulary.** Whisper
and Parakeet both bias toward fluent English. "open Firefox" is plausible
enough, but application names, contact names, and playlist titles are exactly
the out-of-vocabulary cases ASR handles worst. The roadmap's vocabulary
biasing work (M2, item 7) is a prerequisite for this feeling reliable, not an
enhancement to it.

**Every platform does this differently, and badly.** Launching an app is easy
everywhere (`open -a`, `Start-Process`, `gtk-launch`). *Acting inside* an app
is not: macOS has AppleScript and AX actions, Windows has UIA patterns and
COM, Linux has AT-SPI where the toolkit bothered. "Move to inbox" means
AppleScript to Mail.app, a UIA invoke on a ribbon button in Outlook, and
nothing at all in a web client. The matrix is wider than the text matrix in
`docs/compat-matrix.md`, and that one is already the most expensive document
in the project to keep true.

## The shape I would argue for

### 1. An explicit mode, entered deliberately. Never ambient.

Edit-by-voice deliberately has **no prefix word**: a selection plus an
utterance is enough, and `docs/ux/03-edit-by-voice.md` explains why.

**Command mode should break that rule.** The asymmetry is not inconsistency,
it is the stakes. An edit lands in a field the user is looking at and can
undo. A command does something to the system. The user should have had to say
so, through a separate hotkey or a held modifier, so that no sentence dictated
into a document can ever be interpreted as an instruction to the machine.

This also closes an injection problem that does not exist today. If commands
were ambient, text arriving from anywhere near the microphone (a video, a
meeting, a colleague) could trigger actions. A deliberate mode means the user
is always the one who opened the door.

### 2. A closed grammar first, exactly as the edit engine did it.

The precedent is in `crates/edit-intent`, and the measurement that justifies
it is in `docs/investigations/edit-intent.md`: a 55-command corpus through a
too-narrow grammar produced **10 silently wrong results**. The fix was not a
model, it was a wider deterministic grammar plus a refusal path.

Command mode should start with a small enumerated set (launch an app, switch
window, a handful of per-app verbs) parsed deterministically, with everything
else **refused out loud**. "I do not know that command" costs the user a
rephrase. A model guessing at an action costs them an action.

### 3. A registry of permitted actions, declared, not inferred.

Each action declares what it does, which app it applies to, how it is
performed per platform, and crucially **whether it is reversible**. A command
that is not in the registry cannot be performed, however confident anything is
about what the user meant. This is the same fail-closed direction as
`deny.toml`'s licence allow-list, and for the same reason: a deny-list fails
open when something new appears.

### 4. A model may select from the registry. It never authors an action.

There is a legitimate role for an LLM here, and it is narrow: mapping messy
human phrasing onto one of N enumerated actions, the way `llm::transform`
handles freeform edits. Constrained output (GBNF, as `crates/llm` already
does) to a registry index. Never a shell command, never a synthesized AppleScript,
never an action the registry does not contain.

### 5. Irreversible actions confirm. Always. No setting to disable it.

Send, delete, purchase, post, anything that leaves the machine or cannot be
undone: spoken confirmation, naming what is about to happen. The rest of this
project earns trust by being boring about destructive operations, and that is
worth more than the half second the confirmation costs.

### 6. Two surfaces it should refuse to touch on day one.

- **Anything with money or identity**: purchases, password fields, 2FA
  prompts. `--permissions` already detects secure input; the same detection
  should hard-refuse here.
- **Anything with elevated privilege.** On Windows, UIPI blocks injection into
  elevated windows anyway (`docs/hotkeys.md`), and that accident of platform
  policy is a good default to adopt deliberately everywhere.

## What I would want to know before building it

In order. Each is cheap and could invalidate the plan.

1. **Does anyone want it?** The accessibility argument is strong in theory and
   untested in practice. Ask the people who filed the accessibility-adjacent
   issues, and the beta testers, before writing code.
2. **What is the ASR error rate on a command vocabulary?** Measurable now with
   the existing eval harness and a corpus of 200 command phrases through
   whisper and Parakeet. If application and contact names come back wrong 15%
   of the time, nothing downstream can fix that, and vocabulary biasing becomes
   a hard prerequisite rather than a nice-to-have.
3. **How many apps does "move to inbox" actually work in?** Pick five. Try to
   do it through AX, UIA, and AT-SPI by hand, no code. If it is two of five,
   the per-app verb idea is a maintenance burden with a bad ratio and the
   feature should be launch-and-switch only.
4. **Is this better than the keyboard?** For a mouse-free user, a good
   keyboard shortcut is faster and more reliable than any voice command. The
   honest answer may be that command mode is worth it only for things that
   have no shortcut, or for users who cannot use a keyboard either, which is
   a smaller audience than it first appears.

## Sequencing: why this is post-1.0

Not because it is unattractive, but because of what it would compete with:

- Linux cannot type yet in a default GNOME session (`~/plans/projects/outloud-on-spark.md`).
- Windows has never shipped a signed artifact, so no stranger has run it.
- macOS cannot ship a downloadable app at all, pending notarization.
- The CPU recognizer is a stub (`crates/asr/src/backends/parakeet.rs`), so
  anyone without a GPU gets whisper.cpp on CPU, measured at **8970ms** per
  utterance on the Windows box against 346ms with CUDA.

Every one of those is a user who cannot use the product at all. Command mode
is for users who already can. The one piece of preparatory work worth doing
early is the **vocabulary biasing** in M2, which is already scheduled for its
own reasons and is a prerequisite here.

## Smaller version worth considering first

If this is attractive but not attractive enough to schedule properly, there is
a version with a fraction of the risk:

**Launch and switch only.** "open Firefox", "switch to Slack". No per-app
verbs, no inbox, no sending. Launching is uniform across platforms, is
trivially reversible (close the window), and needs no accessibility tree at
all. It would answer the "does anyone want this" question with real usage
rather than speculation, and if the answer is no, almost nothing was spent.

That is the version I would build first.

## See also

- `crates/edit-intent/src/lib.rs` for the refusal-over-guessing rule and why
  it is written the way it is
- `docs/investigations/edit-intent.md` for the 55-command measurement that
  justified a wider grammar over a model
- `crates/llm/` and `docs/llm.md` for constrained generation and the
  preview-before-apply discipline
- `docs/ux/03-edit-by-voice.md` for the no-prefix-word decision this proposal
  deliberately inverts
- `docs/compat-matrix.md` for the per-app tier work this would multiply
