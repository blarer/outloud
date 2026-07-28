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
shell command**. All three blockers are now cleared: the broken install path and
the undocumented Gatekeeper reality were fixed here, and the single-instance
guard was implemented and tested here.

**This verdict covers macOS only, and is not the whole picture.** The release QA
assessment in [`release-readiness.md`](release-readiness.md) reaches a stricter
conclusion on CI and Linux ("would not ship today": four glibc CI rows still
need workflow changes and have no owner, and no Linux job has passed across this
run of commits). Both can be true at once, because they answer different
questions: whether a macOS user can install and use this, and whether the
project's build and release machinery is trustworthy. A beta that ships macOS
source installs to people who build locally does not depend on the Linux rows;
anything wider does. Read both before deciding.

Do **not** ship a downloadable `.app` yet. That path is blocked by
notarization, which needs a paid Apple Developer account and cannot be worked
around. The conditional in "conditional go" is that sentence and nothing else.

Every blocker and every major except two were closed during this assessment,
most of them in code rather than in prose. The two open items are M6 (settings
accepted but not read, detection built and tested, waiting on a call site) and
M8, which is a sequencing decision rather than a bug: the pending rename to
Hexavoice changes the bundle identifier, and TCC keys grants by identifier, so
every permission an existing tester has granted stops applying. That one costs
nothing if the rename lands before any stranger installs, and is entirely
self-inflicted if it lands after.

The reason this is a GO rather than a NO-GO: the product's hard parts are
genuinely done. Dictation, edit-by-voice, and terminal injection work, latency
beats the commercial competition, error messages are unusually good, and the
doctor is better than most shipping products have. What is missing is the
boring layer around the outside, which is cheap to add and is what this
document is about.

**End-to-end validation.** After the fixes below landed, a stranger's path was
walked start to finish on a clean clone, following the corrected README
verbatim with no other steps:

```
$ git clone <repo> && cd aqua-oss
$ ./scripts/bundle-aquad-macos.sh
==> Building aquad (release)
==> Building the speech helper
    dist/Aqua.app: valid on disk
Built: dist/Aqua.app

$ ./dist/Aqua.app/Contents/MacOS/Aqua --once --say "hello from a local dictation daemon" --no-overlay
e2e: release->text 383ms (finalize 326ms, inject 56.6ms) via synthetic-keys | "Hello from a local dictation demon."
RC=0
```

That is the documented path working from nothing, which it did not do when this
assessment started. Two things are visible in that output and are recorded
honestly rather than smoothed over: the recognizer produced "demon" for
"daemon", and 383ms is well above even the corrected 131-215ms range because it
includes a cold first-utterance model load. Neither is a defect; both are what a
first run actually looks like.

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

**B1 regressed once and was re-fixed.** The Hexavoice rename renamed
`bundle-aquad-macos.sh` to `bundle-hexad-macos.sh` but left the README pointing
at the old name, so the documented install broke again in exactly the same
place:

```
$ ./scripts/bundle-aquad-macos.sh
bash: ./scripts/bundle-aquad-macos.sh: No such file or directory
```

Re-fixed and re-verified end to end on the renamed tree:

```
$ ./scripts/bundle-hexad-macos.sh
==> Building hexad (release)
==> Building the speech helper
Built: .../dist/Hexavoice.app
$ ./dist/Hexavoice.app/Contents/MacOS/Hexavoice --version
hexad 0.1.0
$ ./dist/Hexavoice.app/Contents/MacOS/Hexavoice --once --say "..." --no-overlay
e2e: release->text 352ms ... | "Hello from a local dictation demon."
```

That this recurred within hours, from a rename rather than from the original
oversight, is the argument for the item below: **the documented install path
needs an automated check.** Nothing currently fails when the README names a
script that does not exist, and a beta user is precisely the person who finds
out first. It is the highest-value untested thing left.

---

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
until it bites. Status: FIXED in this assessment, with tests.**

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

**Fixed in this assessment.** `crates/aquad/src/instance.rs` takes an advisory
`flock(LOCK_EX | LOCK_NB)` on a lock file in `$XDG_RUNTIME_DIR` (else the temp
directory) before anything is bound or opened, so a refused start cannot
disturb the daemon already running.

`flock` rather than a pid file that gets parsed, deliberately. The obvious
design, write our pid and have the next process check whether it is alive,
fails in exactly the case that matters: a daemon killed with `SIGKILL` never
cleans up, and the next launch finds a pid that is either dead or has since
been reused, and has to guess. A `flock` is owned by the open file description
and the kernel releases it however the process dies, so "is another daemon
running?" has an authoritative answer. The pid is still written to the file,
but only so the error message can name it.

Before and after, same commands:

```
BEFORE
$ ./target/release/aquad --asr mock --no-overlay &   # A
$ ./target/release/aquad --asr mock --no-overlay &   # B
$ pgrep -f 'release/aquad' | wc -l
2                                    <- both running, both hot on one keypress

AFTER
$ ./target/release/aquad --asr mock --no-overlay &   # A
$ ./target/release/aquad --asr mock --no-overlay     # B
Error: aquad is already running (pid 17138). Quit it from the menu bar, or
`kill 17138`, then start this one. Running two copies makes both record you
and both type what you said.
B exit code: 1
$ pgrep -f 'release/aquad' | wc -l
1
```

The three paths that a naive guard breaks were each verified rather than
assumed:

- **Restart after quit**: start, `pkill`, start again, reaches `state idle`.
- **Restart after `SIGKILL`**: start, `kill -9`, start again, reaches
  `state idle`. No stale lock, which is the whole reason for `flock`.
- **Concurrent `--once`**: still allowed, because it is a measurement that
  neither binds the hotkey nor stays resident, and benchmarks run several at
  once. Both runs commit.

Six unit tests cover acquire, contention with the pid reported, release on
drop, file cleanup, a stale lock file from a dead process, and the wording of
the refusal message. That last one is a test because the message *is* the
feature: a daemon with no Dock icon that says only "already running" leaves the
user with no way to act.

**The `--say` collision is fixed too**, since it is the same missing
assumption. `synthesize()` now writes to a per-process temp directory:

```
BEFORE (two concurrent --once runs)
run 1: e2e: release->text 192ms ... | "Duplicate delivery test."
run 2: Error: ExtAudioFileCreateWithURL failed (-48)
       Error: afconvert failed

AFTER
run 1: e2e: release->text 411ms ... | "Concurrent one."
run 2: e2e: release->text 283ms ... | "Concurrent too."
```

Windows is left unguarded on purpose: there is no `flock`, and the right
mechanism there is a named mutex, which should be written when the Windows
backends are first exercised on real hardware rather than guessed at now.

---

## Majors

Will not stop a beta, but will generate support load.

### M1. No `--version` anywhere in the daemon

**Frequency: every single bug report. Status: FIXED in `6990886`.**

Before the fix:

```
$ ./target/release/aquad --version
Error: unknown argument --version (try --help)
RC=1
$ ./target/release/aquad -V
Error: unknown argument -V (try --help)
RC=1
```

After, verified against a rebuilt binary:

```
$ ./target/release/aquad --version
aquad 0.1.0
RC=0
$ ./target/release/aquad --help | grep -i version
--version        print the version and exit
```

When a beta user says "it doesn't work", the first question is "what version?"
and until this landed they could not answer it. Fixed by the menu bar agent on
request, since `main.rs` was its file and was dirty at the time.

`shell-bridge` and `spike-cli` still have the gap, which matters much less: the
daemon is what users run.

### M2. No uninstall path on macOS

**Frequency: every tester who decides the tool is not for them, so a
single-digit percentage of a 50-person beta, but a disproportionate share of
the bad word of mouth. Status: FIXED in this commit.**

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

**Frequency: moderate. Guaranteed for anyone who experiments with the toggle.
Status: FIXED in `c1ca678`.**

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
grant did not work, and files a bug. That is the direction that will actually
generate support mail, because granting-while-running is what the documentation
tells people to do.

**Fixed in `c1ca678`, and improved on the recommendation above.** `Runtime`
gained `accessibility_blocked`, the run loop polls `ax_edit::is_trusted(false)`
on a 30-frame tick primed to fire immediately at launch, and the menu copies
the `microphone_blocked` row including the deep link.

Two departures from the design sketched here, both better than what was
proposed:

1. The setter takes the **positive** sense, `set_accessibility_trusted(bool)`,
   rather than mirroring the `set_microphone_blocked` event shape. The
   microphone flags are events (a stream came up, a stream died) while trust is
   a *level* that is polled, so every poll must be able to clear the flag as
   well as set it. That is precisely what makes the grant-while-running
   direction work, which is the common case.
2. The **glyph itself** changes, not only a menu row. A permission problem you
   can only discover by clicking leaves a daemon that still looks healthy while
   dictation is broken, and answering "is it on?" without a click is the entire
   reason the status item exists. The override applies only to `Idle`, so it
   can never replace the microphone-hot glyph mid-utterance, which is the one
   state a user must be able to trust absolutely. Pinned by
   `a_live_utterance_is_never_interrupted_by_the_permission_glyph`.

Verified on the real thing, not only in tests: an ad-hoc build genuinely lost
its grant when its `cdhash` changed on rebuild, and the running app showed the
amber warning triangle, the tooltip "Aqua: Accessibility permission needed",
and the row that opens the pane. The recovery direction is covered by unit test
rather than live, because `TCC.db` is not readable, correctly.

Related, from the same agent: the single-instance refusal now also shows a
**dialog** when stderr is not a terminal. The message this assessment added was
only ever seen by someone watching a terminal, and a bundled launch has none,
so double-clicking `Aqua.app` while a daemon was already running still appeared
to do nothing at all. The wording and the menu-bar-first ordering are unchanged.

### M4. Stale ASR helper process, trigger unidentified

**Frequency: unknown. Observed once, not reproducible. Status: symptom fixed;
cause still unidentified.**

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

**The right fix does not require knowing the trigger, and is now in.** The
daemon reaps stale helpers at startup, immediately after taking the
single-instance lock. That ordering is what makes it safe: once the lock is
held, no other daemon is running, so any helper still alive necessarily
belongs to a daemon that is gone. Only a daemon ever spawns one, and each is
used for a single utterance, so a leftover is always wrong.

This is deliberately a cure for the class rather than the cause. Waiting to
identify the trigger before making the symptom impossible would leave beta
users holding a live microphone session owned by a process that no longer
exists.

When it finds something, it says so rather than tidying up silently:

```
aquad: cleaned up 1 stale speech helper(s) from a previous run
```

Verified against a real orphaned helper, in a scratch directory under a
private name so the test could not touch a legitimately running one:

```
orphaned helper pids: 35539
signalled 1 helper(s)
helper pids after: (none)
PASS: SIGTERM reaps a stale helper

=== the other agent's real helper must be UNTOUCHED ===
32936 .../Aqua.app/Contents/MacOS/aqua-speech-helper     <- still alive
```

The pid-selection logic is split from the killing so it can be unit-tested
without signalling anything: one test proves our own pid is filtered out (a
daemon that killed itself at startup would be a spectacular regression, and
`pgrep -f` matches whole command lines so it is reachable), and one proves
empty, blank, and non-numeric `pgrep` output are all tolerated on the startup
path.

### M5. The README's latency claim was optimistic

**Frequency: 100% of readers, though only the sceptical ones will measure and
notice. Status: FIXED in this commit.**

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

**Update, partially landed.** The release QA agent implemented the detection
half in `22f579b`: `KeySpec` now carries a `wired` flag and
`Config::inert_settings()` returns the unwired keys **the user actually set**,
correctly staying silent about unwired defaults. It is tested.

The warning does not reach the user yet, because nothing calls it. Verified
against a fresh build with two inert keys set:

```
$ printf 'schema-version = 1\nformatting.casing = "upper"\nlanguage = "fr"\n' > config.toml
$ ./target/release/aquad --asr mock --no-overlay
aquad: state model-loading
aquad: hold right-option to dictate
aquad: recognizer ready: mock
aquad: state idle
aquad: capturing from MacBook Pro Microphone
```

Still no warning. The remaining work is a single call site, whose natural home
is `crates/aquad/src/main.rs` or `menuhost.rs`. This item stays open.

### M7. `enabled = false` self-write: RETRACTED, it was this assessment's own clicks

**Status: not a defect. Cause found. Regression test added.**

This document previously reported, across two revisions, that the daemon wrote
`enabled = false` into a config file on its own, escalating it to "the one open
item that can silently disable the product". **That was wrong, and the cause was
this assessment's own activity.**

Both sightings were verification clicks on the menu's "Pause Dictation" row,
made while the menu bar agent was proving its AppKit event-dispatch fix. The
second sighting's file mtime is 09:15, the same minute a synthetic mouse click
landed on that row; the first, around 09:02, was the same action driven through
the accessibility API. An accessibility-driven press invokes the menu action
directly, so it *is* a real click as far as the code is concerned. Two agents
poking a live machine that also held a real user config produced what looked
like spontaneous writes.

Two independent audits closed it. The release QA agent found exactly one
non-test filesystem write in the whole config crate, and it is unreachable for
an existing file:

```rust
match std::fs::read_to_string(&path) {
    Ok(text) => Ok((path, text)),                  // existing file: returns, never writes
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
        std::fs::write(&path, &text)?;             // only when absent
```

and what it writes is `starter_file()`, which emits every key commented out, so
it cannot produce an uncommented `enabled = false` even then. `layers.rs`,
`migrate.rs`, and `profile.rs` are pure string-in, string-out with no I/O.

The menu bar agent independently demonstrated that a daemon left running
unattended for 45 seconds, including a real audio device change, left
`config.toml` byte-identical at md5 `7765b6776ec919e26a0a517df228197e`, matching
the pristine backup taken at the start of this assessment.

**One real bug was found underneath the false alarm.** An earlier revision of
the Pause row wrote the *current* value rather than the negation, so clicking it
persisted a key without changing anything. Fixed in `e5fd7f2`, and
`write_setting` now skips writes that would not change the file, so a duplicated
or spurious click cannot persist a key the user never chose.

The scenario is kept in the known-issues list in weakened form, and the passive
paths are now pinned shut by a regression test rather than by assertion.
`nothing_but_a_click_writes_the_config` (`084b14e`) exercises construction,
reload, watcher polling, and model rebuilds carrying a device-change detail, and
fails if the file changes. Verified passing:

```
$ cargo test -p aquad nothing_but_a_click
test menuhost::tests::nothing_but_a_click_writes_the_config ... ok
```

**The lesson is methodological and worth more than the finding was.** An
observation on a machine with several agents active is not evidence about the
product until the observer's own footprint is excluded. This document escalated
it twice on two sightings without doing that. The correct move, in hindsight,
was to reproduce it on an idle machine with no other agent running before
promoting it at all.

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
- **The issue templates already ask the right questions.** `bug-report.yml`
  makes doctor output mandatory and asks how the binary was launched and
  whether it was rebuilt since granting, which are precisely the two questions
  that separate a real bug from the environment. Refreshed here to name
  `./scripts/doctor.sh` and `aquad --version` directly, both of which have
  changed since the template was written, and to point at the README's new
  known-limitations list before someone files a known issue.
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
3. ~~No single-instance guard.~~ Fixed; a second copy is refused and told
   which pid to quit.
4. ~~No `--version` on the daemon.~~ Fixed; `aquad --version` prints `aquad 0.1.0`.
5. The Accessibility grant dies on every rebuild (cdhash pinning).
6. ~~Revoking a permission while running is not noticed until relaunch.~~
   Fixed; the menu bar glyph changes within a second either way.
7. macOS 13-25 has no bundled recognizer; only 26+ has `SpeechTranscriber`.
8. Most config settings are accepted but not yet read by anything.
9. Freeform edits are not wired to the language model.
10. Linux does not work; Windows compiles but has never been run.
11. If you tested before the Hexavoice rename, your permission grants do not
    carry over: re-grant Accessibility and Microphone, and remove the stale
    entry for the old name from System Settings by hand.

---

## Prioritized plan to beta-ready

Ordered by user pain per unit of effort.

| # | Action | Effort | Why it is where it is |
|---|---|---|---|
| 1 | ~~Point the README at `bundle-aquad-macos.sh`~~ | done | Every source install failed without it |
| 2 | ~~State the Gatekeeper reality in the README~~ | done | Silent failure with `open` returning 0 |
| 3 | ~~Ship `scripts/uninstall-macos.sh`~~ | done | Testers must be able to leave |
| 4 | ~~Correct the latency claim~~ | done | A false number in a beta README burns trust |
| 5 | ~~Single-instance guard (`flock` on a lock file)~~ | done | Two hot microphones was the worst remaining bug |
| 6 | ~~`--version` on the daemon~~ | done | Blocked every bug report |
| 7 | ~~Reap stale helpers at daemon startup~~ | done | Killed M4's whole class without finding the trigger |
| 8 | ~~Poll accessibility trust once a second~~ | done | Turned a silent failure into a visible glyph |
| 9 | Call `inert_settings()` at startup (detection landed in 22f579b, no caller yet) | ~4 lines | Twelve silent no-ops become one honest line |
| 10 | Land the rename BEFORE any stranger installs, or write the re-grant step into the release notes | sequencing | Otherwise every existing tester's grants silently die (M8) |
| 10 | Make `bundle-aquad-macos.sh` fail, not warn, without `swiftc` | 2 lines | Stops shipping a recognizer-less bundle |
| 11 | Always rebuild the helper, or hash-check it | 2 lines | The staleness that hid blocker B1 |
| 12 | `shell-bridge uninstall` | ~20 lines | Users should not need the sledgehammer script |
| 13 | ~~Issue template asking for doctor output and version~~ | done | Turns "it doesn't work" into a diagnosis |
| 14 | Wire `migrate()` into the config load path | ~15 lines | Must exist before schema version 2, not after |
| 15 | Buy the Developer ID certificate | 99 USD, days | Unblocks binary distribution and fixes cdhash pain |

Items 1 through 8 landed during this assessment. Item 9 is the only remaining
item with a user-visible effect, and its hard half is already done: the
detection exists and is tested, it just has no call site yet.

---

### M8. The Hexavoice rename voids every existing tester's permission grants

**Frequency: 100% of anyone who tests before the rename and updates after.
Status: partially handled; the rest is a release-sequencing decision.**

The product is being renamed from Aqua to Hexavoice, which changes the bundle
identifier from `dev.aquaoss.aquad` to `dev.hexavoice.hexad`. TCC keys its
grants by bundle identifier, verifiable directly:

```
$ tccutil reset Accessibility dev.aquaoss.nonexistent
tccutil: No such bundle identifier "dev.aquaoss.nonexistent": The operation
couldn't be completed. (OSStatus error -10814.)
```

Per-identifier, with no aliasing. So a renamed build is, to macOS, an entirely
different application:

1. Every Accessibility and Microphone grant a tester has already given becomes
   unreachable from the new binary. Dictation stops working after an update
   that otherwise looks routine.
2. The old grants do not disappear. They sit in System Settings under the old
   name, pointing at an app that may no longer exist, and there is no API to
   remove a row, only to reset the grant behind it.
3. It presents in the worst way for this app class: the *old* entry's toggle
   still reads "on", so a user glancing at Settings concludes permissions are
   fine while nothing works.

This is the highest-value thing to get right in the release sequencing, because
it turns a cosmetic change into "the update broke it" for every existing tester
at once.

**Partially handled here.** `scripts/uninstall-macos.sh` now removes both
naming generations, so an upgrader cannot strand the old app or orphan its
grants:

```
==> rm -rf .../dist/Aqua.app
==> rm -rf .../dist/Hexavoice.app
==> tccutil reset Accessibility dev.aquaoss.aquad
==> tccutil reset Accessibility dev.hexavoice.hexad
```

Listing both costs nothing, since resetting an absent identifier is a no-op.

**Still needed, owned by whoever lands the rename:**

- Release notes must say plainly that permissions have to be re-granted, and
  give the `tccutil reset` line for the old identifier.
- The stale System Settings row has to be removed by hand, since software
  cannot do it.
- Best of all: land the rename **before** the beta, not during it. This costs
  nothing if no stranger has ever installed the old identifier, and is entirely
  self-inflicted if one has.

Related verification debt, in the release QA lane rather than this one: the
identifier change invalidates the Designated Requirement check performed on the
current build, so that pass needs re-running afterwards.

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

## A process hazard worth more than any single finding

This is not a product defect, but it nearly shipped one and it will recur.

Several agents share one working tree. During this assessment, `HEAD` stopped
compiling while **every local checkout still built and tested clean**. A
`spawn_mic -> Result` change sat uncommitted in `source.rs` while the matching
`?` at the call site in `main.rs` was committed, swept into an unrelated commit
by a `git add` on a shared dirty file. Each person had both halves locally, so
each person's `cargo test` passed. Only a fresh clone saw the truth:

```
$ git clone <repo> /tmp/check && cd /tmp/check && cargo test --workspace
error[E0277]: the `?` operator can only be applied to values that implement `Try`
   --> crates/aquad/src/main.rs:286:37
```

This is the nastiest shape of failure available in a shared tree: the tree lies
to everyone who has it, and only a stranger cloning fresh is told the truth. A
beta tester is exactly that stranger.

**`scripts/verify-head.sh` now makes it a one-command check.** It clones
committed `HEAD` to a scratch directory and runs fmt, clippy, the test suite,
and a `--no-default-features` check, the last because the headless
configuration resolves features differently and is the one least likely to be
exercised by anyone working on macOS.

Verified in both directions, which is the only way to know a guard works:

```
$ ./scripts/verify-head.sh                 # current HEAD
HEAD is buildable from a fresh clone.
RC=0

$ ./scripts/verify-head.sh 66a3eba         # the commit that was actually broken
    note: the source repo has 10 uncommitted path(s), none of which are tested here
==> cargo clippy --workspace --all-targets -- -D warnings
error[E0277]: the `?` operator can only be applied to values that implement `Try`
RC=101
```

It should be run before declaring anything done, and especially after a large
mechanical change such as the pending rename, where the blast radius of a
half-committed edit is largest.

**It happened again while this was being written**, which is the strongest
argument for the script existing. The commit that was meant to introduce
`verify-head.sh` raced with another agent's `git add`, and the script, this
section, and the `CONTRIBUTING.md` note all landed inside `8c2b50c`, a commit
about Linux CI dependencies. Content survived byte-identical and the file kept
its executable mode, so nothing was lost, but the attribution and the commit
message are wrong and were left that way: rewriting history that several agents
have already pulled is far worse than a misleading message.

Twice in one session, from two different directions. The window in both cases
was between `git add` and `git commit`, which is the thing to close. Staging
and committing as one operation, or stashing first, would have prevented both.

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
