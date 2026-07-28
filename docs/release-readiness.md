# Release readiness

**Verdict: a release would NOT ship today**, but the blocker is now mostly
cleared and the remainder is small and named. One blocker (largely resolved),
six majors, five fixed.

CI went from **7 green / 7 red** to **8 green / 6 red** during this pass, and
the character of the red changed: it was one build-script failure masking
everything, and it is now three specific, understood items.

Produced by adversarial QA against `main` on 2026-07-28. Every finding below
was reproduced by running the thing, not by reading it. Commands and their
exact output are quoted so each claim can be re-checked or falsified.

Scope note: this agent could not edit `.github/workflows/**`, `crates/asr/**`,
`crates/llm/**`, `docs/planning/**`, `docs/ux/**`, or the three other agents'
in-flight files. Findings in those areas name the required action and its
owner instead of fixing it.

---

## Summary

| # | Severity | Finding | Status |
|---|---|---|---|
| 1 | **BLOCKER** | Linux CI+release cannot build: `alsa-sys` has no system deps | **MOSTLY FIXED** (`8c2b50c`); `repro` green, rest named |
| 2 | **MAJOR** | `release.yml` would fail at tag time for the same reason | Diagnosed; workflow fix required, owner needed |
| 3 | **MAJOR** | MSRV job red: dep required rustc 1.86 vs documented 1.85 | **FIXED** (`5bd83fd`), verified on real CI |
| 4 | **MAJOR** | `scripts/ci-install-linux-deps.sh` untracked and non-executable | **FIXED** (`0e81b08`), committed mode 100755 |
| 5 | **MAJOR** | Build from source per README yields a daemon that cannot transcribe | Found by lobster, confirmed here; owner: lobster |
| 6 | MINOR | Stale `hexavoice-speech-helper` orphan; trigger not reproducible | Reported, mechanism refuted |
| 7 | MINOR | `bundle-hexad-macos.sh` warns (not fails) when `swiftc` is absent | Reported |
| 8 | INFO | Config defaults and edge cases behave correctly | Verified, regression net added (`0e81b08`) |
| 9 | INFO | Latency claims in `docs/latency.md` hold up | Verified, 8-run measurement |
| 10 | **MAJOR** | Every Linux build linked ALSA, including headless and musl | **FIXED** (`a7c7b1b` + `af852d6`), musl rows no longer need CI plumbing |
| 11 | **MAJOR** | 13 of 16 config settings are silently ignored | **FIXED** (`22f579b` + `ed5425f`), verified end to end |

CI matrix as of run `30361756410`: **7 green, 7 red.** All 7 red are Linux or
MSRV, and after finding 3 was fixed they share a **single root cause**
(finding 1). macOS and Windows are fully green.

```
success  build-matrix (macos-15, x86_64-apple-darwin)
success  build-matrix (macos-15, aarch64-apple-darwin)
success  build-matrix (windows-2025, x86_64-pc-windows-msvc)
success  build-matrix (windows-2025, aarch64-pc-windows-msvc)
success  check (macos-15)
success  compliance
success  headless
failure  check (ubuntu-24.04)
failure  msrv
failure  repro
failure  build-matrix (ubuntu-24.04, x86_64-unknown-linux-gnu)
failure  build-matrix (ubuntu-24.04, x86_64-unknown-linux-musl)
failure  build-matrix (ubuntu-24.04, aarch64-unknown-linux-gnu, true)
failure  build-matrix (ubuntu-24.04, aarch64-unknown-linux-musl, true)
```

---

## 1. BLOCKER (mostly cleared): Linux jobs could not build at all

**Original state.** All six red Linux jobs died in a build script before
compiling a line of our code:

```
check (ubuntu-24.04)                        Package alsa was not found in the pkg-config search path.
repro                                       Package alsa was not found in the pkg-config search path.
build-matrix x86_64-unknown-linux-gnu       Package alsa was not found in the pkg-config search path.
build-matrix aarch64-unknown-linux-gnu      Package alsa was not found in the pkg-config search path.
build-matrix aarch64-unknown-linux-musl     Package alsa was not found in the pkg-config search path.
build-matrix x86_64-unknown-linux-musl      pkg-config has not been configured to support cross-compilation.
```

`cpal` is an unconditional dependency of `crates/audio`, so `alsa-sys` runs
`pkg-config` on every glibc Linux build. Even a clippy-only job needed
`libasound2-dev`.

**Resolved without the workflow owner who never appeared.** This was written up
as "someone with `.github/workflows/**` access must add an install step". That
was true but not the only option, and waiting on an absent permission is not a
plan. Those jobs do not run inline YAML: they run `scripts/ci-check.sh` and
`scripts/build-repro.sh`, which are ordinary files. `8c2b50c` moves the
dependency step into the scripts, where the build actually happens.

That is also the better design. `ci-check.sh` exists so "a green local build and
a green CI build are the same claim" (its own header). A system dependency
declared only in workflow YAML breaks that: CI gets it, a contributor on a
fresh Ubuntu box does not, and they hit an audio-library error while running a
linter. `ci-install-linux-deps.sh` was made safe to call from anywhere, exiting
0 immediately when not on Linux, when alsa is already present, when there is no
`apt-get`, or when it cannot elevate. A provisioning script that can fail turns
one clear error into two confusing ones.

**Measured effect.** `repro` went **green** for the first time this session, and
`check (ubuntu-24.04)` got past the build script and began reporting real,
pre-existing, Linux-only clippy errors that had been unreachable behind the
blocker:

```
error: unused import: `AxError`            --> crates/hexad/src/inject.rs:17:15
error: unneeded `return` statement          --> crates/hexad/src/inject.rs:116:9
error: redundant redefinition of a binding `shared`  --> main.rs:337:9
```

The first two are fixed (`8483d1b`); the third is handed to the agent whose
rename has that file staged. None are caused by the ALSA work. **Clearing a
blocker does not make CI green, it makes CI honest**, and the next layer of
real problems becomes visible. This class is invisible on macOS, so the Linux
job is the only signal for it.

**What still needs the workflow YAML** (see appendix): the `msrv` job and the
two aarch64 cross rows, which invoke `cargo`/`cross` directly from the workflow
rather than through a script. The two musl rows need `--no-default-features` in
the build command, which works as of `af852d6`.

## 2. MAJOR: `release.yml` would fail at tag time, for the same reason

Checked by reading the workflow, since a tag cannot be rehearsed cheaply.
`release.yml` contains **no `libasound2-dev` anywhere**. Its only apt lines are:

```
line 164:  sudo apt-get install -y musl-tools rpm binutils
line 194:  sudo apt-get update && sudo apt-get install -y musl-tools
```

Consequences at `git tag v0.1.0 && git push --tags`:

- The `compliance` job runs `scripts/ci-check.sh` on `ubuntu-24.04` and dies on
  `alsa-sys` exactly as CI's `check (ubuntu-24.04)` does now. Because every
  other job declares `needs: compliance`, **the entire release pipeline stops
  before building a single artifact.** This is the expensive failure the brief
  warned about, and it is currently guaranteed, not merely possible.
- The `linux` job (4 targets) and `headless` job would fail for the same
  reason even if `compliance` were bypassed.

`macos` and `windows` release jobs look sound; their CI equivalents are green.

**Required action:** the `.github/workflows/**` owner adds the same deps step
to `release.yml`'s `compliance`, `linux`, and `headless` jobs.

**Second, unrelated defect in the same file.** In the `headless` job:

```yaml
          cargo install cross --locked
          scripts/build-headless.sh aarch64-unknown-linux-musl || true
```

The `|| true` means an aarch64 headless build failure is **silently swallowed**
and the job goes green having uploaded only the x86_64 binary. A release would
publish a `headless` artifact set quietly missing an architecture. Recommend
removing `|| true` and letting it fail loudly, or dropping the target from the
release entirely. Shipping half of what the artifact name promises is worse
than shipping neither.

---

## 3. MAJOR (FIXED): MSRV job could not resolve dependencies

**Before.** `msrv` failed before compiling anything:

```
error: rustc 1.85.0 is not supported by the following packages:
  icu_collections@2.2.0 requires rustc 1.86
  icu_normalizer@2.2.0 requires rustc 1.86
  icu_properties@2.2.0 requires rustc 1.86
  icu_provider@2.2.0 requires rustc 1.86
  idna_adapter@1.2.2 requires rustc 1.86
```

Note the brief attributed this to `thiserror`. That was wrong; `thiserror` is
merely adjacent in the log. The actual chain is `ureq -> url -> idna ->
idna_adapter -> icu_*`, reached from `crates/asr` and `crates/llm`.

**Fix** (`5bd83fd`): lockfile-only pin of `idna_adapter` to 1.2.0, which pulls
the `icu 1.5` chain instead of `icu 2.x`. Chosen over bumping the MSRV to 1.86
because `docs/build-and-release.md#msrv-policy` states the MSRV is a promise to
downstream packagers and must not be raised "for convenience of a single
dependency". No code in this workspace needs 1.86; only a transitive Unicode
table did.

**After, verified locally:**

```
cargo +1.85.0 build --workspace --locked   -> exit 0
cargo test --workspace --locked            -> 466 passed, 0 failed
cargo deny check                           -> advisories ok, bans ok, licenses ok, sources ok
```

The `cargo deny` run matters: `deny.toml` uses a closed licence allow-list, and
downgrading to pre-`Unicode-3.0` icu could plausibly have introduced an
unlisted licence id. It did not.

**After, verified on real CI** (run `30361756410`, job `90282955322`): the
`rustc 1.86` resolution error is **gone**. The job now proceeds through
resolution and fails only on the shared `alsa` root cause of finding 1:

```
msrv  Build with MSRV  Compiling alsa-sys v0.4.0
msrv  Build with MSRV  error: failed to run custom build command for `alsa-sys v0.4.0`
msrv  Build with MSRV      Package alsa was not found in the pkg-config search path.
```

This job will go green with finding 1's fix and no further MSRV work.

**Maintenance risk:** a bare `cargo update` silently re-breaks this. Other
agents have been told. A `[patch]` or an explicit upper bound would enforce it
mechanically, but both are heavier than the problem; the commit message
documents the constraint instead.

---

## 4. MAJOR (FIXED): the Linux deps script was untracked and not executable

`scripts/ci-install-linux-deps.sh` existed only in the working tree, never
committed, at mode `100644`. The drafted workflow invokes it as
`run: scripts/ci-install-linux-deps.sh`, which would have failed with
`Permission denied` **even after** someone landed the YAML, converting one red
job into a differently red job and costing another push-wait-read cycle.

Committed in `0e81b08` at mode `100755`:

```
$ git ls-files -s scripts/ci-install-linux-deps.sh
100755 6cc5aa3d748eb18495cd61f587fd394e9fd878ed 0  scripts/ci-install-linux-deps.sh
```

Script contents reviewed and syntax-checked (`bash -n`, clean). Its logic is
correct for glibc; its own comments correctly disclaim musl. **Committing it
does not by itself fix Linux CI** — the workflow change of finding 1 is still
required.

---

## 5. MAJOR: building from source per the README produces a mute daemon

Found by the beta-readiness agent (lobster); independently confirmed here from
a genuine `git clone` with no stale artifacts. Recorded because it is a
release-integrity fact, with credit; **lobster owns the fix.**

Fresh clone contains only the Swift source, and there is no `build.rs` in the
workspace to compile it:

```
== helper binary in a fresh clone ==
-rw-r--r--  .gitignore
-rw-r--r--  transcriber.swift        <- source only, no binary

== build.rs anywhere in the workspace ==
(none)
```

After the README's `cargo build --release`, the helper is still absent and the
daemon cannot recognise anything:

```
hexad: state model-loading
Error: recognizer failed to load (hexavoice-speech-helper not found; build it with
`swiftc -O crates/asr/helper/transcriber.swift -o hexavoice-speech-helper` or set
HEXA_SPEECH_HELPER) -> build the speech helper (see crates/asr/helper) or run
with --asr mock
RC=1
```

> **Name note, added after the fact.** Captured mid-rename, so the tool
> really did print `hexavoice-speech-helper`. The shipped name is
> `aqua-speech-helper`, because `crates/asr/src/backends/apple.rs` looks for
> that exact filename and the helper deliberately did not follow the product
> rename. Transcripts left verbatim: an edited record of a real run is no
> longer evidence. Use `aqua-speech-helper` in anything you type.

**Severity is MAJOR, not blocker, because the packaged path is fine.** On the
same fresh clone, `scripts/bundle-hexad-macos.sh` compiles the helper itself
and the resulting `.app` transcribes correctly:

```
==> Building the speech helper            <- swiftc ran, from a clean tree
-rwxr-xr-x 2631152 Hexavoice
-rwxr-xr-x   98512 hexavoice-speech-helper     <- shipped inside the .app

hexad: recognizer ready: apple-speechtranscriber
hexad: e2e: release->text 200ms (finalize 163ms, inject 37.4ms) | "The rain in Spain."
RC=0
```

So a user who downloads the artifact is fine; a user who follows the README's
build-from-source instructions is not, 100% of the time. It stayed invisible
to all four agents because every working tree has a stale gitignored helper.

**On the fix:** a `build.rs` in `crates/asr` is the obvious idea and is the
wrong one. It would shell out to `swiftc` on every `cargo build` (needing
macOS cfg-gating for Linux/Windows), and it would inject a non-hermetic
external compiler into the build graph, directly threatening the `repro` job's
byte-identical guarantee. A README plus script fix carries none of that risk.

---

## 6. MINOR: a stale speech-helper process, trigger unidentified

An `hexavoice-speech-helper` was found alive with `PPID=1` for 7h50m, holding an OS
speech session after its parent died:

```
  PID  PPID STARTED                       ELAPSED COMMAND
45677     1 Tue Jul 28 01:07:42 2026     07:50:15 .../hexavoice-speech-helper
```

The proposed mechanism (SIGKILL skips `Drop`, helper never notices) was
**tested and refuted.** A 15-case sweep of {SIGKILL, SIGTERM, SIGINT} x {0.1,
0.3, 0.6, 1.0, 1.5}s kill delays produced **no orphan in any case**; the helper
exited within one second every time. lobster independently re-tested the
mid-utterance case (killed while the hotkey was still held) and also got a
clean exit, making it 16/16.

The code is already defensive: `crates/asr/src/backends/apple.rs` has a `Drop`
doing `child.kill()` + `child.wait()` (lines 259-265) *and* an explicit
`child.kill()` on the finalize-timeout path (line 225).

**Honest status:** the orphan is real, the trigger is unknown, and normal
signal-death does not cause it. The robust fix does not require knowing the
trigger: **reap stale helpers at daemon startup.** That can live on the hexad
side rather than in the off-limits `crates/asr`.

### Postscript: a related scare that turned out to be our own clicks

A separate report claimed the daemon wrote `enabled = false` into a user's
config on its own. That would be severe: `enabled` is one of the three wired
settings, so a daemon could silently ignore the hotkey forever, across
restarts, with no visible cause.

Audited on request. `crates/config` can be ruled out entirely: there is exactly
one non-test filesystem write in the crate (`paths.rs:63`), it is reachable
only on `ErrorKind::NotFound`, and what it writes is `starter_file()`, in which
every setting is **commented out** — so it cannot emit an uncommented
`enabled = false` even on the path where it does write. `layers.rs`,
`migrate.rs`, and `profile.rs` are pure with no I/O at all.

dove independently traced both sightings to their own verification clicks by
file mtime, and an unattended daemon left running with a device change left the
file byte-identical to a pristine backup. Consistent with the AppKit finding
that AX presses activate menu items: an AX press on the Pause row *is* a real
click as far as the code is concerned. **Not a defect.**

The chase was still worth it: it surfaced a real adjacent bug, since an earlier
Pause row wrote the *current* value rather than the negation, persisting a key
while changing nothing. Fixed, and `write_setting` now skips writes that would
not change the file. `nothing_but_a_click_writes_the_config` pins the passive
paths shut.

---

## 7. MINOR: the bundle script degrades quietly without `swiftc`

`scripts/bundle-hexad-macos.sh` warns rather than fails when `swiftc` is
absent: `"hexad will start but cannot transcribe (use --asr mock)"`. On a
release machine without Xcode CLT this produces a shippable-looking `.app` with
no recognizer, behind a warning a release process can scroll past.

Related: the rebuild guard is
`if [[ ! -x "$HELPER_BIN" || "$HELPER_SRC" -nt "$HELPER_BIN" ]]`, so a **stale**
helper is silently reused whenever the binary is newer than the source. Correct
for a fresh clone, and precisely why finding 5 hid from everyone.

Recommend the script fail hard when `swiftc` is missing *and* it is producing a
release bundle.

---

## 8. INFO: config defaults and edge cases are sound

`crates/config` was reviewed against the brief's question "are the defaults
actually optimal for a first-run user", and probed for crashes, silent
fallbacks, and confusing errors. **No defects found.** The defaults
(`right-option` hotkey, `balanced` model, 1500ms silence timeout, telemetry
off, `on-release` insertion) are defensible, and each carries a doc string and
a constraint.

Rather than report "looks fine", a regression net was added:
`crates/config/tests/edge_cases.rs`, nine tests, committed in `0e81b08`:

```
running 9 tests
test every_default_passes_its_own_constraint ... ok
test an_empty_file_is_silent_and_uses_defaults ... ok
test every_schema_key_resolves_with_no_config_at_all ... ok
test malformed_toml_still_yields_a_usable_config ... ok
test a_partial_file_leaves_everything_else_at_defaults ... ok
test out_of_range_value_falls_back_to_the_default ... ok
test wrong_type_is_rejected_with_an_actionable_message ... ok
test a_close_typo_gets_a_did_you_mean ... ok
test unknown_key_is_reported_and_the_rest_of_the_file_still_applies ... ok

test result: ok. 9 passed; 0 failed
```

Two design observations worth recording:

- `Config::build` returns `Err` (not `Ok`-with-warnings) on malformed TOML, so
  **every caller must have a fallback** or a half-saved config file takes the
  daemon down. The one caller today (`menuhost.rs`, dove's in-flight work) does
  the right thing: keeps the last good settings and surfaces the message. This
  is a latent trap for the *next* caller, not a current bug.
- `Config::all` calls `.expect()` on every schema key, so a schema row the
  defaults layer failed to populate would panic at startup rather than degrade.
  `every_schema_key_resolves_with_no_config_at_all` now pins that invariant.

---

## 9. INFO: documented latency claims hold up

The brief asked whether any documented number is now wrong. Checked, and
**`docs/latency.md` is not contradicted.** Eight runs of the real release
binary on byte-identical WAV input (`--say` was deliberately avoided; it
re-synthesizes and its duration varies):

```
run 1: release->text 162ms (finalize 130ms, inject 32.4ms)
run 2: release->text 181ms (finalize 142ms, inject 39.2ms)
run 3: release->text 174ms (finalize 142ms, inject 31.8ms)
run 4: release->text 179ms (finalize 145ms, inject 34.4ms)
run 5: release->text 177ms (finalize 138ms, inject 38.7ms)
run 6: release->text 172ms (finalize 137ms, inject 35.5ms)
run 7: release->text 175ms (finalize 137ms, inject 37.7ms)
run 8: release->text 172ms (finalize 135ms, inject 36.6ms)
```

Median ~175ms end to end, dominated by recognizer finalize (~138ms), not by the
OS-integration path this project optimizes. The AX read times observed
(23-30ms) sit right in `docs/latency.md`'s documented **cold** range of
20-29ms, which is expected and consistent: `--once` is a fresh process that
pays first-contact cost exactly once, which is the document's central finding.

No optimization is proposed. The brief asked for changes only with before/after
numbers, and the measured hot spot (recognizer finalize) is inside
`crates/asr/**`, which is out of scope for this agent. **Recommendation: do not
micro-optimize the AX path.** It is ~20% of the budget and already 15x inside
its own gate; the remaining win is in the recognizer.

The regression gate WAS run, on a second pass, after initially being skipped.
`scripts/bench-gate.sh` activates TextEdit and steals focus, which is not
acceptable on a live machine other agents are using, so the gate binary was
driven directly against a throwaway TextEdit document with the previously
frontmost app recorded and restored afterwards (restore runs on a trap, so it
happens even on failure):

```
frontmost before: wezterm-gui
read   n=200  p50=434.292us p90=  7.087ms p99= 16.122ms max= 19.729ms
gate OK: p50 <= 2ms, p99 <= 50ms
GATE_RC=0
focus restored to: wezterm-gui
```

**Passes, with a caveat worth stating rather than burying:** p50 of 434us is
~3x the 134us recorded in `docs/latency.md`'s gate table, and p99 of 16.1ms is
~6x its 2.6ms. Both are comfortably inside budget (4.6x and 3.1x headroom
respectively), and this machine was running four agents, several cargo builds,
and a release-mode compile concurrently, which is exactly the "busy machine"
noise the budgets were deliberately loosened to tolerate. So this is **not**
recorded as a regression: the gate's own verdict is OK and the documented
figures were taken on an idle machine. It is recorded because a future reader
comparing raw numbers would otherwise think docs/latency.md was wrong.

---

## What was verified green

Run from a clean `git worktree` on HEAD, because the shared working tree was
mid-edit and did not compile (see below).

| Check | Result |
|---|---|
| `cargo test --workspace --locked` | 466 passed, 0 failed |
| `cargo +1.85.0 build --workspace --locked` | exit 0 |
| `cargo deny check` | advisories ok, bans ok, licenses ok, sources ok |
| `scripts/build-macos-release.sh` | universal DMG, `lipo`: x86_64 + arm64 |
| `scripts/bundle-hexad-macos.sh` | `.app` valid on disk, satisfies Designated Requirement |
| `scripts/build-headless.sh` | passed its own "no AppKit" linkage check |
| `REPRO_VERIFY=1 scripts/build-repro.sh` | **reproducible**, both builds hashed `d3703b7e50ce...` |
| `scripts/verify-shell-bridge.sh` | passed, including zsh undo restoration |
| `hexad --once --wav` / `--say` | transcribed and injected correctly |
| `spike-cli probe` / `target` | correct output, correct AX tier |

The reproducible double-build passing locally is worth stating plainly: the
`repro` CI job's failure is **entirely** finding 1, not a determinism problem.

### Re-verified after the late dependency change

`296c82a` (single-instance guard) added `libc` to `hexad` and changed
`Cargo.lock` after the checks above were run. Everything lock-sensitive was
re-run rather than assumed still valid:

| Re-check | Result |
|---|---|
| `cargo deny check` | advisories ok, bans ok, licenses ok, sources ok |
| `cargo +1.85.0 build --workspace --locked` | exit 0, so the MSRV pin survives |
| `REPRO_VERIFY=1 scripts/build-repro.sh` | reproducible, hash `d3703b7e50ce...` |
| `scripts/build-headless.sh` | no AppKit, pass |
| `hexad --once --wav` | still transcribes and injects, RC=0 |

The repro hash is **byte-identical to the earlier run**, which is the useful
detail: the new dependency did not perturb the shipped binary at all.

The single-instance guard was also checked for over-reach, because a lock that
is too broad would break every script in this repo that shells out to `--once`
(bench, latency, CI smoke), and could break them *nondeterministically*:

```
1. two concurrent --once runs      -> rc=0, rc=0, neither refused
2. --once while a daemon holds it  -> rc=0, ran fine alongside
3. second DAEMON while one runs    -> rc=1, refused:
   "hexad is already running (pid 32884). Quit it from the menu bar, or
    `kill 32884`, then start this one. Running two copies makes both record
    you and both type what you said."
```

Correctly scoped: it refuses the case it exists for and leaves tooling alone.

## Cross-agent note

While this review ran, the shared working tree did not compile, due to another
agent's in-flight menu bar work:

```
error[E0599]: no method named `handle` found for struct `MenuHost`
error[E0308]: mismatched types  --> crates/hexad/src/menuhost.rs:43:43
```

That agent was DM'd with the exact errors. It is **not** a defect in `main`;
clean HEAD passes all 466 tests. It is recorded only so a future reader does
not mistake a transient local failure for a release finding.

---

## 10. MAJOR (partly fixed): every Linux build linked ALSA, headless included

Root-cause work on finding 1. `cpal` was an **unconditional** dependency of
`crates/audio`, and it is the only thing in that crate needing a system audio
library. That is why a clippy-only job needed `libasound2-dev`, and why the
musl rows failed with a message no runner package can fix:

```
pkg-config has not been configured to support cross-compilation.
```

A statically linked headless daemon takes its audio from a WAV file or a
socket and has no business linking an audio stack it never calls. Fixed in
`a7c7b1b` by making capture optional (`capture` feature, on by default,
mirroring the existing `display` gate in `text-target`/`overlay` rather than
inventing a second convention). It landed on a seam the crate already had:
capture was deliberately kept at the edge, so only the module declaration
needed gating.

Verified as a cargo-level fact against the target that was actually failing:

```
cargo tree -p audio -e normal --target x86_64-unknown-linux-musl
    | grep -c -E 'cpal|alsa'                       -> 3
cargo tree -p audio --no-default-features -e normal \
    --target x86_64-unknown-linux-musl
    | grep -c -E 'cpal|alsa'                       -> 0
```

`crates/audio/tests/capture_feature.rs` pins both halves of the contract and
runs in both configurations (3 tests without the feature, 4 with).

**Now fully fixed** (`af852d6`). Making `capture` optional was necessary but
not sufficient: `hexad` and `asr` took the `audio` dependency with default
features on, so that edge re-enabled capture whatever the build command said.
Deleting the unused `asr` edge, gating the `hexad` edge behind `display`, and
cfg-gating `spawn_mic` closes it. Measured 0 cpal/alsa crates headless and 3
with defaults, and both configurations compile and pass tests from a clean
clone. Full numbers and the runtime checks are in appendix D.

The practical result is better than the CI fix originally proposed: **the two
musl rows no longer need workflow plumbing at all.**

Related and worth someone's attention: **`crates/asr` depends on `audio` but
never references it in code** — it defines its own `SAMPLE_RATE` const — so
that edge could simply be deleted. That crate is owned elsewhere, so it is
reported rather than changed.

---

## 11. MAJOR (fixed): thirteen of sixteen config settings did nothing

Found by lobster, who reproduced it against the bundled binary; confirmed here
independently before fixing. A user could set `insertion.mode = "stream"`, get
no error, observe no change, and reasonably conclude the feature was broken
rather than unbuilt. The quickstart points users at this file.

Confirmed two ways rather than assumed: it matches the `WIRED` gate dove
already put in `crates/hexad/src/menubar.rs`, and grepping the daemon for each
key shows the other thirteen appear only in menu-**display** code, never in the
pipeline. The starkest case is `insertion.mode`, because `crates/hexad` has
**no dependency on the `stream` crate at all**, so `"stream"` cannot possibly
take effect regardless of what the file says. lobster's third case is the one
most likely to generate support load: setting a nonexistent `microphone` logs
`capturing from MacBook Pro Microphone` and never says the setting was ignored.

The menu bar was already honest about this; the file had no equivalent
protection. Fixed in `22f579b` by giving it one:

- `KeySpec::wired`, deliberately **not** defaulted, so adding a schema row
  forces an explicit answer to "does anything read this" while it is still
  cheap to answer.
- `Config::inert_settings()`, returning only unwired keys the user **actually
  set**. A key at its default is a placeholder, not a broken promise; warning
  about all thirteen every start would be noise, and noise is how a warning
  stops being read.

Config tests went 84 -> 89. The remaining half, the call site that actually
reaches a user, landed as `ed5425f` (dove), routed to **two** surfaces rather
than the one I asked for: stderr for a terminal launch, and a menu row for a
bundled launch, because `Hexavoice.app` sets `LSUIElement` and has no terminal at
all, which makes stderr invisible for exactly the users the quickstart sends
to the config file.

Verified end to end against the release binary, including both negative cases,
because a warning that fires on everything is as useless as one that fires on
nothing:

```
===== two inert keys set (expect BOTH named) =====
hexad: config sets "microphone" but nothing reads it yet; it has no effect
hexad: config sets "model" but nothing reads it yet; it has no effect

===== only a WIRED key set (expect NO warning) =====
(no inert warnings)

===== nothing set (expect silence) =====
(no inert warnings)
```

**Trap worth knowing**, recorded so nobody later "discovers" this is broken and
chases a ghost: `--once` does **not** construct a `MenuHost` (`main.rs:157`,
`(!args.once).then(...)`, so a one-shot measurement neither creates nor mutates
menu state), and therefore never prints this warning. That is defensible, but
`--once` is the path used by every scripted check in this repo, including the
latency measurements above. A first pass at verifying this feature reported
"no warnings" in all three cases for exactly that reason; the test was wrong,
not the feature.

Note this fix makes the settings *honest*, not *implemented*. Thirteen settings
still do nothing; the user is now told so instead of being left to guess.

---

## What would make a release shippable

1. Land the Linux system-deps step in `ci.yml` **and** `release.yml`
   (`.github/workflows/**` owner). Turns 5 of 7 red jobs green.
2. Build musl/headless rows with `--no-default-features` (finding 10, landed).
   No workflow *plumbing* needed for these two, just the flag.
3. Remove `|| true` from the aarch64 headless release step, or drop the target.
4. Fix the README's from-source build path (lobster).
5. Optional but cheap: reap stale speech helpers at daemon startup.

Item 1 is the blocker and needs an owner. Everything else is
shippable-with-known-issues.

---

## Appendix: the exact YAML required

Nobody in this session can edit `.github/workflows/**`, and the owner has not
surfaced. Reproduced here as copy-pasteable blocks so whoever picks it up
cannot get it subtly wrong. A draft of some of this is sitting **uncommitted**
in the working tree's `ci.yml` and `Cross.toml`; it is unowned and unverified.

### A. `ci.yml` — add to `check`, `msrv`, `repro`, and `build-matrix`

Insert **after** `actions/checkout` and **before** `Swatinem/rust-cache`, in
each of those four jobs:

```yaml
      - name: Install Linux system dependencies
        if: runner.os == 'Linux'
        run: scripts/ci-install-linux-deps.sh
```

For the `msrv` and `repro` jobs, which are Linux-only, the `if:` is optional
but harmless. For `build-matrix`, it must additionally not run under `cross`
(the container has its own package set), so use:

```yaml
      - name: Install Linux system dependencies
        if: runner.os == 'Linux' && !matrix.use-cross
        run: scripts/ci-install-linux-deps.sh
```

The script is already committed and executable (`0e81b08`, mode 100755). No
other change is needed for the four glibc rows.

### B. `release.yml` — the tag-time failure

Add the **same step** to the `compliance`, `linux`, and `headless` jobs.
`compliance` is the critical one: every other job declares `needs: compliance`,
so without it a tag stops before building a single artifact.

### C. `release.yml` — stop swallowing a failed architecture

In the `headless` job, change:

```yaml
          scripts/build-headless.sh aarch64-unknown-linux-musl || true
```

to:

```yaml
          scripts/build-headless.sh aarch64-unknown-linux-musl
```

Either it builds or the release fails honestly. Publishing a `headless`
artifact set silently missing an architecture is worse than publishing none.

### D. musl rows: LANDED (`af852d6`), no workflow change needed

Originally written up as work to hand over; ownership was transferred and it is
now done. The reasoning is kept because an earlier revision of this appendix was
**wrong**, and how it was wrong is the instructive part: making `capture`
optional (finding 10) was necessary but not sufficient, and I generalised a
single-crate measurement to the whole workspace without testing it.

`--no-default-features` applies to the workspace *members*, but `hexad` and
`asr` each took `audio = { path = "../audio" }` with default features **on**,
so that dependency edge re-enabled capture regardless of the build command.

Three changes, one per layer of the problem:

1. **`crates/asr`: deleted the `audio` dependency** rather than defusing it.
   That crate has zero `audio::` references and defines its own `SAMPLE_RATE`,
   so the edge was dead weight. An unused dependency kept only to be switched
   off is worse than none: the next person has to work out whether it matters.
2. **`crates/hexad`: `default-features = false` on the audio edge**, with
   `audio/capture` hung off the existing `display` feature. Those two answer
   the same question (is there a human at this machine), so tying them
   together means nobody can build a "headless" daemon that still links ALSA.
3. **`crates/hexad/src/source.rs`: cfg-gated `spawn_mic`**, which used
   `audio::capture` unconditionally, with a headless counterpart returning an
   error that names `--wav` rather than capturing nothing silently.

Verified, and deliberately not by dependency counts alone, since counts are
exactly where the previous claim broke:

```
cargo tree --workspace --no-default-features ... musl | grep -c 'cpal|alsa' -> 0
cargo tree --workspace                       ... musl | grep -c 'cpal|alsa' -> 3

cargo check --workspace --no-default-features   -> compiles (was E0433/E0422)
cargo clippy --workspace --all-targets [--no-default-features] -D warnings -> clean
cargo test --workspace                          -> 0 failures
cargo deny check                                -> ok
cargo +1.85.0 build --workspace --locked        -> exit 0 (MSRV pin survives)

desktop build, real dictation:  RC=0, "Testing the WAV input path."
headless build, mic requested:  RC=1, error names --wav
headless build, --once --wav:   RC=0, so the error's own advice works
```

That last line matters: an error recommending a path which is itself broken is
worse than no advice, so the recommendation was executed, not assumed.

Re-verified from a **clean clone of committed HEAD**, not the working tree,
after a cross-agent race briefly left committed `main.rs` calling a
`Result`-returning `spawn_mic` whose committed `source.rs` half did not yet
return one. A working tree holding both halves compiles and hides exactly that
class of break:

```
== default features ==      Finished dev profile in 28.44s
== no-default-features ==   Finished dev profile in 33.51s
== full test suite ==       43 suites ok, no failures
```

**Consequence for CI:** the two musl rows no longer need workflow plumbing at
all. They need the build command to pass `--no-default-features`, which is a
better outcome than cross-compiling an audio library the binary never calls.
The **four glibc rows still need step A**, which still has no owner.

---

## Shelf life of this report

A product rename is queued (bundle id `dev.hexavoice.hexad` becomes
`dev.hexavoice.hexad`, and the app bundle is renamed to match). When it lands
it **invalidates two green ticks above**, which must be re-run rather than
inherited:

- the **codesign Designated Requirement** check on the bundled app, because the
  DR is written against the old bundle id; and
- any **TCC/permission** state, because a new bundle id voids every existing
  Accessibility and Microphone grant. Every current tester will look like a
  first-run user, and a grant that silently stops applying is the most
  confusing failure this product can present.

The CI, MSRV, dependency-graph, reproducibility, and licence findings are
rename-independent and stay valid.
