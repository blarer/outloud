# Wiring the 12 inert config settings

Investigation, 2026-07-29. No code shipped by this document; the one
proof-of-concept written for it (the `formatting.*` transforms) lives outside
the repository and is reproduced inline below.

## What "wired" means here

`crates/config/src/schema.rs:161` carries a per-key `wired: bool`, and two
surfaces consume it:

- `crates/outloud/src/menubar.rs:659` (`only_implemented_settings_are_offered`)
  refuses to build a menu row for an unwired key.
- `crates/config/src/layers.rs:258` (`inert_settings`) reports keys the user
  *set* but nothing reads; `crates/outloud/src/menuhost.rs:136` prints them to
  stderr and to the menu.

Five keys are wired today: `hotkey`, `enabled`, `insertion.mode`,
`microphone.sensitivity`, `overlay.position`. Twelve are not.

### The pattern the sensitivity commits established

Read `e08bfed` and `d495b47`. The full path a setting travels:

1. schema row, `wired: true` (`crates/config/src/schema.rs:260`)
2. `menubar::Settings` field + `from_config` read
   (`crates/outloud/src/menubar.rs:106,189`)
3. a `MenuHost` accessor (`crates/outloud/src/menuhost.rs:82`)
4. `pipeline::Config` field (`crates/outloud/src/pipeline.rs:48`) populated in
   `main` (`crates/outloud/src/main.rs:286`) with flag > file > default
   precedence
5. the code that consumes it (`pipeline.rs:715 new_segmenter`)
6. the `wired` list in `schema.rs:408` updated in the same commit
7. a menu row where it helps, plus a test that the offered values pass schema
   validation (`menubar.rs:916`)
8. a *measurement*, not a taste, pinning any boundary
   (`crates/audio/tests/noise_floor.rs`)

Anything below that bar produces a key that writes to disk and changes nothing,
which is the exact failure the `wired` flag exists to prevent.

### A structural finding that predates all twelve

`Config::get_for(key, Some(app))` is the only entry point that applies the
per-app **profile** layer (`crates/config/src/layers.rs:204`). Grepping the
daemon:

```
$ grep -rn "get_for\|AppIdentity" --include=*.rs crates/outloud crates/diag tests
(no matches)
```

`menubar::Settings::from_config` calls `cfg.get(key)`
(`crates/outloud/src/menubar.rs:168`), which is `get_for(key, None)`. So every
`[profile.slack]` block in a user's file, and every profile example in
`docs/configuration.md:100-118`, is inert regardless of which keys get wired.
Worse, wiring `formatting.*` without also wiring profile resolution ships
exactly half of the documented feature: the global value works, the per-app
override silently does not.

This is one shared piece of work (~half a day) and it is a prerequisite for
`formatting.*`, `vocabulary.sets`, `insertion.paste-fallback` and `enabled`
being per-app, i.e. for most of the value in the list below. Call it **W0**.

**W0 sketch.** The daemon already reads the frontmost app at key-down
(`inject::snapshot_and_mode_at_keydown`, `crates/outloud/src/pipeline.rs:282`,
whose `TextSnapshot.app` is populated by `ax_edit::macos.rs:533`). That is the
correct moment: profile selection must use the app the user was looking at when
they spoke, not the one focused when the transcript lands. The snapshot gives an
app *title*, not a bundle id, so `AppIdentity.bundle_id` cannot be filled from
it today; `ax-edit` needs an `AXProcessIdentifier` → bundle-id lookup, or
profiles match on `process-name` only until then. That gap should be named in
the commit rather than papered over: matching a bundle-id pattern against a
window title would silently mismatch.

---

## Per-key analysis

Effort is in engineer-days for someone already fluent in this codebase,
including tests and docs, following the commit pattern above.

### 1. `formatting.smart-quotes` — 0.5d

**What it needs.** A pure `format_transcript(&str, &FormatOptions) -> String`
applied to the final transcript before delivery. There is exactly one seam:
`crates/outloud/src/pipeline.rs:485` (`let text = t.text.trim().to_string();`),
before the branch into the streamed and buffered paths. Add
`pipeline::Config.formatting: FormatOptions`, populated in
`crates/outloud/src/main.rs:272`.

New module `crates/outloud/src/format.rs`, or better `crates/config/src/format.rs`
next to `vocab.rs`, which already owns transcript post-processing and is pure.

**Caveat the streaming path creates.** `insertion.mode = "stream"` writes
partials into the field as they prove out (`pipeline.rs:468`). Formatting only
the final means the streamed prefix and the settled text disagree, and the
`Streamer`'s final pass would then rewrite characters the user watched appear.
Either format partials too (the transforms are idempotent, so this is fine) or
document that formatting applies on commit. Test it: `stream` + `smart-quotes`
must not produce a double correction.

**Verified by.** Unit tests on the transform, plus one pipeline test asserting
the delivered payload differs from the raw transcript. The PoC below passes.

### 2. `formatting.trailing-punctuation` — 0.25d (rides with #1)

Same seam, same struct. Strip trailing `.`/`!`/`?` when false. Note the
existing precedent at `crates/outloud/src/inject.rs:179`, which already strips
trailing punctuation from *edit commands*; that is a different rule (imperative
stripping) and must not be conflated with this one.

### 3. `formatting.casing` — 0.5d (rides with #1)

`casual-lowercase` lowercases sentence-initial capitals. Two traps found while
prototyping: the pronoun "I" and acronyms ("NASA") must survive, and
`vocab.rs`'s `[case]` flag (`crates/config/src/vocab.rs:31`) exists precisely to
preserve casing — so casing must run **before** vocabulary correction, or it
undoes it. Ordering is a correctness decision, not a preference, and needs a
test naming it.

**All three together are ~1 day** and are the highest value-per-effort in the
list, conditional on W0 for the per-app half.

### 4. `launch-at-login` — 0.5d

**What it needs.** Write/remove `~/Library/LaunchAgents/com.outloud.daemon.plist`
with `RunAtLoad`, then `launchctl bootstrap gui/$UID` / `bootout`. No existing
code; new module, say `crates/outloud/src/autostart.rs`. `menuhost::handle`
(`crates/outloud/src/menuhost.rs:200`) gains a side effect on this key, which is
a first for the `Action::Set` path — every other set is pure file I/O. Worth
stating explicitly rather than sneaking a `launchctl` call into a config write.

**Trap.** The plist must point at the *bundle*, not at a `target/debug` binary,
or a developer enables it once and their machine tries to launch a deleted path
at every login forever. Refuse to write the agent when
`std::env::current_exe()` is not inside an `.app`, and say why.

**Verified by.** Unit-test the plist generation as a pure function of an exe
path. The `launchctl` call itself is manual QA: log out, log in, daemon runs.

### 5. `microphone` (device selection) — 1d

**What it needs.** `crates/audio/src/capture_cpal.rs:139` hardcodes
`host.default_input_device()`. Enumeration already exists (`input_devices()`,
`capture_cpal.rs:38`), so the work is threading a `preferred: Option<String>`
through `start_capture` → `supervisor_loop` → `build_stream`, and through
`source::spawn_mic` (`crates/outloud/src/source.rs:116`) and `mic::Mic::open`
(`crates/outloud/src/mic.rs:92`).

**The hard half is not selection, it is absence.** The supervisor's rebuild loop
(`capture_cpal.rs:178`) currently treats "default device changed" as a signal to
rebuild. With a pinned device, that logic inverts: a named device disappearing
(AirPods out of the case) must *not* silently fall back to the built-in
microphone, because that is precisely the "recording from the wrong device" state
the menu bar exists to expose (`menubar.rs:296`). It must either wait for the
device to return or surface a named error. Getting this wrong makes the setting
actively worse than not having it.

**Verified by.** `input_devices()` round-trip test; manual test unplugging the
named device mid-session and asserting the menu says so.

### 6. `insertion.paste-fallback` — 0.5d

**What it needs.** A `bool` in `pipeline::Config`, threaded into
`inject::deliver` (`crates/outloud/src/inject.rs:134`), forcing the
`deliver_without_ax` clipboard branch (`inject.rs:717`) and skipping the AX and
typing tiers. Mechanically small.

**Trap.** `deliver_without_ax` tries typing *first* and the clipboard second,
deliberately (`inject.rs:691-715`, keystrokes leave the user's clipboard alone).
"Paste fallback" as named means clipboard, so honouring it inverts a deliberate
order. And the edit path must keep its rule that an insert-only tier may never
serve an edit (`inject.rs:262`).

**Value note.** This is the documented workaround for broken-accessibility apps,
so it is the setting a struggling user reaches for first — which makes it a
worse-than-average key to leave inert.

### 7. `vocabulary.sets` — 1d

**What it needs.** Everything hard is already built and tested:
`crates/config/src/vocab.rs` parses sets, merges them, and post-corrects with
both exact and fuzzy engines (573 lines, fully covered). Missing is only the
loading and the call: read `config::vocabulary_dir()/<name>.txt` for each named
set, `Vocabulary::merge`, and call `.correct(&text)` at the same seam as
`formatting.*` (`pipeline.rs:485`).

**Ordering matters.** Vocabulary correction must run *after* casing (see #3) and
its `[case]` flag must win. One test, stated as a rule.

**Second, larger half, deliberately out of scope for wiring the key.**
`Vocabulary::bias_terms()` (`vocab.rs:140`) exists to bias the *recognizer*, and
nothing consumes it. The Apple helper takes no bias list today
(`crates/asr/helper/transcriber.swift:70`); `SpeechTranscriber` supports
contextual strings, so this is real but separate work. Wiring the key should
deliver post-correction only, and say so.

**Also worth flagging:** the file watcher (`menuhost.rs:161`) watches
`config.toml` only. Editing a vocabulary file would not hot-reload, which
contradicts "edits apply live".

### 8. `silence-timeout-ms` — 0.5d, but the feature behind it is missing

**What it needs.** The segmenter's hangover is
`SegmenterConfig.hangover_frames`, 10 frames of 30ms
(`crates/audio/src/segment.rs:54`). Converting ms → frames and threading it
through `new_segmenter` (`pipeline.rs:715`) is trivial.

**Why it is not actually trivial.** The doc string says "in latch mode". Latch
exists in the hotkey layer (`crates/hotkey/src/taphold.rs`, `HotkeyEvent::Latched`)
but the daemon drops it on the floor: `crates/outloud/src/source.rs:81` maps
`Latched => None`, and the pipeline only auto-commits on a VAD endpoint when
`cfg.auto_endpoint` is set, which is `--once` only (`main.rs:276`). So a latched
capture today never ends by silence at all. Wiring this key against the segmenter
hangover would change *push-to-talk* endpointing, which is not what the key
documents, and would silently retune the sensitivity work that was just
measured.

The honest options are (a) wire latch-mode auto-commit first and then this key
means what it says, or (b) leave it and stop advertising latch. Do not wire the
number alone.

### 9. `history.enabled` — 0.5d to implement, but see §"Dangerous"

Append `<timestamp>\t<transcript>` to `~/.config/outloud/history.txt` at
`pipeline.rs:485`. Mechanically the smallest item in this list.

### 10. `language` — 1.5d

**What it needs.** The Apple helper already reads a locale from the
environment: `crates/asr/helper/transcriber.swift:50` uses
`AQUA_ASR_LOCALE`, defaulting to `en_US`. Nothing sets it. The spawn site is
`crates/asr/src/backends/apple.rs:107`; adding `.env("AQUA_ASR_LOCALE", locale)`
is a two-line change, and the factory
(`crates/outloud/src/main.rs:118 make_recognizer_factory`) already takes a
config-derived argument (`sensitivity`), so the shape exists.

**Why it is still 1.5d.** `"auto"` cannot be honoured — `SpeechTranscriber`
wants a concrete locale and `supportedLocale(equivalentTo:)` will `fail()` the
helper on an unsupported one (`transcriber.swift:52`), which the daemon surfaces
as "recognizer failed to load". So this needs a mapping from a bare code (`"en"`)
to a locale (`"en_US"`), an `auto` that means "the system locale", and a
failure path that names the unsupported language instead of looking like a
broken install. The schema constraint is `None` today; it should probably become
a validated set or at least a validated shape.

Also: the schema key is `language` while the env var is `AQUA_ASR_LOCALE`. Rename
the helper's variable to `OUTLOUD_ASR_LOCALE` (honouring the old one) while
touching it, consistent with `layers.rs:20`.

### 11. `model` — 3d+, and it is dishonest at any smaller size

`fast`/`balanced`/`accurate` implies a choice among recognizers. The daemon has
one working backend (`apple`), plus `mock`, plus two documented stubs that fail
loudly at finalize (`crates/asr/src/backends/parakeet.rs`,
`whisper_cpp.rs`). Apple's `SpeechTranscriber` exposes no size dial: there is
nothing for the three values to select between.

Real wiring means landing whisper.cpp or Parakeet — a native build dependency
(cmake / ONNX Runtime), model download and SHA pinning (`crates/asr/src/models.rs`
has the registry and a `fetch` already), CI coverage, and a licence surface
(Parakeet weights are CC-BY-4.0, not MIT). That is a project, not a setting.

**Interim option that is honest:** narrow the key to the backends that exist and
rename it, e.g. `recognizer = "apple" | "mock"`, matching the existing `--asr`
flag (`main.rs:69`). That is 0.5d and removes a lie without pretending to a
quality dial.

### 12. `telemetry.enabled` — do not wire. Remove.

See below.

---

## Dangerous to wire naively

### `telemetry.enabled` — remove from the schema

The product's whole claim is that audio and text never leave the machine.
Wiring this key means, concretely: adding an HTTP client to the dependency tree,
adding outbound network code to a daemon that currently makes zero network calls
at runtime, and adding an endpoint someone must operate. Every one of those is
irreversible in the way that matters — a reviewer can no longer verify "no
network" by observing that there is no network code, only by auditing a flag.

The key is also *already* misleading in the direction nobody wants: it
advertises a capability the project says it will never build. `docs/pre-release-audit.md:385`
reached the same conclusion, and `docs/planning/01-backlog.md:130` (I-06)
commits to "verifiable by network inspection", which a shipped-but-disabled
telemetry client would defeat by construction.

**Recommendation: delete the row.** That is a schema change, so bump
`SCHEMA_VERSION` to 2 and add a `migrate.rs` step that drops the key with a
comment rather than erroring on it, so an existing file with
`telemetry.enabled = false` still loads. The privacy claim then lives in
`README` and `docs/`, where it is prose, instead of in a schema where it reads
as a switch waiting to be flipped.

If the project ever does want an opt-in version ping, that is a new key with a
new name, introduced alongside the code that implements it and the documented
network test that proves it is off. Not this one, silently activated later.

### `history.enabled` — dangerous as a *default*, not as a feature

Default `true`, doc string "Keep a local plain-text transcription history"
(`schema.rs:253`). Today it writes nothing, so the file lies in the safe
direction. The moment it is wired at its current default, every user who
upgrades starts having every dictated sentence — passwords spoken by mistake,
medical notes, private messages — appended to a plaintext file they never asked
for and were never told about, because the setting was already "on" in a file
they had read and dismissed.

**Do not wire this key at `default: true`.** Either flip the default to `false`
in the same commit that implements it, or implement it opt-in with a first-write
notice naming the path. Also decide retention (unbounded plaintext growth is its
own problem) and make deletion reachable from the menu. That turns a 0.5d task
into ~1.5d, and the extra day is the point.

### `microphone` — dangerous through its failure mode, not its success

Covered in §5: a pinned device that vanishes must not silently become the
built-in mic. Wired naively (pin the name, fall back on failure) this makes the
"recording from the wrong device" state *more* likely while appearing to fix it.

### `insertion.paste-fallback` — clipboard clobbering

`deliver_without_ax` restores the user's clipboard 300ms later on a detached
thread (`inject.rs:756`). Forcing every utterance through that path makes a
race that is currently rare into one that happens on every single dictation.
Worth a deliberate look before defaulting anyone into it.

---

## Ranked by value / effort

| # | Key(s) | Effort | Value | Notes |
|---|---|---|---|---|
| 0 | **W0: profile layer** (`get_for`) | 0.5d | High | Prerequisite for most rows below being per-app; profiles are documented and 100% inert today |
| 1 | `formatting.smart-quotes`, `.trailing-punctuation`, `.casing` | 1d for all three | High | Pure transforms, one seam, no new dependency. PoC below |
| 2 | `launch-at-login` | 0.5d | Medium-high | Self-contained; guard against non-bundle paths |
| 3 | `vocabulary.sets` (post-correction only) | 1d | High | The engine is already written and tested; only loading + one call missing |
| 4 | `insertion.paste-fallback` | 0.5d | Medium | The workaround a struggling user reaches for first |
| 5 | `microphone` | 1d | Medium-high | Value is high, but the absent-device path is where the work is |
| 6 | `language` | 1.5d | Medium (high for non-English users) | Helper already reads a locale env var nothing sets |
| 7 | `history.enabled` | 1.5d | Medium | 0.5d of code, 1d of doing it safely. Must not ship at `default: true` |
| 8 | `silence-timeout-ms` | 0.5d + latch work | Low until latch exists | Wiring the number alone would change push-to-talk, not latch |
| 9 | `model` | 3d+ | Low as specified | Or 0.5d to narrow it honestly to `apple`/`mock` |
| 10 | `telemetry.enabled` | — | — | **Remove.** Schema v2 + migration |

## Proposed order

1. **W0, profile resolution.** Everything after this is either per-app or
   knowingly global. Doing it later means revisiting each key.
   *Verified by:* a pipeline test where a `[profile.x]` override changes the
   delivered payload for a matching app and not for another.
2. **`formatting.*` (all three).** One commit, one seam, one `FormatOptions`.
   *Verified by:* unit tests per transform; a test pinning casing-before-vocab
   ordering; a streaming test proving no double correction.
3. **`telemetry.enabled` removal.** Do it early and cheaply, while the schema
   version bump is the only migration in flight.
   *Verified by:* an existing config containing the key still loads with a
   warning, not an error; `unwired_keys()` shrinks by one.
4. **`vocabulary.sets`.** Slots into the seam step 2 just built, and the
   correction engine is already covered.
   *Verified by:* end-to-end `--once --wav` with a vocabulary file, asserting
   the mangled term is corrected; a test that `[case]` survives casing.
5. **`launch-at-login`.** Independent, visible, finishes a menu row.
   *Verified by:* pure plist-generation test + one manual logout/login.
6. **`insertion.paste-fallback`.** Small, and unblocks users on broken-AX apps.
   *Verified by:* a `deliver` test asserting the clipboard tier is chosen and
   the typing tier skipped; edits still refuse insert-only tiers.
7. **`microphone`.** Budget most of the day for the disappearing-device path.
   *Verified by:* manual unplug test asserting the menu names the problem
   rather than silently switching devices.
8. **`language`.** Needs the locale-mapping and failure-path design first.
   *Verified by:* `--once --say` in a second language returning that language's
   text; an unsupported code producing a named error, not a load failure.
9. **`history.enabled`,** default flipped to `false`, with retention and a
   delete path.
   *Verified by:* a test that the default writes nothing; a manual check that
   the menu can delete the file.
10. **`silence-timeout-ms`,** only after latch-mode auto-commit exists.
11. **`model`,** either narrowed honestly now or deferred behind a real second
    backend.

After steps 1-9, `unwired_keys()` holds two entries (`silence-timeout-ms`,
`model`) instead of twelve, and both have a stated reason rather than a
placeholder.

---

## Appendix: the `formatting.*` proof-of-concept

Written and tested outside the repository to size the estimate; five tests pass.
Reproduced so the estimate is checkable.

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct FormatOptions {
    pub casual_lowercase: bool,
    pub smart_quotes: bool,
    pub trailing_punctuation: bool,
}

pub fn format_transcript(text: &str, o: &FormatOptions) -> String {
    let mut s = text.trim().to_string();
    if o.casual_lowercase {
        s = lowercase_sentence_starts(&s);
    }
    if o.smart_quotes {
        s = smart_quotes(&s);
    }
    if !o.trailing_punctuation {
        s = s.trim_end_matches(['.', '!', '?']).trim_end().to_string();
    }
    s
}

/// Lowercase only what the recognizer capitalized as a sentence start.
/// "I" and acronyms are left alone: lowercasing either is a visible error,
/// and the vocabulary crate's `[case]` flag exists to protect the second
/// class, so this must not fight it.
fn lowercase_sentence_starts(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at_start = true;
    for word in s.split_inclusive(char::is_whitespace) {
        let trimmed = word.trim_end();
        let is_acronym = trimmed.chars().filter(|c| c.is_alphabetic()).count() > 1
            && trimmed.chars().filter(|c| c.is_alphabetic()).all(|c| c.is_uppercase());
        if at_start && !is_acronym && trimmed != "I" {
            let mut ch = word.chars();
            if let Some(first) = ch.next() {
                out.extend(first.to_lowercase());
                out.push_str(ch.as_str());
            }
        } else {
            out.push_str(word);
        }
        at_start = trimmed.ends_with(['.', '!', '?']);
    }
    out
}

/// Straight quotes to typographic ones, direction taken from the preceding
/// character the way a word processor does it.
fn smart_quotes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev: Option<char> = None;
    for c in s.chars() {
        let opening = prev.is_none_or(|p| p.is_whitespace() || "([{".contains(p));
        match c {
            '"' => out.push(if opening { '\u{201C}' } else { '\u{201D}' }),
            '\'' => out.push(if opening { '\u{2018}' } else { '\u{2019}' }),
            other => out.push(other),
        }
        prev = Some(c);
    }
    out
}
```

Cases the tests pin: quote direction (`he said "hi"` → curly pair),
apostrophe-as-right-single-quote (`it's` → `it\u{2019}s`), casual-lowercase
preserving `I` and `NASA`, trailing-punctuation stripping only the final mark
(`cd src. ls.` → `cd src. ls`), and identity on plain text at defaults.

The last one matters most: the defaults are `smart-quotes = true` and
`trailing-punctuation = true`, so wiring these keys changes the behaviour of
every existing installation on the first launch after the upgrade. Curly quotes
arriving in a terminal or a code editor is a regression, which is a further
argument for landing W0 first so a terminal profile can turn them off — exactly
as `docs/configuration.md:100-118` already promises it can.
