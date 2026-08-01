# Configuration

Everything the daemon does is controlled by one human-readable TOML file plus
a folder of plain-text vocabulary files. No binary blobs, no plists, no
database: your settings are yours, in files you can read, edit, version, and
sync. The GUI settings window and `outloud set` are both convenience views over
these same files, and external edits hot-reload without a restart.

Implemented in `crates/config`. The layering, profile matching, vocabulary
correction, validation, and migration logic are all pure functions with full
unit coverage; only file watching and the actual reads/writes touch the OS.

## Files

Everything lives in one directory: `$XDG_CONFIG_HOME/outloud/`, or
`~/.config/outloud/` when that variable is unset (on macOS too, deliberately:
this is a file you are meant to read, edit, diff and sync like any dotfile).
The daemon writes a fully-commented `config.toml` there on first launch, and
the menu bar's **Edit config file…** opens exactly that file.

| File | Purpose |
|---|---|
| `config.toml` | All settings and per-app profiles |
| `config.toml.broken` | Kept automatically if the file fails to parse; defaults load instead |
| `config.toml.v<N>` | Automatic backup made before a schema migration |
| `vocabulary/*.txt` | Vocabulary sets, one entry per line (see below) |

A machine-wide `/etc/outloud/config.toml` is also read, for managed
deployments; the daemon never writes to it.

The macOS menu bar item's **Settings** submenu is a view over this same
file: choosing a value writes it here, preserving your comments, and editing
the file by hand is picked up by the menu within a second.

The menu deliberately surfaces only the settings that are implemented today
(`hotkey`, `enabled`, `microphone.sensitivity`, `microphone.warm-hold-ms`,
`silence-timeout-ms`, and hiding the overlay). The rest of the table below is
schema and documentation ahead of the code: the keys validate, migrate, and
report provenance, but the pipeline does not read them yet. They are listed
here rather than offered as menu rows because a settings control that writes
a key nothing consumes is worse than no control at all.

## Layering

A value can be set in several places. Higher layers win:

1. **Built-in defaults** (lowest)
2. **System file** (e.g. `/etc/outloud/config.toml`, for managed machines)
3. **User file** (your `config.toml`)
4. **Matched per-app profile** (from either file; see Profiles)
5. **`OUTLOUD_*` environment variables** (highest)

Every resolved value knows which layer set it, and `outloud status --json`
reports it, so "why is my hotkey not what I set" is always answerable:

```
hotkey = "f13"        from environment variable OUTLOUD_HOTKEY
                      (shadowing: user file ~/.config/outloud/config.toml = "right-option";
                       built-in default = "right-option")
```

Environment variables spell the key with `OUTLOUD_` plus the key upper-cased and
`.`/`-` replaced by `_`: `insertion.mode` → `OUTLOUD_INSERTION_MODE`,
`silence-timeout-ms` → `OUTLOUD_SILENCE_TIMEOUT_MS`. Booleans accept
`true/false/1/0/yes/no/on/off`; lists are comma-separated. A mistyped or
invalid environment override is reported and skipped, never silently ignored.

### `OUTLOUD_NO_INJECT`

Not a setting; a testing guard. With `OUTLOUD_NO_INJECT=1` the daemon runs the
whole pipeline and reports the transcript, but writes nothing to any app.

Use it for anything that replays audio, because `--once --wav` delivers into
whatever window is focused. Benchmarking against a recording while someone is
working will otherwise type the test sentence into their chat window, which is
exactly how this guard came to exist. `scripts/sweep-sensitivity.sh` sets it.

## Options

Both spellings work: dotted keys (`insertion.mode = "stream"`) or tables
(`[insertion]` then `mode = "stream"`).

| Key | Default | Effect |
|---|---|---|
| `hotkey` | `"right-option"` | Push-to-talk key. Hold to dictate, tap to latch. |
| `microphone` | `"auto"` | Input device name, or `"auto"` to follow the system default. |
| `language` | `"auto"` | Recognition language code (e.g. `"en"`), or `"auto"` to detect. |
| `model` | `"balanced"` | Recognizer trade-off: `fast`, `balanced`, or `accurate`. |
| `enabled` | `true` | Master switch. Profiles set this `false` to mute the tool in an app. |
| `insertion.mode` | `"on-release"` | `on-release` inserts the whole utterance at once; `stream` types words as you speak. |
| `insertion.paste-fallback` | `false` | Force clipboard-paste insertion for apps with broken accessibility. **Not implemented yet:** parsed and validated, but nothing reads it. |
| `formatting.casing` | `"standard"` | `standard` or `casual-lowercase` (chat apps often read better lowercase). **Not implemented yet:** parsed and validated, but nothing reads it. Verified by running the binary with it set. |
| `formatting.smart-quotes` | `true` | Convert straight quotes to typographic quotes. **Not implemented yet:** parsed and validated, but nothing reads it. Verified by running the binary with it set. |
| `formatting.trailing-punctuation` | `true` | End utterances with inferred punctuation. **Not implemented yet:** parsed and validated, but nothing reads it. Verified by running the binary with it set. |
| `history.enabled` | `true` | Keep a local plain-text transcription history. **Not implemented yet:** there is no transcript history to enable. |
| `microphone.sensitivity` | `50` | How quiet a voice still counts as speech (1–100). Raise it if you sit back from the mic; lower it if room noise is transcribed. |
| `microphone.warm-hold-ms` | `0` | Keep a *slow* microphone open this long after you stop speaking, so the next utterance is not clipped (0–10000). Off by default. |
| `silence-timeout-ms` | `60000` | Safety net: force-commit and close the microphone after capture has run this long (1000–600000). |
| `overlay.position` | `"bottom-center"` | `bottom-center`, `bottom-left`, `bottom-right`, `top-center`, or `hidden`. |
| `vocabulary.sets` | `[]` | Named vocabulary sets active by default; profiles override per app. |
| `telemetry.enabled` | `false` | Anonymous usage reporting. Off by default, forever. Nothing to disable: the binary links no networking framework, so no telemetry exists to send. |
| `launch-at-login` | `false` | Start the daemon when you log in. **Not implemented yet:** parsed and shown, but no login item is installed. |
| `schema-version` | `1` | Written by the daemon; used for automatic migration. |

### Bluetooth headsets and the first word

A Bluetooth headset takes far longer to start capturing than a built-in
microphone: measured on this machine, AirPods deliver their first sample
187–210ms after the stream opens, against a 150ms pre-roll. The daemon warns
you once per device when it sees this.

The audio in that gap is not delayed, it is **never captured**, so no buffer
can recover it. Widening the pre-roll does nothing. The first word is not
dropped either, it is *misrecognised*, which reads as "this tool mishears me"
rather than "my headset is slow".

`microphone.warm-hold-ms` keeps the stream open for a moment after you stop
speaking, so the device is already warm when you press the key again:

```toml
microphone.warm-hold-ms = 2000
```

This is off by default, and deliberately narrow when on. It applies **only** to
devices this machine has actually measured as slow, never to a built-in
microphone. It is capped at 10 seconds, and the hold always expires on its own.

The trade is explicit: for that window the system recording indicator stays lit
while you are not dictating. OutLoud's default is that the orange dot means
"dictating right now" and nothing else, which is why this is opt-in rather than
a silent optimisation.

### Microphone sensitivity

If dictation misses words unless you lean in and enunciate, raise this. It sets
how quiet a frame of audio can be and still count as speech; anything below the
threshold never reaches the recognizer at all.

The scale is 1–100 and geometric, so each step is a constant ratio rather than a
constant amount. `50` is the default and sits at roughly the 10th percentile of
measured speech (~0.0009 RMS).

That anchor is deliberately the *quiet tail* of speech rather than its median.
Half of all speech frames sit below the median by definition, and those quiet
frames are not noise: they are word endings, trailing syllables, and the start of
a sentence before the voice reaches full volume. A median anchor measured as
"70% of frames heard" and dropped `A quick` off the front of *A quick brown fox
jumps over the lazy dog*.

| Setting | Use when |
| --- | --- |
| `25` | Noisy room, or background speech is being transcribed |
| `40` | Slightly quieter room than default assumes |
| `50` | Default: normal seated distance |
| `70` | You sit back from the machine, or speak quietly |

Above roughly 75 the gate is low enough that room noise itself gets recognized
as words, which is why the menu bar stops at 70. That boundary moves whenever
the anchor moves, so `crates/audio/tests/noise_floor.rs` derives it from a
synthetic quiet room rather than hardcoding it. `scripts/sweep-sensitivity.sh
<wav>` re-derives that boundary against a recording, and `cargo run -p audio
--example mic_level` reports what your own microphone actually produces so the
setting can be chosen from a measurement.

Hotkey grammar (see `crates/hotkey`): a bare side-specific modifier
(`"right-option"`), bare `"fn"`, a function key (`"f13"`), or a classic chord
(`"cmd+shift+space"`). Modifier-only chords like `"cmd+shift"` are rejected
because there is no key-up to anchor push-to-talk release on, and the error
message says so.

## Per-application profiles

A profile is a matcher plus overrides. Any *wired* key from the table above can
be overridden per app; overriding an inert key does nothing, exactly as setting
it globally does nothing.

Profiles resolve against the app that was focused **when you pressed the key**,
not the one focused when the transcript lands. Those differ when a slow
utterance races a window switch, and applying one app's rules to another app's
text is the failure that ordering prevents.

To find an app's bundle id, focus it and run:

```
cargo run --release -p ax-edit --example whoami
```

```toml
[profile.terminal]
match.bundle-id = "com.apple.terminal"       # exact, case-insensitive
formatting.smart-quotes = false               # never curl quotes in a shell
formatting.trailing-punctuation = false       # a stray "." breaks commands
vocabulary.sets = ["code", "kubernetes"]

[profile.jetbrains]
match.bundle-id = "com.jetbrains.*"           # trailing * = prefix match
vocabulary.sets = ["code"]
insertion.mode = "on-release"                 # IDEs handle streaming badly

[profile.slack]
match.bundle-id = "com.tinyspeck.slackmacgap"
formatting.casing = "casual-lowercase"
vocabulary.sets = ["team-names"]

[profile.ssh-vim]
match.process-name = "vim"                    # exe name, for terminal programs
insertion.paste-fallback = true

[profile.games]
match.window-class = "steam_app_*"            # X11/Wayland only; inert on macOS
enabled = false                               # mute entirely
```

`match.window-class` is an X11/Wayland concept and never matches on macOS: the
daemon leaves it empty rather than substituting an app name, so a profile keyed
on it cannot fire against the wrong thing. Use `match.bundle-id` on macOS, or
`match.process-name` for a bare executable run from a shell (`nvim` over SSH),
which has no bundle id at all.

### Matching rules

Exactly one matcher per profile: `match.bundle-id`, `match.process-name`, or
`match.window-class`. Matching is case-insensitive; a trailing `*` makes the
pattern a prefix match (that is the entire wildcard grammar, on purpose).

When several profiles match the same app, one winner is chosen. Precedence,
in order:

1. **Matcher kind**: `bundle-id` beats `process-name` beats `window-class`
   (more stable identifiers win).
2. **Exact over prefix**: `com.jetbrains.clion` beats `com.jetbrains.*`.
3. **Longer prefix**: `com.jetbrains.intellij*` beats `com.jetbrains.*`.
4. **File order**: earlier profile wins a full tie.

Only the winner applies; profiles never stack. Same-kind profiles that can
overlap are reported at load time, naming the winner and the rule that
decided it, so surprises happen in the log rather than mid-dictation.

## Vocabulary

Plain UTF-8 text files in `vocabulary/`, one entry per line. **No entry
limit**: 5 entries or 50,000, same behavior. Files are git-able and
shareable, and external edits hot-reload.

```text
# code.txt — lines starting with # are comments

# Bias terms: a bare word tells the recognizer to prefer it, and enables
# fuzzy correction when it still gets mangled ("cube cuddle" -> kubectl).
kubectl
systemd
nginx
PostgreSQL

# Replacements: spoken form -> written form, matched on word boundaries.
dash dash force -> --force
open paren -> (

# Auto-expansions use the same arrow.
my address -> 12 Elm St, Springfield

# Flags in [brackets] at the end of a line:
github -> GitHub [case]          # keep this exact casing, even mid-sentence
kubectl -> kubectl [strip-punct] # drop punctuation the recognizer glued on
```

Correction runs in two passes over recognizer output:

1. **Exact replacement**: spoken forms are replaced case-insensitively on
   word boundaries (so `cat -> feline` never touches "concatenate"). Longer
   rules win over shorter ones.
2. **Fuzzy correction**: a word, or an adjacent word pair, that *sounds*
   close enough to a bias term (combined edit-distance and phonetic score) is
   rewritten to it. This is what turns "cube cuddle" into `kubectl` without
   an entry for every possible mangling. Words shorter than 5 characters and
   clean prefixes of a term ("system" vs `systemd`) are never fuzzy-matched,
   because false positives corrupt words you actually said.

Every applied correction is logged with the vocabulary line that fired, which
is what powers the post-dictation diff chip and its one-key "[+ dictionary]".

Sets are named after their file (`code.txt` → `"code"`) and activated by
`vocabulary.sets` globally or per profile. When two active sets define the
same spoken form, the later-listed set wins.

## Hot reload

The daemon watches `config.toml` and the vocabulary folder and applies
changes live. Changes are debounced (300ms of quiet) so editors that save in
a burst of writes trigger one reload, not five. Deleting the file is also a
change: the daemon falls back to defaults and says so.

If a save leaves the file unparsable, the daemon keeps running on the last
good configuration, copies the bad file to `config.toml.broken`, and shows
one tray notice naming the TOML error. It never refuses to start, and it
never discards your file.

## Validation

Nothing is silently ignored. Every problem is reported with the file it came
from and enough context to fix it:

- **Unknown key** → a did-you-mean suggestion when one is close
  (`unknown setting "hotkye"; did you mean "hotkey"?`), or a pointer to
  `outloud set --list` when nothing is.
- **Wrong type** → both sides named (`"hotkey" expects a string, got integer`).
- **Invalid value** → what would be valid (`"turbo" is not one of the allowed
  values: fast, balanced, accurate`; invalid hotkeys quote the chord grammar
  with examples).
- **Profile problems** → missing/duplicate matchers name the fix; overlapping
  profiles name the winner and why it wins.
- Valid keys still apply even when other lines have errors. One typo never
  takes down the whole file.

## Schema version and migration

`config.toml` carries `schema-version = 1`. A file without the key is treated
as current, so hand-written files never need it.

When a future release renames or reshapes a key, it bumps the version and
ships a migration step. On first load of an older file the daemon migrates it
in place (comments and layout preserved), writes the result, and keeps the
original as `config.toml.v<old>` so downgrading or auditing is always
possible. Files written by a *newer* version are read best-effort and never
rewritten. This path is decided and tested now, before anyone has a file,
precisely so it never has to change under one.
