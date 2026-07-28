# Pre-release audit

**Scope:** read-only audit of this repository ahead of making it public.
**Audited at:** commit `567fb46` ("fix(bundle): stop claiming the TCC grant survives a rebuild, and clear it"), 2026-07-28.
**Platform:** macOS 26.5.2 (`sw_vers` → `ProductVersion: 26.5.2`), Apple silicon, `rustc 1.95.0`.

**Method.** Every claim below is backed by quoted command output. The working
tree was dirty with an in-flight rename during this audit, so analysis ran
against two stable copies rather than the live tree: a snapshot of the working
tree (`/tmp/audit/snap1`) and a clean `git archive HEAD` extraction
(`/tmp/audit/headlesstest`). Where a claim could not be reproduced, it is
labelled **NOT REPRODUCED** rather than repeated as fact.

**Verdict:** **DO-NOT-SHIP** as written. Four findings must be fixed first; the
shortest path is at the end. None is architectural, and the total fix is small.

---

## Severity 1 (blocker): the headline claim is not accurate

`README.md:3`:

```
A fully local, open-source alternative to [Aqua Voice](https://withaqua.com):
```

Both non-Apple recognizer backends are stubs that return an error:

```
$ sed -n '58,66p' crates/asr/src/backends/whisper_cpp.rs
    fn finalize(&mut self) -> anyhow::Result<Transcript> {
        self.buffered.clear();
        anyhow::bail!(
            "whisper.cpp backend not yet implemented: needs whisper-rs integration \
             (see crates/asr/src/backends/whisper_cpp.rs for the integration plan)"
        )
    }

$ sed -n '60,66p' crates/asr/src/backends/parakeet.rs
    fn finalize(&mut self) -> anyhow::Result<Transcript> {
        self.buffered.clear();
        anyhow::bail!(
            "parakeet backend not yet implemented: needs ONNX Runtime integration \
             (see crates/asr/src/backends/parakeet.rs for the integration plan)"
        )
    }
```

The only non-mock recognizer that works is Apple's, confirmed by running the
bundled binary from a clean checkout:

```
$ ./dist/Hexavoice.app/Contents/MacOS/Hexavoice --once --say "hello from a local dictation daemon" --no-overlay
hexad: recognizer ready: apple-speechtranscriber
hexad: e2e: release->text 355ms (finalize 306ms, inject 48.8ms) via synthetic-keys | "Hello from a local dictation demon."
```

`crates/asr/helper/transcriber.swift:22` imports Apple's `Speech` framework and
line 19 documents `(macOS 26+ SDK)`. So:

- **"Local" is defensible.** Nothing leaves the machine; `SpeechTranscriber` is
  on-device, and I found no network egress in the dictation path.
- **"Open-source" is not accurate as applied to the product.** The MIT licence
  covers this repository's code, but the component doing the actual recognition
  is Apple-proprietary with closed weights. A reader of line 3 reasonably
  concludes they can read, fork, and run the recognizer. They cannot.
- **"Alternative to Aqua Voice" overstates portability.** On macOS 15 and below
  the app launches and shows a menu bar icon but cannot transcribe a word:
  `scripts/bundle-outloud-macos.sh:85` sets `LSMinimumSystemVersion` to `13.0`,
  so macOS 13-25 users can install and run a product that cannot do its one job.
  On Windows and Linux there is no working recognizer at all, even though the
  hotkey and text-injection backends exist there.

The README does disclose this further down (`README.md:52-53`, and the known
limitations table at `README.md:330`), which is to its credit. The problem is
specifically that **line 3 is the line that gets quoted, screenshotted, and put
on Hacker News**, and it contradicts line 330 of the same file. A disclosure 300
lines below a headline does not repair the headline.

### Recommended action

Replace `README.md:3` with wording that is true at a glance:

```
Local-first dictation and edit-by-voice for macOS. MIT-licensed, and your
audio never leaves the machine. Recognition today uses Apple's on-device
SpeechTranscriber (macOS 26+); fully open-weight backends (Parakeet, whisper.cpp)
are scaffolded but not yet implemented.
```

And amend `README.md:52-53`, which currently reads:

```
Recognition is Apple's on-device `SpeechTranscriber` (macOS 26+), which needs no
model download. Parakeet TDT and whisper.cpp backends are stubbed with
documented model URLs for platforms without it.
```

"stubbed" understates it. Suggested: *"Parakeet TDT and whisper.cpp are
scaffolded only: the trait implementations exist and both return `not yet
implemented` from `finalize`. Until one of them lands, macOS 26+ is the only
platform that can transcribe, and the recognizer itself is closed-source."*

Also worth a one-line addition to the `## Install` section (`README.md:86`),
which currently says `Requires macOS 13 or newer (26+ for the zero-install
recognizer)`. On 13-25 there is no recognizer at all, not merely a
non-zero-install one. Suggested: *"Requires macOS 26+ to transcribe. It builds
and runs on 13+, but on 13-25 no recognizer is available and dictation will not
work."*

---

## Severity 1 (blocker): `doctor`'s default output leaks the user's home path, and the issue template asks strangers to paste it

`.github/ISSUE_TEMPLATE/bug-report.yml` makes doctor output a **required** field:

```yaml
  - type: textarea
    id: doctor
    attributes:
      label: Doctor output
      description: >-
        Paste the full output of `./scripts/doctor.sh`. Reports without this
        will be sent back for it
    validations:
      required: true
```

`crates/diag/src/redact.rs` is genuinely good and its redaction works. I tested
it rather than trusting the unit tests:

```
$ cargo run -q -p diag --bin doctor -- --report   # redacted section only
[WARN] model-files: no recognizer model in ~/.aqua-oss/models
(transcripts, clipboard contents, window titles, and file paths are redacted by construction)

$ ... | grep -c blare    # username occurrences in the redacted section
0
```

**But the redacted bundle is opt-in behind `--report`, and the issue template
does not ask for it.** The default run leaks:

```
$ cargo run -q -p diag --bin doctor > /tmp/audit/doctor_default.txt; grep -n "blare" /tmp/audit/doctor_default.txt
23:[WARN] model-files                no recognizer model in /Users/blare/.aqua-oss/models
```

`crates/diag/src/bin/doctor.rs:28` gates the redaction on the flag:

```rust
let want_report = args.iter().any(|a| a == "--report");
```

and `scripts/doctor.sh:73` passes through `--args "$@"`, so the wrapper the
template names inherits the unredacted default. The username is low-severity on
its own, but the mechanism is the problem: **the redaction path exists, is
correct, and is not the one users are told to run.** As more checks add `detail`
lines over time, whatever they capture will go straight into public GitHub
issues on a tool that by design can read the focused text field of any
application.

### Recommended action

Two one-line changes, both outside this audit's write scope:

1. `.github/ISSUE_TEMPLATE/bug-report.yml`: change the two documented
   invocations from `./scripts/doctor.sh` to `./scripts/doctor.sh --report`,
   and ask for the section under `----- pasteable redacted report -----`.
2. Better, in `crates/diag/src/bin/doctor.rs`: make redaction the default and
   put the unredacted form behind `--raw`. A privacy default that requires a
   flag is not a privacy default. This also removes the two-surfaces-must-agree
   coupling between a YAML template and a Rust flag.

I did **not** find evidence that transcripts reach a log file. `redact.rs`
policy is honoured in `crates/diag/src/replay.rs:263-264`, and I found no
`eprintln!` in the daemon interpolating transcript text into a persisted log.
The one place transcript text is printed is stdout (see next finding).

---

## Severity 2 (major): the daemon prints the verbatim transcript to stdout

`crates/outloud/src/pipeline.rs:58-73`, `UtteranceReport::render()`, embeds the
full transcript:

```rust
"e2e: release->text {:.0}ms (finalize {:.0}ms, inject {:.1}ms) via {} | \"{}\"",
...
self.transcript,
```

Reproduced above: the run printed `| "Hello from a local dictation demon."`.
`crates/outloud/src/pipeline.rs:206` does the same for the *selection*, which is
text read out of another application's focused field:

```rust
eprintln!("outloud: edit mode on selection: \"{selected}\"");
```

This is not currently written to a file by the daemon (`grep -rn "SPIKE_LOG"
crates/outloud/src/` returns nothing), so today it lands on the terminal of
whoever launched it, and on nothing at all under `open -a`. That makes it
**major, not critical**. It becomes critical the moment anyone adds file
logging, a log-collection step, or a "paste your daemon output" issue field,
because the content is precisely what `redact.rs` exists to keep out of reports.

### Recommended action

Route both through `diag::redact::redact_content` unless an explicit
`--verbose`/debug flag is set, exactly as `replay.rs:264` already does. The
latency numbers, which are the actual diagnostic value of that line, survive
redaction unchanged.

---

## Severity 2 (major): `dropped_chunks` is cumulative but reported as per-utterance

This is the "second time differs from the first" class the brief asked for, and
it is a real instance, proven rather than argued.

`crates/outloud/src/recognize.rs:41-73` keeps one `AtomicU64` for the lifetime of
the `AudioFeed`, incremented on every dropped chunk. Nothing ever resets it:

```
$ grep -rn "store(0\|dropped.*= 0" crates/outloud/src/recognize.rs crates/outloud/src/pipeline.rs
(no matches)
```

`crates/outloud/src/pipeline.rs:440` reads that lifetime total into a
*per-utterance* report:

```rust
        dropped_chunks: feed.dropped_chunks(),
```

and `pipeline.rs:66-67` presents it as this utterance's loss:

```rust
            if self.dropped_chunks > 0 {
                format!(" | DROPPED {} audio chunks", self.dropped_chunks)
```

The existing test suite cannot catch this because it only ever runs one
utterance: `crates/outloud/src/recognize.rs:205` asserts
`feed.dropped_chunks() == 0` after a single finalize, which is true and
uninformative. I wrote a two-utterance test (in the `/tmp` copy only, never in
the repo) driving a deliberately slow recognizer to force drops on utterance 1,
then sending a single chunk on utterance 2:

```
$ cargo test -p outloud --lib audit_second_utterance -- --nocapture
utterance 1 reported dropped_chunks = 2614
utterance 2 reported dropped_chunks = 2614
test recognize::audit_second_utterance::dropped_chunks_is_cumulative_not_per_utterance ... ok
```

Utterance 2 dropped nothing and reported 2614 drops. The user-visible effect: a
single burst of audio pressure early in a session makes **every subsequent
utterance for the rest of that session** print `DROPPED N audio chunks`, so a
healthy dictation looks lossy forever. The comment on `dropped_chunks()` at
`recognize.rs:69-70` says it exists "for honest diagnostics in the end-of-utterance
report", which is exactly what it fails to deliver.

### Recommended action

Snapshot-and-difference at commit time, or reset the counter in the `Finalize`
arm of the worker loop (`recognize.rs:148`). Then add a two-utterance test.
More broadly: **the repo has no test anywhere that runs two utterances through
`recognize::spawn`.** `grep -rn "finalize()" crates/outloud/src/recognize.rs
tests/` shows finalize called once per test. Given that a second-utterance bug
already escaped the suite today, one two-utterance integration test is the
highest-value test this repo could add.

Related, same file: the doc comment at `crates/asr/src/lib.rs:60-62` still
asserts the contract the trait immediately below it refutes —

```
/// - `finalize` consumes the utterance state and returns the transcript to
///   commit. After `finalize`, the recognizer is reset and reusable for the
///   next utterance.
```

whereas `lib.rs:79-91` documents at length that this was false for the Apple
backend and that callers must query `reusable()`. A stale doc comment directly
above the corrected one will mislead the next contributor. Recommend deleting
those two lines.

---

## Severity 2 (major): the CI headless smoke test asserts a filename no script produces

`.github/workflows/ci.yml:200`:

```yaml
          env -u DISPLAY -u WAYLAND_DISPLAY dist/headless/aqua-spiked-x86_64-unknown-linux-musl dry-run "change hello to goodbye"
```

`scripts/build-headless.sh:92` writes a different name:

```bash
cp "$BIN" "$OUT_DIR/hexavoice-spiked${TARGET:+-$TARGET}"
```

Reproduced from a clean `git archive HEAD` checkout, so the working tree's stale
`dist/` cannot mask it:

```
$ bash scripts/build-headless.sh
Built: dist/headless/hexavoice-spiked
$ ls dist/headless/aqua-spiked
ls: cannot access 'dist/headless/aqua-spiked': No such file or directory
```

The `headless` job therefore fails on every run, at the smoke-test step, after
the build succeeds. Note this is a **pre-existing** mismatch, not rename
fallout: the same skew exists at `HEAD` between `ci.yml` (`aqua-spiked`) and
`build-headless.sh` (`hexavoice-spiked`), i.e. two *different* stale names.

The same class of skew exists in `release.yml`, where it is more expensive
because it only fires at tag time:

| Workflow line | Path asserted | Script | Path produced |
|---|---|---|---|
| `release.yml:136` | `dist/windows/$T/aqua-spike.exe` | `build-windows.sh:58` | `outloud-spike.exe` |
| `release.yml:173` | `dist/linux/$T/aqua-spike` | `build-linux.sh:57` | `outloud-spike` |
| `ci.yml:200` | `dist/headless/aqua-spiked-…` | `build-headless.sh:92` | `outloud-spiked-…` |

(Script-side names above are from the current working tree mid-rename; at `HEAD`
they read `hexavoice-*`. Either way they do not match the workflow.)

### Recommended action

Have the scripts print the artifact path and the workflows consume that, rather
than both sides hardcoding a product name that changes. A `$GITHUB_OUTPUT`
variable emitted by each build script removes this entire failure class
permanently, which matters given the product has now been renamed twice.

---

## Severity 3: config honesty, per key

The `wired` flag is an unusually honest piece of engineering and I want to be
clear that the design is right. `crates/config/src/schema.rs:161` forces an
explicit answer per key, `crates/outloud/src/menubar.rs:609-626` (`only_implemented_settings_are_offered`)
gates the menu on it with a test, and `crates/outloud/src/menuhost.rs:120-128` warns at startup
for keys the user actually set. Verified the warning is live code, not just
tested:

```
$ grep -rn "inert_settings" --include=*.rs .
./crates/outloud/src/menuhost.rs:120:                for spec in cfg.inert_settings() {
```

Counts confirmed: 3 wired, 13 not.

```
$ grep -c "wired: true" crates/config/src/schema.rs   → 3
$ grep -c "wired: false" crates/config/src/schema.rs  → 13
```

**Where a user is still misled.** `Config::inert_settings()`
(`layers.rs:258-267`) deliberately warns only for keys set outside the defaults
layer. That is a good decision for noise, but it means the first-run experience
is silent. The generated starter file lists all 16 keys with **no marker**
distinguishing the 13 inert ones. Dumped from a real run:

```
$ cargo test -p config --test audit_probe -- --nocapture   # calls ensure_user_config()
# Keep a local plain-text transcription history.
# history.enabled = true

# Anonymous usage reporting. Off by default, forever.
# telemetry.enabled = false

# Recognition language code (e.g. "en"), or "auto" to detect.
# language = "auto"
```

Per-key assessment of the 13 inert settings:

| Key | Doc string implies | Reality | Risk |
|---|---|---|---|
| `history.enabled` | "Keep a local plain-text transcription history", default **true** | Nothing writes history. `grep -rn "history" crates/` finds no writer | **Highest.** A privacy-relevant claim, shown as ON by default. A user reads this as "my dictation is being written to disk" and may go looking for a file to delete, or conversely may rely on a history that does not exist |
| `telemetry.enabled` | "Anonymous usage reporting. Off by default, forever" | No telemetry code exists | Benign but misleading in the opposite direction: it advertises a capability the project says it will never build. Reads as future-proofing for something users would object to |
| `microphone` | Selects input device | Ignored; system default always used | Real. `menuhost.rs:118` names exactly this: a user setting `microphone = "no-such-device"` gets a daemon recording from a different device |
| `language` | Recognition language | Ignored | Real for non-English users, who will conclude the tool is English-only |
| `model` | fast/balanced/accurate | Ignored | Moderate |
| `insertion.mode` | `stream` types words as spoken | Ignored. `schema.rs:412` is candid: "outloud has no dependency on the stream crate at all" | Moderate; README already discloses streaming is unwired |
| `insertion.paste-fallback` | Forces clipboard insertion | Ignored | Moderate: this is the documented workaround for broken-AX apps, so the setting a struggling user reaches for first is the one that does nothing |
| `formatting.casing`, `.smart-quotes`, `.trailing-punctuation` | Text formatting | Ignored | Low individually; the profile examples in `docs/configuration.md:100-118` show all three being set per-app, which implies a working feature set |
| `silence-timeout-ms` | Latch-mode timeout | Ignored | Low |
| `vocabulary.sets` | Vocabulary sets | Ignored | Low |
| `launch-at-login` | Start at login | Ignored | Low |

`docs/configuration.md:36-40` does disclose the situation in prose and deserves
credit for it. But the options table at `docs/configuration.md:73-90` then lists
all 16 keys in one undifferentiated table with an "Effect" column describing
effects that do not happen. Prose ten lines above a table does not survive
someone scrolling to the table.

### Recommended action

1. Add a `Status` column to the `docs/configuration.md` options table with
   `wired`/`not yet` per row, generated from `schema()` so it cannot go stale.
   The schema already carries the fact; the doc just is not using it.
2. Have `starter_file()` (`crates/config/src/paths.rs:88`) emit `# (not
   implemented yet; this setting currently has no effect)` above each key where
   `!spec.wired`. This is a four-line change and closes the first-run gap.
3. Consider deleting `history.enabled` and `telemetry.enabled` from the schema
   until they exist. Unlike the others, these two make privacy claims, and a
   privacy claim that is neither true nor false is the worst kind to ship.

---

## Severity 3: documented CLI surface does not exist

`docs/configuration.md` references commands the binary does not implement:

```
docs/configuration.md:6:   The GUI settings window and `outloud set` are both convenience views over
docs/configuration.md:52:  Every resolved value knows which layer set it, and `outloud status --json`
docs/configuration.md:214:  `outloud set --list` when nothing is.
```

The complete argument parser is `crates/outloud/src/main.rs:60-93`. It accepts
`--once`, `--wav`, `--say`, `--asr`, `--chord`, `--no-overlay`, `--realtime`,
`--version`, `--help` and nothing else:

```rust
            other => anyhow::bail!("unknown argument {other} (try --help)"),
```

There is no subcommand dispatch, no `set`, no `status`, and no `--json` anywhere
in the crate. There is also no "GUI settings window": `README.md:57` correctly
lists "a settings UI" under *not yet built*, and `menubar.rs:411-418` explains
at length why the menu deliberately offers only three settings. So
`docs/configuration.md:6` contradicts both the code and the README.

A newcomer following the configuration doc will run `outloud status --json`,
get `unknown argument status`, and reasonably conclude the build is broken.

### Recommended action

Reword the three lines to the future tense or delete them. `layers.rs:237` and
`:270` carry the same phantom command in doc comments and should follow.

---

## Notes on prior coverage, including where I disagree

`docs/release-readiness.md` and `docs/beta-readiness.md` are substantial and
largely accurate. Spot-checking rather than repeating, two corrections:

**1. I disagree with `docs/release-readiness.md:127-133`,** which states that at
tag time the `release.yml` `compliance` job "runs `scripts/ci-check.sh` on
`ubuntu-24.04` and dies on `alsa-sys`", stopping the whole pipeline. That was
true when written, but `8c2b50c` ("fix(ci): self-provision Linux deps from the
scripts, not the workflow") made the script install its own dependencies, and
that fix is committed at `HEAD`:

```
$ git show HEAD:scripts/ci-check.sh | grep -n "ci-install-linux-deps"
23:scripts/ci-install-linux-deps.sh
```

`ci-install-linux-deps.sh` installs `libasound2-dev` and is explicitly designed
to be safe to call from scripts (its header, lines 22-28, says exactly this).
So `release.yml`'s `compliance` job self-provisions and the "entire release
pipeline stops before building a single artifact" conclusion no longer holds.
The doc's own line 129 hedges with "as CI's `check (ubuntu-24.04)` does now",
which is stale for the same reason. **Recommend updating that section**, because
a readiness doc that overstates a blocker gets trusted less on the blockers that
are real.

**2. The `release.yml` `linux` job does not need ALSA at all,** contrary to the
same section's line 134. It builds `spike-cli`, which has no path to `cpal`:

```
$ cargo tree -p spike-cli --target x86_64-unknown-linux-gnu -e normal | grep -E "alsa|cpal"
(no matches; exit 1)

$ cargo tree -p outloud --target x86_64-unknown-linux-musl -e normal | grep -E "alsa|cpal"
│   ├── cpal v0.18.1
│   │   ├── alsa v0.11.0
│   │   │   ├── alsa-sys v0.4.0
```

So the ALSA exposure is real for `outloud` and not for the release Linux job.
Note the second command also shows that a `musl` target still resolves `cpal`
under default features, which is why `crates/outloud/Cargo.toml:32`'s
`default-features = false` on the `audio` edge matters; that guard is correct
and should not be removed.

**Confirmed as still true:** the `|| true` on the aarch64 headless build,
`release.yml`:

```yaml
          cargo install cross --locked
          scripts/build-headless.sh aarch64-unknown-linux-musl || true
```

A failure there is swallowed and the job uploads a `headless` artifact set
silently missing an architecture. `docs/release-readiness.md:149-153` is right
about this and it remains unfixed. Given the filename mismatch documented above
also lives in the headless path, this `|| true` is currently hiding a second
bug on top of the one it was flagged for.

**NOT REPRODUCED.** I could not independently verify the four glibc CI row
failures on a real runner. Local cross-compilation dies earlier, on a missing C
toolchain, before reaching `pkg-config`:

```
$ cargo build -p outloud --locked --target x86_64-unknown-linux-gnu
error occurred in cc-rs: failed to find tool "x86_64-linux-gnu-gcc": No such file or directory
```

That is a limitation of this machine, not evidence either way. The dependency
edge shown above makes the mechanism plausible, and `ci-install-linux-deps.sh`
is the right fix regardless, but I am not confirming the failure list from
`docs/release-readiness.md:67-71` as reproduced.

---

## Gate results (clean checkout of `HEAD`, macOS 26.5.2)

Run in `/tmp/audit/headlesstest`, extracted with `git archive HEAD`, never in the
working tree.

| Gate | Result |
|---|---|
| `cargo build --workspace` | **pass** (`Finished dev profile`, exit 0) |
| `cargo fmt --all -- --check` | **pass** (no output, exit 0) |
| `cargo clippy --workspace --all-targets -- -D warnings` | **pass** (`Finished`, `CLIPPY_EXIT=0`) |
| `cargo test --workspace` | **pass** — `TOTAL passed 549 failed 0 ignored 4` |
| `cargo check -p overlay -p text-target --no-default-features` | **pass** (`NDF_EXIT=0`) |
| `bash scripts/bundle-outloud-macos.sh` | **pass**, produces a working bundle |
| `--once --say` end-to-end dictation | **pass**, transcribed and injected in 355ms |
| `scripts/build-headless.sh` + `ci.yml:200` smoke assertion | **FAIL** (see Severity 2) |

Minor: `README.md:376` says `cargo test --workspace  # 400 tests`. The actual
count is 549 passed, 4 ignored. An understatement rather than an overclaim, but
worth correcting since it is a checkable number.

Minor: `README.md:36-37` advertises `131-215ms` end to end. My one measured run
was **355ms** (`finalize 306ms, inject 48.8ms`) via the synthetic-keys transport
into a terminal, which the README itself identifies as the slow end of the
range. This is a single sample on a busy machine and is **not** presented as
contradicting the benchmark; a reader should know the quoted range is the fast
transport and best case. Recommend the README say which transport produced
`131ms`, which `README.md:38-40` nearly does already.

---

## What I checked and found healthy

Stated so the findings above are read in proportion.

- **Redaction logic is correct.** `redact_content`, `redact_title`,
  `redact_path`, and `scrub_free_text` all behave as documented; the `--report`
  bundle contained zero occurrences of the username. The problem is reachability,
  not correctness.
- **The `wired` flag mechanism is right,** including the cross-crate test
  `only_implemented_settings_are_offered` (`menubar.rs:609`) that gates the menu on the schema rather than on a duplicated
  list.
- **Microphone lifetime is honest.** `crates/outloud/src/mic.rs` opens the device
  on key-down and closes it on every exit path including errors, with a `Drop`
  impl, so the macOS recording indicator means what a user thinks it means. The
  module header explains why, and the reasoning is sound.
- **Headless feature gating is real,** not aspirational:
  `cargo check --no-default-features` passes, and `build-headless.sh:70-88`
  mechanically greps the linked libraries rather than asserting.
- **The config watcher re-arms correctly** on repeated loads
  (`menuhost.rs:139-145`), rebuilding rather than reusing so a moved path is
  picked up. I looked for a second-reload bug here and did not find one.
- **`Mic::open` is idempotent** and `close` is safe to call twice, both tested.
  The second-keypress path in `pipeline.rs:186-190` explicitly refuses overlap.

---

## Verdict: DO-NOT-SHIP

Not because the project is weak. The engineering is careful, the tests are
genuine, and the codebase is unusually honest with itself in comments. The
blockers are all in the layer strangers read first.

**Shortest path to SHIP** (est. under two hours, none of it architectural):

1. **Rewrite `README.md:3`** so "open-source" is not attached to a closed
   recognizer, and amend `:52` and `:86` for the macOS 13-25 dead end.
   *(Blocker 1. This is the one that cannot be undone once it is public.)*
2. **Point the issue template at `doctor --report`**, or better, make redaction
   the default in `crates/diag/src/bin/doctor.rs`. *(Blocker 2, one line.)*
3. **Redact the transcript in `UtteranceReport::render()`** and the selection at
   `pipeline.rs:206`. *(Severity 2, two lines.)*
4. **Fix the artifact-name skew** in `ci.yml:200`, `release.yml:136`, and
   `release.yml:173`, and drop the `|| true` in the headless release job.
   *(Severity 2. Do this before tagging, not after.)*

Then, not blocking a public repo but before inviting users:

5. Reset `dropped_chunks` per utterance, and add the two-utterance test.
6. Mark unwired keys in `starter_file()` and in the `docs/configuration.md`
   table; consider removing `history.enabled` and `telemetry.enabled` entirely.
7. Remove the phantom `outloud set` / `status --json` references.
8. Correct the stale ALSA conclusions in `docs/release-readiness.md:127-134`.

---

## Audit scope and limitations

- **Phase 2 (naming sweep) was not performed.** The rename to OutLoud had not
  landed at the time of writing: `git status --short | wc -l` reported 66
  modified paths and the rename commit was absent from `git log`. Naming
  findings are therefore **deliberately excluded**, with one exception noted
  below, since the tree was moving underneath the audit. The artifact-name
  mismatches in Severity 2 are reported as *workflow-vs-script skew*, verified
  to exist at `HEAD` with two different stale names on either side, which makes
  them a real bug independent of the rename.
- **One likely rename casualty, flagged not asserted:** `README.md:37` currently
  reads "against OutLoud's advertised ~450ms insert latency". Context makes
  clear this should name the *competitor*, not this product. Worth a look when
  the naming sweep runs. Per the brief, `docs/competitive-analysis.md` and
  `docs/ux/visual-parity.md` legitimately reference Aqua Voice as a competitor,
  and `~/.aqua-oss/models` plus `crates/asr/helper/aqua-speech-helper` are
  deliberately unchanged; none of those are reported here.
- **No file in the repository was modified by this audit** other than this
  document. The two-utterance proof test and the starter-file probe were written
  into `/tmp` copies and are not present in the working tree.
- **Windows and Linux runtime behaviour was not exercised**, only dependency
  graphs and workflow definitions. No Windows or Linux hardware was available.
