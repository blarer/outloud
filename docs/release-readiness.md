# Release readiness

**Verdict: a release would NOT ship today.** One blocker, four majors.

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
| 1 | **BLOCKER** | Linux CI+release cannot build: `alsa-sys` has no system deps | Diagnosed; workflow fix required, owner needed |
| 2 | **MAJOR** | `release.yml` would fail at tag time for the same reason | Diagnosed; workflow fix required, owner needed |
| 3 | **MAJOR** | MSRV job red: dep required rustc 1.86 vs documented 1.85 | **FIXED** (`5bd83fd`), verified on real CI |
| 4 | **MAJOR** | `scripts/ci-install-linux-deps.sh` untracked and non-executable | **FIXED** (`0e81b08`), committed mode 100755 |
| 5 | **MAJOR** | Build from source per README yields a daemon that cannot transcribe | Found by lobster, confirmed here; owner: lobster |
| 6 | MINOR | Stale `aqua-speech-helper` orphan; trigger not reproducible | Reported, mechanism refuted |
| 7 | MINOR | `bundle-aquad-macos.sh` warns (not fails) when `swiftc` is absent | Reported |
| 8 | INFO | Config defaults and edge cases behave correctly | Verified, regression net added (`0e81b08`) |
| 9 | INFO | Latency claims in `docs/latency.md` hold up | Verified, 8-run measurement |

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

## 1. BLOCKER: no Linux job can build, because `alsa-sys` has no system deps

**Evidence.** All six red Linux jobs from run `30361756410`, first real error
line from each log:

```
check (ubuntu-24.04)                        Package alsa was not found in the pkg-config search path.
repro                                       Package alsa was not found in the pkg-config search path.
build-matrix x86_64-unknown-linux-gnu       Package alsa was not found in the pkg-config search path.
build-matrix aarch64-unknown-linux-gnu      Package alsa was not found in the pkg-config search path.
build-matrix aarch64-unknown-linux-musl     Package alsa was not found in the pkg-config search path.
build-matrix x86_64-unknown-linux-musl      pkg-config has not been configured to support cross-compilation.
```

**Why it hits even non-audio jobs.** `cpal` is an unconditional dependency of
`crates/audio` (`crates/audio/Cargo.toml:28`), not gated behind a feature or a
`cfg`. `alsa-sys`'s build script runs `pkg-config` on every glibc Linux build,
so `cargo clippy` and `cargo fmt`-adjacent jobs that never execute a line of
audio code still fail. That is why this single missing package takes out
`check`, `repro`, and the whole Linux build matrix at once.

**Two distinct variants, and the second is not yet solved.**

- *glibc targets* need `libasound2-dev` on the runner. The drafted
  `scripts/ci-install-linux-deps.sh` handles this.
- *musl targets* fail differently: `pkg-config has not been configured to
  support cross-compilation`. The drafted script explicitly says it does not
  cover musl ("this covers native glibc builds only"). **Nothing in the tree
  currently fixes the two musl rows.** Installing `libasound2-dev` will turn
  four jobs green and leave `x86_64-unknown-linux-musl` and
  `aarch64-unknown-linux-musl` red.

**Required action, and by whom.** This agent may not edit
`.github/workflows/**`. The owner of that file must add, to the `check`,
`msrv`, `repro`, and `build-matrix` (non-cross Linux) jobs:

```yaml
      - name: Install Linux system dependencies
        if: runner.os == 'Linux'
        run: scripts/ci-install-linux-deps.sh
```

The script is now committed and executable (finding 4), so this is the only
missing piece for the glibc rows. A draft of exactly this change is already
sitting **uncommitted** in the working tree's `.github/workflows/ci.yml` and
`Cross.toml`; someone needs to own and land it.

For the musl rows, the honest options are: install a musl-targeted ALSA and
set `PKG_CONFIG_ALLOW_CROSS=1` with a musl `PKG_CONFIG_PATH`; or make `cpal`
an optional dependency so headless/musl builds exclude the audio backend
entirely. The second is architecturally cleaner (a static headless daemon has
no business linking ALSA) but is a change to `crates/audio`, so it needs that
crate's owner. **Recommend deciding this deliberately rather than pinning more
CI plumbing on top.**

---

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
aquad: state model-loading
Error: recognizer failed to load (aqua-speech-helper not found; build it with
`swiftc -O crates/asr/helper/transcriber.swift -o aqua-speech-helper` or set
AQUA_SPEECH_HELPER) -> build the speech helper (see crates/asr/helper) or run
with --asr mock
RC=1
```

**Severity is MAJOR, not blocker, because the packaged path is fine.** On the
same fresh clone, `scripts/bundle-aquad-macos.sh` compiles the helper itself
and the resulting `.app` transcribes correctly:

```
==> Building the speech helper            <- swiftc ran, from a clean tree
-rwxr-xr-x 2631152 Aqua
-rwxr-xr-x   98512 aqua-speech-helper     <- shipped inside the .app

aquad: recognizer ready: apple-speechtranscriber
aquad: e2e: release->text 200ms (finalize 163ms, inject 37.4ms) | "The rain in Spain."
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

An `aqua-speech-helper` was found alive with `PPID=1` for 7h50m, holding an OS
speech session after its parent died:

```
  PID  PPID STARTED                       ELAPSED COMMAND
45677     1 Tue Jul 28 01:07:42 2026     07:50:15 .../aqua-speech-helper
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
trigger: **reap stale helpers at daemon startup.** That can live on the aquad
side rather than in the off-limits `crates/asr`.

---

## 7. MINOR: the bundle script degrades quietly without `swiftc`

`scripts/bundle-aquad-macos.sh` warns rather than fails when `swiftc` is
absent: `"aquad will start but cannot transcribe (use --asr mock)"`. On a
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

`scripts/bench-gate.sh` was deliberately **not** run: it activates TextEdit and
steals focus on a live machine other agents are working on. That is a real gap
in this report, not a claim of coverage.

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
| `scripts/bundle-aquad-macos.sh` | `.app` valid on disk, satisfies Designated Requirement |
| `scripts/build-headless.sh` | passed its own "no AppKit" linkage check |
| `REPRO_VERIFY=1 scripts/build-repro.sh` | **reproducible**, both builds hashed `d3703b7e50ce...` |
| `scripts/verify-shell-bridge.sh` | passed, including zsh undo restoration |
| `aquad --once --wav` / `--say` | transcribed and injected correctly |
| `spike-cli probe` / `target` | correct output, correct AX tier |

The reproducible double-build passing locally is worth stating plainly: the
`repro` CI job's failure is **entirely** finding 1, not a determinism problem.

## Cross-agent note

While this review ran, the shared working tree did not compile, due to another
agent's in-flight menu bar work:

```
error[E0599]: no method named `handle` found for struct `MenuHost`
error[E0308]: mismatched types  --> crates/aquad/src/menuhost.rs:43:43
```

That agent was DM'd with the exact errors. It is **not** a defect in `main`;
clean HEAD passes all 466 tests. It is recorded only so a future reader does
not mistake a transient local failure for a release finding.

## What would make a release shippable

1. Land the Linux system-deps step in `ci.yml` **and** `release.yml`
   (`.github/workflows/**` owner). Turns 5 of 7 red jobs green.
2. Decide the musl/ALSA story: cross-pkg-config, or make `cpal` optional
   (`crates/audio` owner). Turns the last 2 green.
3. Remove `|| true` from the aarch64 headless release step, or drop the target.
4. Fix the README's from-source build path (lobster).
5. Optional but cheap: reap stale speech helpers at daemon startup.

Items 1 and 2 are the blocker. Everything else is shippable-with-known-issues.
