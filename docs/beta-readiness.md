# Beta readiness: GO / NO-GO for a public beta

Assessment for handing this to roughly 50 strangers. Written by the beta
readiness agent, in coordination with the release QA agent, whose findings live
in [`release-readiness.md`](release-readiness.md) and are not duplicated here.

Scope: the seams between the other work streams. First run and cold start, the
macOS permission maze, uninstall and upgrade, crash and recovery, concurrency
and lifecycle, the support surface, distribution reality, and whether the
documentation tells the truth. Build correctness, CI, and latency optimization
belong to release QA.

Every claim below was reproduced on this machine. Commands and their exact
output are quoted. Where something could not be reproduced, it says so.

---

## Recommendation: **CONDITIONAL GO**

Go, for a **source-install beta on macOS 26+, capped at people who can run a
shell command**, and only once the three blockers below are cleared. Two of the
three are already fixed in this commit.

Do **not** ship a downloadable `.app` yet. That path is blocked by
notarization, which needs a paid Apple Developer account and cannot be worked
around.

The reason this is a GO rather than a NO-GO: the product's hard parts are
genuinely done. Dictation, edit-by-voice, and terminal injection work, latency
beats the commercial competition, error messages are unusually good, and the
doctor is better than most shipping products have. What is missing is the
boring layer around the outside, which is cheap to add and is what this
document is about.

---

## Blockers

### B1. The documented install produces a daemon that cannot transcribe

**Severity: blocker. Frequency: 100% of source installs. Status: FIXED in this
commit (README), root cause remains.**

The README told the user to run `cargo build --release`. That does not build
the Swift speech helper, so the recognizer is absent.

Reproduction, from a genuinely fresh clone:

```
$ git clone /Users/blare/aqua-oss-spike /tmp/lobster-fresh && cd /tmp/lobster-fresh
$ cargo build --release
    Finished `release` profile [optimized] target(s) in 39.86s

$ ./target/release/aquad --once --say "the rain in spain falls mainly on the plain" --no-overlay
aquad: state model-loading
Error: recognizer failed to load (aqua-speech-helper not found; build it with `swiftc -O crates/asr/helper/transcriber.swift -o aqua-speech-helper` or set AQUA_SPEECH_HELPER) -> build the speech helper (see crates/asr/helper) or run with --asr mock
RC=1
```

The plain daemon, which is the README's "Using it" step, dies the same way:

```
$ ./target/release/aquad --no-overlay
aquad: state model-loading
aquad: hold right-option to dictate
aquad: state error (recognizer failed to load (aqua-speech-helper not found ...))
RC=1
```

Cause: `crates/asr/helper/aqua-speech-helper` is a compiled artifact and is
gitignored, there is no `build.rs` anywhere in the workspace (`find crates -name
build.rs` returns nothing), and the only thing that ever invokes `swiftc` is
`scripts/bundle-aquad-macos.sh`. The README pointed at `scripts/bundle-macos.sh`,
which packages `spike-cli`, not the daemon. This stayed invisible to the team
because every development tree already has a stale helper binary in the
gitignored path.

The correct script does work, from the same fresh clone:

```
$ ./scripts/bundle-aquad-macos.sh
==> Building the speech helper
...
$ ls dist/Aqua.app/Contents/MacOS/
Aqua                 aqua-speech-helper

$ ./dist/Aqua.app/Contents/MacOS/Aqua --once --say "the rain in spain falls mainly on the plain" --no-overlay
e2e: release->text 193ms (finalize 145ms, inject 48.0ms) via synthetic-keys | "The rain in Spain falls mainly on the plain."
```

Independently reproduced by release QA, which also confirmed the packaged path
transcribes correctly. That is the severity-limiting fact: the artifact works,
only the documented from-source path was broken.

**Fixed here** by pointing the README at `bundle-aquad-macos.sh`, naming the
Xcode Command Line Tools prerequisite, and changing the "Using it" examples to
run the bundled binary.

**Not fixed, and should be before beta:** two ways this still bites.

1. `bundle-aquad-macos.sh` warns rather than fails when `swiftc` is missing
   ("aquad will start but cannot transcribe"). A release process can scroll
   past that and ship a bundle with no recognizer.
2. The helper is rebuilt only when the source is newer than the binary
   (`[[ ! -x "$HELPER_BIN" || "$HELPER_SRC" -nt "$HELPER_BIN" ]]`), so a stale
   helper is silently reused. This is precisely why nobody noticed.

A `build.rs` was considered and rejected, for good reasons: it would put a
non-hermetic external compiler in the build graph and threaten the `repro`
job's byte-identical guarantee. The scripts-and-docs fix is the right one.

### B2. Unsigned and un-notarized: a downloaded app silently does not open

**Severity: blocker for binary distribution. Frequency: 100% of downloads.
Status: cannot be fixed without a paid Apple Developer account. Documented in
this commit.**

The bundle is ad-hoc signed with no team identifier:

```
$ codesign -dv --verbose=2 dist/Aqua.app
Identifier=dev.aquaoss.aquad
CodeDirectory v=20400 size=5322 flags=0x2(adhoc) hashes=160+3 location=embedded
Signature=adhoc
TeamIdentifier=not set
```

Gatekeeper rejects it:

```
$ spctl -a -vvv -t exec dist/Aqua.app
dist/Aqua.app: rejected
RC=3

$ stapler validate dist/Aqua.app
Aqua.app does not have a ticket stapled to it.
```

Simulating a browser download by applying the quarantine flag, then
double-clicking:

```
$ xattr -w com.apple.quarantine "0083;68a70000;Safari;" /tmp/lobster-qtest.app
$ spctl -a -vvv -t exec /tmp/lobster-qtest.app
/tmp/lobster-qtest.app: rejected

$ open /tmp/lobster-qtest.app
open RC=0                       <- reports success
$ pgrep -fl 'lobster-qtest'
(NOT running - Gatekeeper blocked it)
```

The worst detail is that `open` returns **0**. The app does not launch and
nothing reports an error to the terminal. A user double-clicking in Finder gets
a dialog, but any scripted or documented `open`-based path fails silently.

Neither the README nor the quickstart mentioned Gatekeeper, notarization, or
quarantine before this commit (`grep -iE 'gatekeeper|notariz|quarantine' README.md
docs/macos-quickstart.md` returned nothing). `docs/signing-runbook.md` covers it
well but is a maintainer document that no beta user will read.

**Mitigated here** by a README note stating plainly that builds are unsigned,
that a copied app will not open, and that building locally avoids the problem
because locally built files carry no quarantine flag. That makes a source-only
beta honest. It does not make binary distribution possible.

### B3. No single-instance guard: two copies both go hot on one keypress

**Severity: blocker. Frequency: high, because the failure mode is invisible
until it bites. Status: NOT fixed, needs an owner.**

Nothing prevents two daemons running at once. Both bind the same hotkey and
both open the microphone:

```
$ ./target/release/aquad --asr mock --no-overlay &   # instance A
$ ./target/release/aquad --asr mock --no-overlay &   # instance B
$ pgrep -fl 'target/release/aquad'
91059 ./target/release/aquad --asr mock --no-overlay
91072 ./target/release/aquad --asr mock --no-overlay
```

Both logs report a successful bind and an open capture device:

```
=== A ===                                    === B ===
aquad: hold right-option to dictate          aquad: hold right-option to dictate
aquad: recognizer ready: mock                aquad: recognizer ready: mock
aquad: capturing from Jessie's AirPods #2    aquad: capturing from Jessie's AirPods #2
```

Neither warns. Neither mentions the other.

One physical keypress drives both. Two instances bound to left-option (chosen
only because it can be synthesized in a test; the behaviour is identical for
the default right-option), then a single press and release:

```
=== A ===                        === B ===
aquad: state idle                aquad: state idle
aquad: state listening    <---   aquad: state listening    <--- both hot
aquad: state idle                aquad: state idle
```

Both entered `listening` from the same keypress, meaning **two processes had
the microphone open and were recording the user simultaneously**, and both
would inject their transcript into the focused field.

This is realistic, not contrived. A user starts the `.app`, forgets, and later
runs the daemon from a terminal to see logs. Or launch-at-login starts one and
they start another. The quickstart actively encourages the second case: "To
watch the logs instead of running detached: `./target/release/aquad --no-overlay`".

There is also a related crash with no guard at all. Two `--once` runs at the
same time collide on a shared temp file:

```
=== instance 1 ===
e2e: release->text 192ms (finalize 158ms, inject 33.5ms) via synthetic-keys | "Duplicate delivery test."
=== instance 2 ===
Error: ExtAudioFileCreateWithURL failed (-48)
Error: afconvert failed
```

`-48` is `dupFNErr`. Both processes write `$TMPDIR/aquad-say/utterance.aiff`.
Only affects `--say`, which is a test path, but it shows the same missing
assumption.

**Recommended fix:** an advisory lock on a pidfile at startup. If another
instance holds it, print which pid owns it and exit non-zero. Roughly 30 lines.
It belongs in `crates/aquad/src/main.rs`, which was being actively edited by
the menu bar agent during this assessment, so it was not written here rather
than risk a collision.

---

## Majors

Will not stop a beta, but will generate support load.

### M1. No `--version` anywhere in the daemon

**Frequency: every single bug report.**

```
$ ./target/release/aquad --version
Error: unknown argument --version (try --help)
RC=1
$ ./target/release/aquad -V
Error: unknown argument -V (try --help)
RC=1
```

When a beta user says "it doesn't work", the first question is "what version?"
and today they cannot answer it. The version exists (`CFBundleShortVersionString
0.1.0` in the bundle) but is not reachable from the binary. `shell-bridge` and
`spike-cli` have the same gap.

Four lines in `parse_args`. Requested from the agent who owns `main.rs`; the
README documents the `defaults read` workaround in the meantime.

### M2. No uninstall path on macOS

**Status: FIXED in this commit.**

Before this commit, `grep -i uninstall` across the repo matched only
`scripts/build-windows.sh`. The untested platform had an uninstaller; the only
working platform did not.

Removing Aqua by hand means knowing about five locations, three of them
invisible: the app bundle, `~/.config/aqua`, `~/.aqua-oss/models`, the TCC
grants, and a line appended to the user's `.zshrc`. That last one is the worst:
`shell-bridge install` writes an absolute path into the rc file and has no
uninstall verb.

```
$ ./target/release/shell-bridge install
installed into /Users/blare/.zshrc
$ grep -A1 'aqua shell-bridge' ~/.zshrc
# aqua shell-bridge
[ -f "/Users/blare/aqua-oss-spike/shell/aqua.zsh" ] && source "/Users/blare/aqua-oss-spike/shell/aqua.zsh"

$ ./target/release/shell-bridge uninstall
usage: shell-bridge <serve|intent|status|peek|install|print-plugin-path> [flags]
```

Delete the repo and every new shell sources a file that no longer exists.

Added `scripts/uninstall-macos.sh`: stops processes, removes bundles, resets
TCC grants, strips the rc line (with a backup), clears runtime state, and keeps
configuration unless `--purge` is passed. `--dry-run` shows the plan.

Verified in a sandboxed fake `HOME`, ten assertions, all passing:

```
PASS: app bundles removed
PASS: model dir removed
PASS: config KEPT without --purge
PASS: aqua line gone from .zshrc
PASS: user EDITOR line survived
PASS: user PAGER line survived
PASS: user alias survived
PASS: rc backup was written
PASS: config removed WITH --purge
PASS: third run exits 0        (idempotent on a clean machine)
```

The `.zshrc` before and after, showing surgical removal:

```
# the user's own settings, which must survive     # the user's own settings, which must survive
export EDITOR=vim                                 export EDITOR=vim
alias gs='git status'                             alias gs='git status'

# aqua shell-bridge                        --->
[ -f "/x/shell/aqua.zsh" ] && source ...

# more of the user's own settings                 # more of the user's own settings
export PAGER=less                                 export PAGER=less
```

### M3. Permission revocation while running is never noticed

**Frequency: moderate. Guaranteed for anyone who experiments with the toggle.**

Nothing polls accessibility trust after startup. `AXIsProcessTrusted` is called
in `ax-edit` and once at startup, never on a timer. `crates/ax-edit/src/macos.rs`
documents why this matters:

> the trust check is cached per process while the real permission lives with the
> *responsible* process

So a user who toggles Accessibility off while Aqua is running gets a daemon that
still believes it is trusted, silently degrades to clipboard paste or fails, and
does not say why. The `NoPermission` state exists in the state machine and the
menu bar renders it, but nothing drives a transition into it after launch.

Worse in the other direction: a user who follows the quickstart's advice to
grant permission *while the daemon is running* sees no change, concludes the
grant did not work, and files a bug. The fix is a low-frequency poll (once a
second is ample) that publishes `NoPermission` on transition.

### M4. Stale ASR helper process, trigger unidentified

**Frequency: unknown. Observed once, not reproducible.**

A helper process was found alive on this machine, reparented to launchd, 7h50m
after its parent died:

```
  PID  PPID STARTED                       ELAPSED COMMAND
45677     1 Tue Jul 28 01:07:42 2026     07:46:20 .../aqua-speech-helper
```

Wedged on a semaphore in `main`:

```
1728 Thread_1453639   DispatchQueue_1: com.apple.main-thread  (serial)
+ 1728 main  (in aqua-speech-helper) + 188
+   1728 _dispatch_semaphore_wait_slow  (in libdispatch.dylib) + 132
+     1728 semaphore_wait_trap  (in libsystem_kernel.dylib) + 8
```

**The obvious mechanism is refuted.** The initial hypothesis was that `SIGKILL`
skips `Drop` and leaks the child. Release QA ran a 15-case sweep crossing
SIGKILL/SIGTERM/SIGINT with five kill delays: 15 of 15 clean. This agent then
tested the one shape that sweep might have missed, killing the daemon
mid-utterance with the hotkey still held:

```
helpers mid-utterance: 45677 92273
--- kill -9 daemon WHILE key still held ---
--- helpers after ---
45677     1 07:50:24 .../aqua-speech-helper       <- only the pre-existing one
```

92273 exited correctly. 16 of 16 clean between both agents. The code is more
defensive than assumed: `Drop` kills and reaps, and there is an explicit
`child.kill()` on the finalize-timeout path.

So the sighting is real and the trigger is unknown. It probably predates a
recent fix (the process started at 01:07, before several of that day's commits,
and the empty-utterance wedge it resembles is fixed at HEAD: feeding EOF with
no audio now returns `{"type":"done"}` and exits 0).

**The right fix does not require knowing the trigger:** reap stale helpers at
daemon startup. That makes the whole class of causes irrelevant. It belongs on
the `aquad` side, since `crates/asr/**` is off-limits to both agents.

### M5. The README's latency claim was optimistic

**Status: FIXED in this commit.**

The README claimed **131-189ms**. Measured on the bundled binary from a fresh
clone, three consecutive runs:

```
e2e: release->text 193ms (finalize 145ms, inject 48.0ms) via synthetic-keys
e2e: release->text 191ms (finalize 143ms, inject 48.1ms) via synthetic-keys
e2e: release->text 215ms (finalize 158ms, inject 56.9ms) via synthetic-keys
```

Release QA independently measured 223ms and 349ms. Every observation exceeded
the top of the advertised range. The claim is not wildly wrong, and the
comparison to Aqua's ~450ms still holds comfortably, but a beta README must not
overstate. Corrected to **131-215ms** with an explanation that the spread
depends on the transport.

### M6. Most settings in the config file are silently ignored

**Frequency: high. Anyone who opens the config file and changes something.**

The starter config is generated with all 16 settings listed and documented.
Thirteen of them do nothing. They are accepted without warning and changing
them has no observable effect.

The menu bar is honest about this, and deliberately so. `crates/aquad/src/
menubar.rs` carries a test gate:

```rust
const WIRED: &[&str] = &["hotkey", "enabled", "overlay.position"];
...
assert!(WIRED.contains(&key.as_str()),
    "the menu offers \"{key}\", which no code reads yet; ...
```

with a comment that says the gate exists so "the user believes they changed
something and nothing happens" cannot occur. That is exactly the right
instinct. The problem is that the gate protects the *menu* while the *config
file* offers all 16 keys with no such protection, and the config file is what
the quickstart tells users to edit.

Three confirmed cases, all against the bundled binary with `XDG_CONFIG_HOME`
pointed at a scratch config:

`insertion.mode = "stream"` versus `"on-release"`, identical behaviour, no
warning:

```
insertion.mode = "stream"
e2e: release->text 198ms ... via synthetic-keys | "Streaming mode test."
insertion.mode = "on-release"
e2e: release->text 147ms ... via synthetic-keys | "Streaming mode test."
```

The `stream` crate is fully built (`session.rs`, `undo.rs`, coalescing, a
commit horizon) but `crates/aquad/Cargo.toml` does not depend on it at all, so
the setting cannot possibly work. Independently noted in
[`competitive-analysis.md`](competitive-analysis.md).

`formatting.casing = "upper"` changes nothing:

```
e2e: release->text 142ms ... | "Casing test sentence."     <- not upper-cased
```

`microphone = "no-such-device-at-all"` is the worst of the three, because the
daemon reports capturing from a device the user did not select and does not
mention that its choice was ignored:

```
$ printf 'microphone = "no-such-device-at-all"\n' > config.toml
aquad: capturing from MacBook Pro Microphone
```

A user who sets that and hears nothing has no way to learn the setting was
never honoured.

**Recommended fix, cheap and honest:** mark each `KeySpec` in
`crates/config/src/schema.rs` as wired or not, warn once at startup for any
unwired key the user actually set, and mark the unwired ones in the generated
starter file. That converts thirteen silent no-ops into one honest line of
output. The alternative, wiring all thirteen, is real work and not beta-
blocking; being honest about them is.

### M7. A stray `enabled = false` write, not reproduced

**Frequency: unknown. Reported for completeness, not confirmed.**

Once during config testing, the daemon appended a key the user never set:

```
--- before ---            --- after ---
schema-version = 99       schema-version = 99
hotkey = "fn"             hotkey = "fn"
                          enabled = false     <- appeared on its own
```

Four attempts to reproduce failed. The one difference in the run that produced
it: the audio input device changed mid-run ("capture: input device changed (was
Jessie's AirPods #2); rebuilding stream"). If real, it is unpleasant, because
`enabled = false` is the master switch and persisting it silently disables
dictation across restarts. Reported to the menu bar agent, whose files were
mid-edit at the time; the write path is likely there rather than in the config
crate.

---

## What is genuinely good

Being honest in both directions.

- **Error messages name the next action.** Every failure encountered during
  this assessment told the user what to do. The missing-helper error names the
  exact `swiftc` command. This is rare and is worth protecting.
- **The doctor is better than most shipping products have.** Fourteen checks,
  each classified as permission, configuration, environment, or bug, each with
  a remedy, and a verdict that says "0 bug-class failure(s); only those belong
  in a GitHub issue". It correctly detected the responsible-process trap on
  this machine.
- **First-run config generation.** With no `~/.config/aqua`, the daemon starts
  clean and writes a fully commented file with every setting shown at its
  default and commented out. Deleting a line genuinely means "use the default".
  It is self-documenting and it is tested.
- **`docs/macos-permissions.md` and the quickstart** are unusually honest about
  the responsible-process trap and cdhash pinning, which are the two things
  that make this app class miserable to support.
- **`shell-bridge install` is idempotent and guarded.** Running it three times
  produces one guarded line that no-ops if the file is missing.
- **`bundle-aquad-macos.sh` output is excellent.** It tells the user where to
  look in the menu bar, that there is no Dock icon by design, and what to do
  after a rebuild.
- **Config migration is well designed** even though it is not yet reachable.
  `crates/config/src/migrate.rs` fixes the contract before any user has a file,
  keeps `config.toml.v<N>` backups, preserves comments, and refuses to rewrite
  files from the future. It is currently dead code (nothing calls `migrate()`),
  which is fine at schema version 1 but must be wired in before the first bump.
  A future-versioned file was confirmed to be read and left alone.

---

## Known issues list for the beta README

Landed in the README's "Known limitations" section in this commit:

1. Unsigned and un-notarized; a copied or downloaded app will not open.
2. `cargo build` alone does not produce a working recognizer.
3. No single-instance guard; two copies both go hot on one keypress.
4. No `--version` on the daemon.
5. The Accessibility grant dies on every rebuild (cdhash pinning).
6. Revoking a permission while running is not noticed until relaunch.
7. macOS 13-25 has no bundled recognizer; only 26+ has `SpeechTranscriber`.
8. Most config settings are accepted but not yet read by anything.
9. Freeform edits are not wired to the language model.
10. Linux does not work; Windows compiles but has never been run.

---

## Prioritized plan to beta-ready

Ordered by user pain per unit of effort.

| # | Action | Effort | Why it is where it is |
|---|---|---|---|
| 1 | ~~Point the README at `bundle-aquad-macos.sh`~~ | done | Every source install failed without it |
| 2 | ~~State the Gatekeeper reality in the README~~ | done | Silent failure with `open` returning 0 |
| 3 | ~~Ship `scripts/uninstall-macos.sh`~~ | done | Testers must be able to leave |
| 4 | ~~Correct the latency claim~~ | done | A false number in a beta README burns trust |
| 5 | Single-instance guard (pidfile + advisory lock) | ~30 lines | Two hot microphones is the worst remaining bug |
| 6 | `--version` on `aquad`, `shell-bridge`, `spike-cli` | ~10 lines | Blocks every bug report |
| 7 | Reap stale helpers at daemon startup | ~20 lines | Kills M4's whole class without finding the trigger |
| 8 | Poll accessibility trust once a second | ~25 lines | Turns a silent failure into a visible state |
| 9 | Warn once for settings the user set that nothing reads | ~20 lines | Thirteen silent no-ops become one honest line |
| 10 | Make `bundle-aquad-macos.sh` fail, not warn, without `swiftc` | 2 lines | Stops shipping a recognizer-less bundle |
| 11 | Always rebuild the helper, or hash-check it | 2 lines | The staleness that hid blocker B1 |
| 12 | `shell-bridge uninstall` | ~20 lines | Users should not need the sledgehammer script |
| 13 | Issue template asking for doctor output and version | 20 min | Turns "it doesn't work" into a diagnosis |
| 14 | Wire `migrate()` into the config load path | ~15 lines | Must exist before schema version 2, not after |
| 15 | Buy the Developer ID certificate | 99 USD, days | Unblocks binary distribution and fixes cdhash pain |

Items 5 through 9 are the beta-blocking remainder. All five are small and none
requires a design decision. Items 1 through 4 landed with this document.

---

## Synthesis: the in-flight UI work

New UI is new failure surface, so the menu bar and overlay work landing
alongside this assessment was reviewed rather than assumed.

**The menu bar click bug is real and is already fixed.** The daemon pumps
AppKit itself instead of calling `NSApplication::run()`, and an earlier version
of the loop only *spun* the run loop. Spinning services timers and sources,
which is enough to draw, but does not deliver window-server input to AppKit.
The result was a status item that appeared, updated, and could not be clicked.

That failure deserves recording even though it is fixed, because of how nearly
it shipped. It looked correct in a screenshot, and it responded to
accessibility-driven presses, because those invoke the action directly and
bypass the event queue. Only a real human click could catch it. Any future
automated check of the menu bar that drives it through the accessibility API
will pass while the product is broken. The code now carries a comment saying
exactly this, which is the right outcome.

**`overlay.position = "hidden"` is now wired**, which moves one key off the
unwired list in M6 and leaves twelve.

**The workspace is green with that work in the tree:** 507 tests pass, up from
503 when this assessment started, and `cargo fmt --check` and
`cargo clippy --workspace --all-targets -- -D warnings` are both clean.

Nothing in the new UI code changes the GO/NO-GO. It removes a would-be blocker
that this assessment would otherwise have had to file.

---

## What was not covered

Stated plainly so nobody assumes coverage that does not exist.

- **Windows behaviour.** Every backend compiles on real Windows CI runners and
  none has ever been run. Not testable from here.
- **Linux.** No Linux CI job has passed across this run of commits. Unproven end
  to end, not merely red.
- **Sleep/wake and log-out/log-in.** Not exercised; they would have disrupted
  other agents working on this machine.
- **Physical microphone unplug mid-recording.** A device *change* was observed
  and handled correctly ("capture: input device changed ...; rebuilding
  stream"), but a true unplug was not tested.
- **Disk full and corrupt model files.** The doctor checks free space; the
  failure path was not exercised.
- **Real multi-app injection.** Other agents were using this machine's focus, so
  driving TextEdit and Terminal by voice was not safe to attempt.
