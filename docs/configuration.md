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
(`hotkey`, `enabled`, and hiding the overlay). The rest of the table below is
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
| `insertion.paste-fallback` | `false` | Force clipboard-paste insertion for apps with broken accessibility. |
| `formatting.casing` | `"standard"` | `standard` or `casual-lowercase` (chat apps often read better lowercase). |
| `formatting.smart-quotes` | `true` | Convert straight quotes to typographic quotes. |
| `formatting.trailing-punctuation` | `true` | End utterances with inferred punctuation. |
| `history.enabled` | `true` | Keep a local plain-text transcription history. |
| `silence-timeout-ms` | `1500` | Stop listening after this much silence in latch mode (200–30000). |
| `overlay.position` | `"bottom-center"` | `bottom-center`, `bottom-left`, `bottom-right`, `top-center`, or `hidden`. |
| `vocabulary.sets` | `[]` | Named vocabulary sets active by default; profiles override per app. |
| `telemetry.enabled` | `false` | Anonymous usage reporting. Off by default, forever. |
| `launch-at-login` | `false` | Start the daemon when you log in. |
| `schema-version` | `1` | Written by the daemon; used for automatic migration. |

Hotkey grammar (see `crates/hotkey`): a bare side-specific modifier
(`"right-option"`), bare `"fn"`, a function key (`"f13"`), or a classic chord
(`"cmd+shift+space"`). Modifier-only chords like `"cmd+shift"` are rejected
because there is no key-up to anchor push-to-talk release on, and the error
message says so.

## Per-application profiles

A profile is a matcher plus overrides. Any key from the table above can be
overridden per app.

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
match.window-class = "steam_app_*"            # X11/Wayland window class
enabled = false                               # mute entirely
```

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
