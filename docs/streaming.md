# Streaming text commit: the stability model

`crates/stream` decides what a streaming dictation is allowed to write into
the user's document, when, and how. This document is the narrative half:
the model, worked examples, the degradation matrix, and the failure modes.
The executable half is the crate's tests, including property tests over
adversarial Unicode.

## The problem

A streaming recognizer revises its hypothesis. This sequence is normal:

```
t=0.3s  "recognise"
t=0.5s  "recognise speech"
t=0.9s  "wreck a nice beach"
```

If "recognise speech" was already injected into the user's document, the
revision at t=0.9s forces a visible correction of text the user may already
be reading, or copying, or has typed after. Users tolerate ~450ms of waiting
(Hexavoice's Instant mode). They do not tolerate words rewriting themselves.
Perceived latency is set by when the *first* character appears, so the goal
is: stream early, and never write anything that has to be taken back.

## The stability model: local agreement

`CommitHorizon` implements **LocalAgreement-N** (from the simultaneous
translation literature): a prefix is committed only once the last N
consecutive hypotheses agree on it, where hypotheses are whole-replacement
partials (the `asr` crate's contract: every `Partial` replaces the previous
one, never appends).

Two refinements on raw agreement:

- **Word-boundary trimming.** Agreement is measured in grapheme clusters,
  then the committable length is trimmed *back* to a UAX #29 word boundary
  of the current hypothesis. Agreeing on `"thermodynami"` (shared prefix of
  "thermodynamics"/"thermodynamite") commits nothing: half a word in a
  document reads as a typo. UAX #29 also handles CJK, where each Han/Kana
  syllable is its own boundary unit, so Chinese commits per syllable rather
  than waiting for a space that will never come.
- **Lookback holdout.** The last `lookback_words` words of an
  otherwise-stable prefix are withheld anyway. Recognizers churn hardest at
  the audio frontier, and two partials can agree there by accident. Holding
  one word back costs one word of lag and absorbs most frontier flips.

Both knobs are in `HorizonConfig`. Defaults: `stability: 3`,
`lookback_words: 1`. `stability: 1` commits every partial immediately and
is only sane for a finalizer's own output.

### The two safety properties

1. **Committed text is never retracted.** The committed prefix grows
   monotonically. The type has no operation that shrinks it, and a property
   test drives adversarial hypothesis sequences (total rewrites, CJK,
   emoji ZWJ, combining marks) through it asserting every committed state
   extends the previous one.
2. **Contradiction does not panic the horizon.** If the recognizer rewrites
   text we already committed, we *hold*: the committed text stands, the
   divergent hypothesis rides in the overlay tail, and the final pass
   settles it with one consolidated correction diff (the UX doc's
   "append-only writes" rule: one visible settle at the end, not churn
   mid-read).

## Worked example: hypothesis revision, stability 2, lookback 0

| # | Hypothesis | Agreed prefix (with prev) | Committed after | Written to app |
|---|---|---|---|---|
| 1 | `recognise` | (no history) | `` | nothing |
| 2 | `recognise speech` | `recognise` | `recognise` | append `recognise` |
| 3 | `wreck a nice beach` | `` (total flip) | `recognise` (held) | nothing |
| 4 | `wreck a nice beach today` | `wreck a nice beach` | `recognise` (contradicts commit, held) | nothing |
| final | `wreck a nice beach today.` | n/a | replaced wholesale | one correction splice |

The final settle is computed by `minimal_edit("recognise", "wreck a nice
beach today.")`, a single splice replacing `recognise`. The user sees one
settle at the moment they expect text to finalize, never mid-stream churn.

Counterfactual with no horizon (commit every partial): the app would show
`recognise speech`, then visibly rewrite it to `wreck a nice beach`. That
is the flickering-garbage failure this whole crate exists to prevent.

## The minimal diff

`minimal_edit(old, new)` returns one contiguous splice `{range, insert}`
such that applying it to `old` yields exactly `new` (property-tested over
arbitrary Unicode pairs). Construction:

1. Longest common prefix and suffix in **extended grapheme clusters**, so
   the splice can never cut an emoji ZWJ family, a flag pair, or a
   combining mark off its base. (`"cafe"` -> `"café"` replaces the whole
   `cafe`, because `e` and `é` are different clusters.)
2. **Widen** both splice ends outward to the nearest UAX #29 word boundary
   shared by both strings. Widening preserves the roundtrip property (the
   re-included common text is re-inserted verbatim) while making the edit
   word-shaped: `"hello wor"` -> `"hello world"` replaces `wor`..`world`,
   not a mid-word insertion of `ld`.

Worked minimal edits:

| Old | New | Splice |
|---|---|---|
| `hello ` | `hello world` | insert `world` at 6 (pure append) |
| `change hello to goodbye` | `change hallo to goodbye` | replace bytes 7..12 (`hello`) with `hallo` |
| `recognise speech` | `wreck a nice beach` | replace 0..16 (whole text: no shared word, despite the shared `ch` tail) |
| `我想吃饭` | `我想吃面` | replace the single syllable `饭` with `面` |
| `hi 👩‍👩‍👧‍👦 there` | `hi 👨‍👨‍👦 there` | replace exactly the whole ZWJ sequence |

Why one contiguous splice rather than an edit script: it is the only edit
shape every transport tier can express (AX selected-range replacement, IME
commit, or backspaces-plus-typing), and a single splice cannot interleave
badly with concurrent field changes.

## Coalescing and backpressure

Every write is synchronous IPC into another process (`docs/latency.md`:
~264us warm, milliseconds cold, ~20x worse in Chrome). Two rules, both in
`Coalescer`, both pure logic over injected time:

- **Cadence.** At most one release per 80ms (`DEFAULT_WRITE_INTERVAL`, per
  the UX doc's write tick). The first write of an utterance releases
  immediately: first-character latency is the product. Later offers park.
- **Latest-wins, no queue.** Capacity is exactly one pending state; each
  newcomer overwrites it. When a write is in flight, nothing releases until
  the caller reports `write_done`, so a slow transport (spinning Electron
  renderer) causes intermediates to be *dropped*, not queued. A queue would
  deliver text that was already superseded when it was enqueued: stale text
  is worse than late text. Dropping is safe because the session diffs
  against the last *actually written* state at release time, so a dropped
  intermediate just makes the next splice slightly bigger.

A slow write also consumes its own interval: the next release waits a full
80ms from write *completion*, so a struggling transport is never hammered.

## Degradation matrix

Streaming requires revising already-written text. `TransportProfile`
mirrors `text_target::Capabilities` (data-coupled, not type-coupled, so
`stream` builds headless). `DictationSession` decides the mode once, at
start, from `can_write_in_place`:

| Transport (tier) | `can_write_in_place` | `preserves_undo` | Mode chosen | Why |
|---|---|---|---|---|
| Accessibility, `AXSelectedText` | yes | yes | Streaming (if preferred) | full revise + host undo intact |
| Accessibility, `AXValue` only | yes | no | Streaming (if preferred) | can revise, but our `UndoRing` is the only undo |
| Input method / IME | no | no | Buffered | commit-only insertion; a "revision" would insert a duplicate |
| Synthetic keystrokes | no | no | Buffered | fire-and-forget keys cannot address old text |
| Clipboard paste | no | no | Buffered | one paste per interaction, period |
| Terminal (OSC 52, bracketed paste) | no | no | Buffered | terminals append at the cursor; no retraction |
| Headless daemon | depends on peer | depends | per capability | the protocol reports what the editor can do |

Buffered mode is not an error path: it is the product default
(commit-on-release, one write, one undo entry, zero mid-stream flicker).
Partials still render in the overlay in both modes. The user-facing
contract is that asking for streaming into a clipboard silently gets the
correct behavior instead of garbage.

Note `docs/ux/02-core-interaction.md` rule 4 also excludes fields that only
support whole-`AXValue` rewrites from streaming even though they are
technically writable in place; the caller expresses that by passing
`can_write_in_place: false` for such fields.

## Undo

`AXValue` writes reset the host's undo stack, so the product keeps its own:
`UndoRing`, a fixed-capacity ring of `(before, after, caret)` snapshots.

- **One dictation = one undo unit.** `begin_unit` snapshots the field once
  before the first streamed write; `end_unit` seals it after the last. The
  forty writes in between never touch the ring.
- **The stale-snapshot guard.** `undo` compares the field's *current*
  contents against the unit's after-image. On mismatch (the user typed
  since) it refuses the blind restore and surfaces the before-text for the
  clipboard path: "The field changed since that dictation. Copied the
  previous version." Never stomp user keystrokes.
- No-op units (before == after) are discarded: an undo step that does
  nothing makes "undo that" feel broken.

## Failure modes and their symptoms

| Failure | Symptom the user sees | Prevented by |
|---|---|---|
| Committing unstable text | words rewrite themselves mid-read | stability window + never-retract invariant (property-tested) |
| Committing half a word | `thermodynami` sits in the doc looking like a typo | word-boundary trim on commits |
| Character-level diffs | mid-word garbage during revisions (`recogni||se spee||ch`) | word-boundary widening in `minimal_edit` |
| Splitting a grapheme | broken emoji / detached accents, possible app-side crash on invalid range | grapheme-cluster prefix/suffix; diff range provably sliceable |
| Whole-field rewrites per partial | caret jumps, selection lost, flicker, host undo spam | minimal splice against last-written state |
| Per-partial writes | field stutters at recognizer cadence, IPC storm | 80ms coalescing tick |
| Queueing behind a slow write | text keeps arriving seconds after speech stopped, all of it stale | one-slot latest-wins pending state, in-flight gate |
| Streaming into clipboard/terminal | duplicated text: every revision pastes another copy | capability check degrades to buffered, silently |
| Forty undo steps per dictation | Cmd+Z (ours) walks back word by word | begin/end unit sealing |
| Restoring a stale snapshot | user's post-dictation typing destroyed | after-image comparison in `UndoRing::undo` |
| Final pass contradicts committed text | (unavoidable) one visible settle at the end | `finish` computes one consolidated correction splice, the settle users tolerate |

## What this crate deliberately does not do

- **No OS calls, no threads, no clock reads.** Time is injected, writes are
  returned as commands. This is what makes every branch testable in CI and
  keeps transport ownership in `text-target`.
- **No hypothesis merging.** Partials replace wholesale and the finalizer
  replaces wholesale (the `asr` R-06 rule). Merging is where duplicated and
  dropped words come from.
- **No punctuation/formatting.** That is the finalizer's and formatter's
  job; this layer transports whatever text it is given.
