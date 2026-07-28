# Competitive analysis: Aqua Voice

Research date: 2026-07-28. Sources: [aquavoice.com/guide](https://aquavoice.com/guide)
and every page beneath it, [llms.txt](https://aquavoice.com/llms.txt),
[the FAQ](https://aquavoice.com/info/faq),
[the changelog](https://aquavoice.com/changelog) (all releases 0.2.0 → 0.18.0),
and the Edit Mode and Send It announcement posts. Hexavoice's shipping version at
time of writing is **0.18.0** (26 July 2026).

This document has three parts: what Hexavoice actually ships, an honest gap analysis
against this repository with file-level citations, and a prioritized build
order.

The framing that matters throughout: **Hexavoice is a cloud product with a
proprietary ASR model (Avalon) and an LLM post-processing stage.** Several of
its headline features are LLM features wearing a product name. We cannot match
those with string matching, and pretending otherwise produces a worse product
than admitting it and shipping the deterministic 80%.

---

## Part 1: Feature inventory

### 1.1 Dictation modes

| Hexavoice feature | What it does |
|---|---|
| **Instant Mode** | Press key → talk → release → text appears. Startup <200ms, result ~450ms. |
| **Realtime mode** | Press → talk → words appear live → release. "Continuous output, best with Deep Context." |
| **Hands-free session** | Double-press the key, or hold and tap Space, to latch. Ends by voice, not by key. |
| **Startup** | Advertised under 50ms. |

Hexavoice explicitly does *not* claim one mode is correct: "Instant Mode has the
lowest latency and is great for short clips or chaining Hexavoice many times in a
row; Realtime mode gives maximum contextual understanding."

### 1.2 Edit Mode (their flagship, shipped 0.17.0, July 2026)

This is the feature that overlaps most with our differentiator, and it landed
only three months ago. Mechanics:

- Select text in any app, hold the *same* dictation key, say the change.
- No selection → normal dictation. Selection → Edit Mode, automatically. No
  second hotkey and no mode toggle.
- A chip appears above the pill saying what is held: **"12 words selected"**.
- Two deliberate exclusions, both worth copying:
  - **Search fields and browser address bars** always dictate, never edit.
    ("Selecting a URL in your address bar and dictating should type a query,
    not rewrite the URL.")
  - **Selections over 6,000 characters** fall back to dictation.
- Every edit is recorded in History with the original selection, the result,
  and the exact words spoken.
- Settings carries a **"Try it"** card that runs a short tour ending on a live,
  editable email draft.

Documented example transformations:

| Selection | Utterance | Result |
|---|---|---|
| `Great to meet you, Tony!` | "It's T - O - N - I" | `Great to meet you, Toni!` |
| `See you tomorrow!` | "Translate to Japanese" | `また明日！` |
| `Revenue hit 10 million dollars` | "abbreviate" | `Revenue hit $10M.` |
| `i can send that over tomorrow if that works for you` | "Fix the grammar" | `I can send that over tomorrow if that works for you.` |
| `Thank you very much for taking the time to review this in such detail.` | "Make it shorter" | `Thanks for reviewing this so carefully.` |
| `The meeting is at 5pm on Thursday.` | "Change 5pm to 6pm" | `The meeting is at 6pm on Thursday.` |
| `very very` | "Delete that" | *(deleted)* |

### 1.3 The command vocabulary Hexavoice teaches

This is the section that matters most for `crates/edit-intent`, because it is
what real users are trained to say.

**Replacement, explicit.** Only one shape is documented: `"change X to Y"`.
Everything else in their examples is command-word-free.

**Replacement, implicit — the big one.** Hexavoice's headline claim is that *no
command words are needed at all*:

> "If what you say is clearly a corrected version of what you selected, Hexavoice
> treats it as the replacement. Select 'Hey John, let's meet on Tuesday.', say
> 'Hey John, let's meet on Monday.', and you get the Monday version — no
> 'change it to,' no 'replace with.'"

**Spelling correction.** `"It's T - O - N - I"` — a spelled-out letter sequence
is understood as a respelling of a name in the selection.

**Instruction verbs** (all LLM-backed): `"translate to <language>"`,
`"fix the grammar"`, `"make it shorter"`, `"abbreviate"`, `"make it more
formal"`.

**Deletion**: `"delete that"`.

**Undo, as a spoken follow-up.** Three documented phrasings, all equivalent to
stepping backwards through a stack of edits on the same selection:

- `"undo that"`
- `"go back one step"`
- `"go back to the original"`

**Send It** (0.18.0, July 2026). End a hands-free Realtime dictation with
`"send it"`; Hexavoice strips the command from the transcript, inserts the message,
and presses Return. Documented properties:

- Accepted variants: `"send it"`, `"send"`, `"send message"`, and polite forms
  like `"please send this message"`. "Close variants usually land too."
- **Positional rule**: the command must be the *last* thing said. "Tell them
  I'll send it tomorrow" stays as text; "Running late, be there soon, send it"
  sends. Hexavoice explicitly says it "reads the whole thing you said and works out
  what you meant, rather than matching a keyword" — i.e. this is an LLM
  classifier, not a suffix match.
- Localized: 送って / 送信してください (ja), envíalo / envía el mensaje (es),
  envoie-le / envoie le message (fr), schick es ab / abschicken (de),
  отправь / отправь сообщение (ru). English "send it" works in every language.

**File tagging** (0.10, October 2025). In Cursor and Windsurf, spoken filenames
become `@`-tags that trigger the editor's autocomplete:

- Implicit forms: `"main ts"` → `@main.ts`, `"plates events ts"` →
  `@plates.events.ts`, `"settings pi"` → `@settings.py`, `"main dot pi"` →
  `@main.py`, `"cli dot go"` → `@cli.go`.
- Explicit trigger words: `@`, `"at"`, `"tag"`, `"tagged"`. `"at main"` →
  `@main.ts`, `"tag user profile"` → `@userProfile.tsx`.
- Disambiguation is delegated to the editor's own autocomplete; Hexavoice emits the
  tag and lets the IDE resolve it.

**Punctuation.** Notably, Hexavoice teaches *no* punctuation commands at all. There
is no "period", no "new paragraph", no "open quote" anywhere in the guide.
Punctuation is inferred by the LLM, and users who want control are pointed at
Custom Instructions ("For bullet lists, do not add a trailing period"). This is
a real product position, not an omission: they consider spoken punctuation a
legacy Dragon-ism.

### 1.4 Customization surfaces

| Feature | Shape | Limits |
|---|---|---|
| **Dictionary** | Flat list of terms. Text box + `+ Add`, per-row `Remove`, bulk add (0.14.21). Context-aware casing: enter `factorio`, get `Factorio` sentence-initially. | 5 entries free, **800 on Pro**, shared team dictionary on Enterprise. |
| **Replacements** | `phrase → expansion`. Pitched for repeated LLM prompts, email addresses, calendar links. Per-entry **preserve case** and **strip punctuation** flags (0.11.9). | Pro. |
| **Custom Instructions** | Free-text natural-language style rules, ChatGPT-style. Hexavoice's own advice: write rules as a list, and **always include a good-output and bad-output example**, because examples are what make the model comply. | Pro. |
| **Casual Messaging** | A setting (0.10.4) that enforces lowercase in Slack, iMessage, Discord. | — |
| **File Tagging** | Toggle in Settings → File Tagging. | — |
| **Languages** | 49 languages plus **Auto-Detect**. Auto-Detect is unavailable in Realtime mode. | — |
| **History** | Local, on-device. Per row: ▶ replay audio, 🔁 re-run transcription, 📋 copy, 👍/👎 feedback, and "Undo" to recover the pre-LLM **base transcript**. Audio deleted after 3 days. | — |
| **Keybindings** | Up to **5 bindings per action**, up to **5 keys per chord**, F13–F19 supported, "Show Recommended" presets, "Reset to Defaults". Device-local, deliberately not synced. Separate bindable action: **Paste Last Transcript** (Cmd+Ctrl+V). | — |
| **Deep Context** | Reads the screen to bias recognition toward on-screen identifiers. Off by default. Produces `` `canonical_title` `` and `ContextResponse` from spoken English. | — |

### 1.5 System behaviour and UX affordances

- **Menu bar resident** with an optional Dock icon. Two separate quit paths
  ("Quit Aqua Voice (UI)" vs "Quit Hexavoice Completely") which confuse users badly
  enough to need a dedicated guide page warning about them.
- **Language picker in the menu-bar context menu** (0.2.12), added explicitly
  "for easy access". It is the one setting they promoted out of Settings.
- **Floating Bar** (their overlay), hideable via Settings → System.
- **Mute background audio while recording** (macOS 0.7.0, Windows 0.11.6).
- **Avoid clipboard history**: opt out of writing the transcript to the
  clipboard; **clipboard contents are restored after paste** (0.2.0).
- **Settings sync** across devices for dictionary, instructions, language, and
  mode. Keybindings deliberately excluded.
- **Stats card / dashboard**: time saved, WPM, activity graph, shareable.
  "Hexavoice Wrapped" year-in-review.
- Settings tabs, flat left rail: Home · Keybindings · Model · Language ·
  Dictionary · Custom Instructions · Replacements · File Tagging · History ·
  System · Plan.
- Support story is thin: their answer to "I'm having issues" is *zip
  `~/Library/Logs/Aqua Voice/` and email support*. There is no diagnostic
  command.

### 1.6 What Hexavoice cannot do

Stated plainly in their own FAQ and comparison pages:

- **No offline mode.** "Yes, Hexavoice is cloud-based and needs a connection."
- Transcripts **may be retained to improve the model** unless Privacy Mode is
  on. Zero Data Retention is Enterprise-only.
- Requires WSS on port 443 to `api.`, `core.`, and `realtime.aquavoice.com`;
  corporate SSL inspection breaks it.
- **No terminal line editing.** They dictate *into* terminals; they cannot
  rewrite a command line through the shell's line editor.
- **No headless / SSH operation.**
- No HIPAA BAA.
- Paid: $8/mo Pro, 1,000 lifetime free words.

---

## Part 2: Gap analysis

Legend: **✅ shipped** · **🟡 partial** (code exists, not wired or incomplete)
· **❌ missing** · **🚫 out of scope** (needs a cloud LLM we do not have).

### 2.1 Dictation core

| Hexavoice feature | Us | Where |
|---|---|---|
| Push-to-talk, hold-key dictation | ✅ | `crates/hotkey`, `crates/hexad/src/pipeline.rs`; 189ms measured |
| Instant Mode (commit on release) | ✅ | `insertion.mode = "on-release"`, `crates/config/src/schema.rs:189` |
| Realtime / streaming injection | 🟡 | `crates/stream` is fully built (`session.rs`, `undo.rs`) but **`hexad/Cargo.toml` does not depend on `stream`**. The setting exists in the menu; the behaviour does not. |
| Hands-free latch (double-press / hold+Space) | ❌ | No latch mode anywhere. `silence-timeout-ms` exists in schema only. |
| System-wide injection into any app | ✅ | `crates/text-target` with six tiers (`Accessibility`, `InputMethod`, `SyntheticKeys`, `Clipboard`, `TerminalNative`, `Headless`) — strictly more tiers than Hexavoice documents |
| Clipboard preserved / restored after paste | ✅ | `crates/text-target/src/targets/clipboard.rs` |
| Startup <50ms, insert ~450ms | ✅ **better** | 131–189ms end to end, measured on M4 Pro (README) |
| Mute background audio while recording | ❌ | Not implemented, not in schema |
| Paste Last Transcript hotkey | ❌ | The daemon does not retain the last transcript at all |

### 2.2 Edit-by-voice

| Hexavoice feature | Us | Where |
|---|---|---|
| Selection + same hotkey → edit, no mode switch | ✅ | `crates/hexad/src/pipeline.rs`, 131ms measured |
| `"change X to Y"` | ✅ | `crates/edit-intent/src/lib.rs:52` — and we accept **four** shapes to Hexavoice's one (`change…to`, `replace…with`, `make…into`, `swap…for`) |
| Delete | ✅ | `edit-intent` line 74: `delete`, `remove`, `get rid of`, `scratch` |
| Append | ✅ | line 86: `append`, `add`, `also add` |
| Recase (upper/lower/title/sentence) | ✅ | line 108–111. Hexavoice does not document these at all. |
| **Command-word-free replacement** ("say the corrected version") | ❌ | Our parser falls through to `Freeform` and refuses. This is Hexavoice's single most-promoted Edit Mode behaviour. |
| **Spoken undo** (`"undo that"` / `"go back one step"` / `"go back to the original"`) | 🟡 | `crates/stream/src/undo.rs` implements the ring with a correct staleness check, but no parser verb reaches it and `hexad` does not link `stream`. |
| Spelling correction (`"It's T-O-N-I"`) | ❌ | No letter-sequence handling |
| `"translate to X"` | 🚫 | Needs a multilingual LLM |
| `"fix the grammar"` / `"make it shorter"` / `"abbreviate"` / `"more formal"` | 🟡 → 🚫 | `crates/llm` exists complete (`transform`, `prompt`, `guardrail`, `sanitize`, `llama_backend`) but **`hexad` has no `llm` dependency**; `Outcome::FreeformUnsupported` in `pipeline.rs:364` is the honest refusal. Wiring it makes these work locally at Qwen3-1.7B quality, which is below Hexavoice's. |
| Selection-size guard (6,000 chars) | ❌ | No cap; a whole-document selection would be attempted |
| **Address-bar / search-field exclusion** | ❌ | `crates/text-target/src/detect.rs` detects tiers, not field roles. A URL bar today is editable. |
| "N words selected" chip | ❌ | `crates/overlay` has no selection chip |
| Terminal command-line edit-by-voice | ✅ **unique** | `crates/shell-bridge`; Hexavoice cannot do this |

### 2.3 Customization

| Hexavoice feature | Us | Where |
|---|---|---|
| Dictionary / custom terms | 🟡 | `crates/config/src/vocab.rs` is excellent and **unlimited** (Hexavoice caps at 800): bias terms, replacements, per-entry `[case]` / `[strip-punct]` flags, plus fuzzy correction via `fuzzy.rs` ("cube cuddle" → `kubectl`). **But `vocab::` is referenced nowhere outside `crates/config`** — it is not wired into the pipeline. |
| Replacements / snippets | 🟡 | Same file, same mechanism (`my address -> 12 Elm St`), same wiring gap |
| Per-entry preserve-case / strip-punctuation | ✅ | `vocab.rs:29-36` — feature parity with 0.11.9 |
| Vocabulary sets toggled per profile | 🟡 | `vocabulary.sets` key exists (`schema.rs:243`); no set loader |
| Custom Instructions (free-text style rules) | ❌ | Nothing. Requires an LLM in the loop to be meaningful; the local approximation is deterministic formatting flags plus a system-prompt file once `llm` is wired. |
| Casual-lowercase for chat apps | 🟡 | `formatting.casing = "casual-lowercase"` exists in schema and in the menu; **no formatter implements it** (grep finds only schema, tests, and the menu row) |
| Smart quotes, trailing punctuation | 🟡 | Schema keys only, same story |
| Per-app profiles | 🟡 | `crates/config/src/profile.rs` is complete — matchers, specificity ordering, `WinReason` explainability — and `profile::select` is **called from nowhere**. Also no built-in profiles ship, though docs/ux/05 promises ~10. |
| File tagging (`"main ts"` → `@main.ts`) | ❌ | Entirely absent |
| Languages (49 + auto-detect) | ❌ | `language` key in `schema.rs:171`; nothing reads it. Our recognizer is Apple `SpeechTranscriber`, which does support many locales, so this is plumbing, not modelling. |
| Deep Context (screen reading for term bias) | 🟡 **and we can beat it** | We already read the focused field via AX for editing. Feeding the surrounding field text into vocabulary bias is a strictly local version of Deep Context, and unlike Hexavoice it never leaves the machine. |

### 2.4 System surfaces

| Hexavoice feature | Us | Where |
|---|---|---|
| Menu bar status item + click menu | ✅ | `crates/overlay/src/menu.rs`, `status_item.rs`, `crates/hexad/src/menubar.rs`. Glyph per state, settings write through `config::update_file`. |
| Overlay / floating bar, hideable | ✅ | `crates/overlay`, `overlay.position` incl. `hidden` |
| Settings window | ❌ | Menu submenu only. Being built now. |
| Onboarding wizard with verified permissions | 🟡 **design beats theirs** | `docs/ux/01-onboarding.md` specifies a probe-based checklist that is better than anything Hexavoice ships; `scripts/grant-accessibility.sh` and `crates/diag` exist; the wizard itself does not. |
| History (transcripts + audio) | ❌ | `history.enabled` in `schema.rs:219`; no history store, no replay, no re-transcribe |
| Re-run transcription on stored audio | ❌ | Follows from history |
| Multiple hotkeys / chord recorder | 🟡 | `crates/hotkey` binds one chord; `conflict.rs` probes conflicts (better than Hexavoice's "try it in different apps" advice). Menu offers four presets, no recorder, no alternates. |
| Launch at login | 🟡 | Key + menu row exist; no `SMAppService` registration |
| Stats / dashboard | ❌ | — |
| Diagnostics | ✅ **better** | `scripts/doctor.sh`, `crates/diag`, menu "Run diagnostics…". Hexavoice's equivalent is "zip your logs and email us." |
| Offline operation | ✅ **unique** | Entire product |
| Headless / SSH | ✅ **unique** | `Tier::Headless`, `--no-default-features` |

### 2.5 The honest summary

Three quarters of the gaps are **not missing code**. They are missing *wires*.
`vocab.rs`, `profile.rs`, `stream/undo.rs`, and the whole `llm` crate are built,
tested, and disconnected. That is the single most important finding in this
document, and it sets the priority order below.

What we genuinely cannot match: `"translate to Japanese"`, real style
compliance from free-text Custom Instructions, and Send It's semantic
"did-they-mean-it" classification. Each of those is a large cloud LLM doing
something a 1.7B local model does unreliably. The correct response is a
deterministic approximation plus an honest refusal, which is already the
established pattern in `pipeline.rs` (`FreeformUnsupported` tells the user
rather than silently mangling their text).

---

## Part 3: What to build next

Ordered by value ÷ effort. Effort is engineer-days, assuming familiarity.

### Tier 1 — connect what already exists (highest ratio in the repo)

**1. Wire `vocab` into the pipeline.** *~1 day. Very high value.*
Load vocabulary files, run `Vocabulary::correct` on every transcript before
injection. This single change delivers Hexavoice's Dictionary *and* Replacements
*and* their per-entry flags at once, uncapped, from code that is already
written and tested. Today the feature exists only as a folder the menu can
open.

**2. Wire `profile::select` into the pipeline.** *~1-2 days. High value.*
Resolve the frontmost app, select a profile, apply overrides. Then implement
the three formatting flags that are currently schema-only
(`formatting.casing`, `smart-quotes`, `trailing-punctuation`) and ship built-in
profiles for terminals, Slack/Discord/iMessage (casual-lowercase — Hexavoice's
"Casual Messaging" feature, for free), code editors, and browsers. Two features
land from one wire.

**3. Wire `llm` into the daemon for `Freeform`.** *~3-4 days. High value.*
`EditIntent::Freeform` currently dead-ends. `crates/llm` has the transform, the
prompt builder, the output guardrail, and the sanitizer. Connecting it turns
"tighten this up", "make it more formal", "fix the grammar", and "make it
shorter" from refusals into working commands. Be honest in the docs that
quality is below Hexavoice's; the guardrail already refuses bad generations rather
than shipping them.

**4. Wire `stream::undo` and add the spoken undo verbs.** *~2 days. High value.*
Add to `edit-intent`: `"undo that"`, `"undo"`, `"go back one step"`,
`"go back"`, `"go back to the original"`, `"revert that"`, `"scratch that"`.
Route them into the existing `UndoUnit` ring, whose staleness check is already
correct. Hexavoice documents exactly three of these phrasings; matching them costs
almost nothing and closes a named competitive feature.

### Tier 2 — cheap parser wins

**5. Command-word-free replacement.** *~2-3 days. Very high value, our biggest
Edit Mode gap.*
When a `Freeform` utterance is *similar enough* to the selection (token-level
edit distance below a threshold, comparable length), treat it as a wholesale
replacement rather than an instruction. `crates/config/src/fuzzy.rs` already
has the similarity machinery. This is the behaviour Hexavoice promotes hardest, it
is fully deterministic, and it needs no model. Guard it with the same
threshold discipline `vocab.rs` uses (`FUZZY_THRESHOLD = 0.63`), and add it to
the edit-accuracy eval corpus the roadmap already schedules.

**6. Selection guards.** *~1 day. Medium value, prevents embarrassment.*
Cap edits at 6,000 characters (Hexavoice's exact number) and fall back to dictation.
Detect address bars and search fields and always dictate there. Both are
one-line product decisions Hexavoice learned the hard way and published; taking them
for free is strictly rational.

**7. Retain the last transcript.** *~0.5 day. Medium value.*
`Copy Last Transcript` in the menu, and a bindable `Paste Last Transcript`
hotkey later. Hexavoice shipped this as a named feature and users asked for it to
be rebindable, which tells you it gets used.

**8. Spelled-letter correction.** *~1 day. Medium value.*
`"It's T-O-N-I"` / `"that's spelled J-A-N-E"` → collapse the letter sequence and
replace the fuzzy-nearest token in the selection. Deterministic, no model,
and it fixes the single most common dictation failure: names.

### Tier 3 — new surfaces

**9. Onboarding wizard.** *~5 days. High value, and we win on it.*
`docs/ux/01-onboarding.md` already specifies it in full. Hexavoice's permission
story is weak enough that they shipped "two critical accessibility permission
bugs" in one release and their support answer is to email logs. A probe-based
checklist that verifies rather than trusts is a genuine advantage, and it is
the difference between "a stranger can install this" and "only the author can".

**10. Local history.** *~4 days. Medium-high value.*
Transcripts plus audio on disk, with replay, re-transcribe, and copy. Hexavoice
retains audio for 3 days; ours should default to the same and be a plain
folder the user can delete. The highest-value part is not the list, it is the
**"+ dictionary" affordance at the point of failure** that docs/ux/05 already
designs: the best dictionary entry is the word we just got wrong.

**11. Language plumbing.** *~2-3 days. Medium value.*
Pass `language` through to the recognizer, add a menu-bar language picker.
Apple's `SpeechTranscriber` supports many locales already, so this is wiring,
not modelling. Auto-detect is a later, harder step and should not block the
manual picker.

**12. Multiple hotkeys and a chord recorder.** *~4 days. Medium value.*
Hexavoice allows 5 bindings per action and had to add "Add Alternative" because
users switch between laptop and external keyboards. Our `conflict.rs` probe is
already better than their advice; a recorder plus an alternates list closes
the rest.

### Tier 4 — later, or deliberately declined

**13. File tagging.** *~4 days. Low-medium value.* `"main dot t s"` → `@main.ts`
is a pure text transform and needs no model, but it only pays off inside
Cursor/Windsurf and depends on per-app profile detection landing first.

**14. Send It.** *Declined in the general case; already designed for terminals.*
`docs/ux/04-terminal-and-headless.md` already specifies "run it / send it" for
terminals, off by default and opt-in, on the correct reasoning that terminals
execute things. Hexavoice's version relies on an LLM deciding whether the user
*meant* the command, which a suffix match cannot replicate safely: "tell them
I'll send it tomorrow" would fire. Ship the opt-in terminal version as
designed; do not ship a global suffix match.

**15. Custom Instructions.** *Declined as free text.* The local approximation
is the structured settings we already have (casing, punctuation, quotes,
per-app profiles) plus, once `llm` is wired, an optional plain-text system
prompt file for freeform edits only. Promising ChatGPT-style style compliance
from a 1.7B model would be a lie.

**16. Deep Context.** *Reframe, don't copy.* We already read the focused field
through the Accessibility API. Feeding that surrounding text into the
recognizer's bias list is a local Deep Context, better on privacy by
construction, and it reuses `vocab.rs`'s bias path. Worth doing after item 1.

### Top five, if only five things get done

1. Wire `vocab` into the pipeline (Dictionary + Replacements, uncapped).
2. Command-word-free replacement in `edit-intent` (Hexavoice's headline Edit Mode
   behaviour, fully local).
3. Wire `llm` for `Freeform` (turns four documented commands from refusals into
   features).
4. Spoken undo verbs into the existing `stream::undo` ring.
5. Wire `profile::select` plus the three formatting flags (delivers per-app
   behaviour and Casual Messaging together).

Every one of those is connecting code that already exists or adding string
patterns to a parser that already has the shape. None needs a new crate.

---

## Appendix: phrasings to add to `crates/edit-intent`

Collected verbatim or near-verbatim from Hexavoice's documentation and from the
phrasings their users are trained on. Ordered by expected frequency.

```
# undo (Hexavoice documents the first three exactly)
undo that
go back one step
go back to the original
undo
go back
revert that
scratch that

# replacement, already covered
change X to Y            replace X with Y
make X into Y            swap X for Y

# deletion, already covered
delete X   remove X   get rid of X   scratch X
delete that                          # bare form, not yet handled

# recasing, already covered (Hexavoice documents none of these)
all caps   uppercase   lowercase   title case   sentence case

# spelled correction, missing
it's T-O-N-I
that's spelled J-A-N-E

# freeform, route to llm once wired
fix the grammar          make it shorter
abbreviate               make it more formal
tighten this up          make it more casual

# freeform we cannot do locally, refuse by name
translate to <language>
```

Two design notes for whoever implements these. First, keep the literal-match
philosophy in the module doc: `parse` should stay a recognizer, not a
generator, and anything ambiguous should still fall to `Freeform` so the user
gets told rather than surprised. Second, add every phrasing above to the
edit-accuracy eval corpus that `docs/planning/00-roadmap.md` schedules for
weeks 6-8, because a command grammar without a regression corpus decays the
moment a second person edits it.
