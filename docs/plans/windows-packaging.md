# Windows Packaging & Everything-Else Plan (draft, in progress)

Scope: per-app profiles wiring, doctor/diag Windows correctness, install/distribution story, autostart/config/uninstall/logs. Overlay and tray are covered by `docs/plans/windows-overlay.md` and `docs/plans/windows-tray.md` — not duplicated here.

Status: COMPLETE. All four sections researched against the current repo
state, cross-checked against `docs/plans/windows-overlay.md` and
`docs/plans/windows-tray.md` for overlap (none found — this plan covers
per-app profiles, the doctor, install/distribution, and autostart/config/
uninstall/logs; those two cover overlay rendering and tray/menu).

---

## 1. Per-app profiles dead on Windows

### 1.0 What a Windows user experiences now

`[profile.*]` in `config.toml` is silently inert. No error, no warning in the
doctor, no menu indication. A user writes `[profile.slack] enabled = false`
expecting dictation muted in Slack; it fires in every app. This is worse than
an unimplemented feature because it's *documented as working*
(`docs/configuration.md:175-224`) with no platform caveat.

### 1.1 Root cause, traced end to end

- `pipeline.rs:475`: `inject::snapshot_and_mode_at_keydown()` is called
  unconditionally every utterance.
- `inject.rs:94-104`: on non-macOS this just calls `mode_at_keydown()` and
  returns `(mode, None)` — the snapshot half is hardcoded `None`.
- `inject.rs:122-132` (`app_identity`): takes `Option<&TextSnapshot>`,
  returns `None` immediately if the snapshot is `None`. On Windows this is
  every call.
- `pipeline.rs:486-487`: `per_app = app_identity(snap).and_then(|id|
  cfg.resolve_for_app.map(|f| f(&id)))` — with `app_identity` always
  `None`, `resolve_for_app` (which does exist and is wired,
  `menuhost.rs:206-225`, `main.rs:456`) never runs. `config::profile::select`
  (`profile.rs:133`) is fully implemented and unit-tested
  (`layers.rs:239-`, `profile.rs` tests) but structurally unreachable on
  Windows — the bug is 100% in `inject.rs`, nowhere in `config`.
- Separately, `mode_at_keydown()` on Windows (`inject.rs:67-84`) DOES read
  a real selection via `UiaTarget::selected_text()` — dictate-vs-edit mode
  detection already works on Windows today. Only the *identity* (which app)
  is thrown away, because `snapshot_and_mode_at_keydown` doesn't route
  through the UIA path at all; it just calls `mode_at_keydown()` and pairs
  it with a hardcoded `None` (`inject.rs:100-103`).

### 1.2 What's already available to fix it

`foreground_process_name()` landed today in
`crates/text-target/src/targets/keys.rs:179-216`. It's `#[cfg(all(target_os
= "windows", feature = "display"))]`, uses `GetForegroundWindow` +
`GetWindowThreadProcessId` + `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`
+ `QueryFullProcessImageNameW`, returns the lowercased exe basename without
`.exe`. This is already called from `inject.rs:375` (`frontmost_app_name`)
for the `accepts()`/transport-tier decision on the non-macOS path
(`inject.rs:369-372`) — so the plumbing for "ask Windows who's foreground"
exists and is proven to compile and be called today. It is just never
threaded into `app_identity`/`AppIdentity`.

Also relevant: `UiaTarget::selected_text()` (`ax.rs:165-190`) already reads
selection through the *same* focused element UIA would need to inspect for
identity — but UIA's `IUIAutomationElement` doesn't hand back "the owning
process's exe name" directly without another `GetCurrentPropertyValue
(UIA_ProcessIdPropertyId)` call, so `foreground_process_name()` (window-based,
not element-based) is the simpler and already-working source of truth. Using
two different processes/windows (UIA's focused element vs.
`GetForegroundWindow`) for mode vs. identity is a latent race if a popup
grabs focus between the two reads — small, but worth collapsing into one
call (see 1.3).

### 1.3 Concrete fix

`config::AppIdentity` has three fields: `bundle_id` (macOS only),
`process_name` (works everywhere), `window_class` (X11/Wayland only, "inert
on macOS" per docs — also inert on Windows; nothing here reads a Win32
window class today and nothing should, `match.process-name` is the Windows
matcher per the docs' own guidance for `ssh-vim`/bare executables).

Two implementation options:

**(a) Minimal, low-risk:** add a Windows-only `app_identity()`-equivalent
that calls `foreground_process_name()` directly and builds
`config::AppIdentity { bundle_id: None, process_name: Some(name),
window_class: None }`, independent of the `TextSnapshot` plumbing (which
stays macOS-only/`None` on Windows). Change
`inject.rs::app_identity(snap: Option<&TextSnapshot>)` to instead take
`(snap: Option<&TextSnapshot>, mode: &Mode)` or add a sibling function, and
call it from `pipeline.rs:486` with a `#[cfg]` branch: macOS uses the
existing snapshot-based path, Windows calls
`text_target::targets::keys::foreground_process_name()` directly and
wraps it. Effort: **small, ~30-60 min**, mechanical, no new Win32 calls
needed, reuses today's proven `foreground_process_name`. This is the
recommended path — it does not touch the `TextSnapshot`/AX abstraction at
all, so it can't regress the macOS AX path, and ships the 90% case (process-
name matching, which is the documented Windows-relevant matcher anyway
since bundle ids don't exist on Windows).

**(b) More complete:** extend `snapshot_and_mode_at_keydown` itself so
Windows returns a synthesized `TextSnapshot` (or a new lighter-weight
"app identity + mode" tuple that isn't AX-shaped) built from one
`GetForegroundWindow` call feeding both `mode_at_keydown`'s UIA read and
the identity — collapses the two-call race noted in 1.2, and lets
`streamer::wants_streaming`'s `snap.app` argument (`pipeline.rs:503-506`,
currently always `None` on Windows too — same root cause, separate call
site) get populated for free. Effort: **medium, ~2-3 hrs**: needs a
Windows-shaped snapshot type or a refactor of `TextSnapshot` to be less
AX/macOS-coupled (e.g. making `bundle_id`/`role`/`value_settable` fields
`Option`/platform-conditional rather than assuming AX semantics), plus
re-verifying nothing downstream (`edit_target()`, `strategy()`,
`is_selection_edit()`) implicitly assumes macOS-only invariants.

**Recommendation:** ship (a) first — it is the whole fix for the reported
bug ("profiles are dead") at a fraction of the effort — and note (b) as a
follow-up that also happens to fix the separate `wants_streaming` gap.

### 1.4 Everything downstream that becomes reachable once this lands

Once `AppIdentity.process_name` is populated on Windows, `config::profile::
select` (already implemented, `profile.rs:133-160`) and `Matcher::matches`
(`profile.rs:63-80`, case-insensitive, trailing-`*` prefix) start firing
for `match.process-name` profiles with zero further code changes —
`resolve_for_app` (`menuhost.rs:206`), `AppSettings.enabled` and
`.prefer_streaming` (`pipeline.rs:114-120`), and the `enabled = false` mute
path (`pipeline.rs:489-498`) are all platform-neutral already and will Just
Work. `match.bundle-id` profiles remain permanently inert on Windows (no
such concept exists) — this needs a doc callout, not a code fix. Vocabulary
sets, formatting rules, and other per-profile keys — check `docs/
configuration.md`'s own limitations table (only a handful of keys are wired
at all, per README's known-limitations list) before assuming a profile key
does anything beyond `enabled`/`insertion.mode` on *any* platform; that gap
is pre-existing and not Windows-specific.

### 1.5 Docs that need a correction alongside the code fix

- `docs/configuration.md:181-185`: `cargo run --release -p ax-edit --example
  whoami` is macOS-only (`ax_edit::snapshot_focused()` returns
  `AxError::Unsupported` off macOS, confirmed at `ax-edit/src/lib.rs:158-167`
  and the `#[cfg(not(target_os = "macos"))]` arm at line 165). A Windows user
  following this instruction gets "could not read the focused element:
  accessibility backend unsupported on this platform" with no next action —
  exactly the confident-wrong-answer trap called out for the doctor (item 2).
  Needs either: a tiny new example in `text-target` that calls
  `foreground_process_name()` and prints a `match.process-name` stanza, or a
  doc caveat pointing Windows users at `match.process-name` and to Task
  Manager's "Details" tab for the exe name. Effort: **~20 min** for the doc
  caveat, **~30 min** for the small parallel example (recommended, matches
  the existing pattern and gives Windows users the same copy-pasteable
  workflow macOS gets).
- `docs/configuration.md:207-208` (the `ssh-vim` example) already correctly
  uses `match.process-name` — good prior art to point at.
- `docs/configuration.md:198`, `202-203` (jetbrains, slack examples) use
  `match.bundle-id` — should gain a one-line note that these are macOS-only
  and Windows users need the `match.process-name` equivalent (e.g. `slack.exe`).

### 1.6 Priority / effort summary for this section

| Change | File(s) | Effort | Impact |
|---|---|---|---|
| Wire `AppIdentity.process_name` on Windows (option a) | `inject.rs`, `pipeline.rs` | 30-60 min | High — unblocks the entire profile feature |
| Correct/replace `whoami` example instructions for Windows | `docs/configuration.md`, optionally new `text-target` example | 20-50 min | Medium — prevents a dead-end doc trap |
| Note `match.bundle-id` is macOS-only in each example | `docs/configuration.md` | 10 min | Low-medium, honesty fix |
| (Follow-up, not blocking) collapse snapshot+identity into one call, fix `wants_streaming`'s Windows `None` | `inject.rs`, `pipeline.rs` | 2-3 hrs | Medium — removes a focus-change race, unblocks streaming-mode profile key on Windows |

## 2. The doctor (crates/diag)

15 checks total, run from `lib.rs:211-231` (`AccessibilityPermission,
InputMonitoringPermission, MicrophonePermission, CodeSignature, BundleLaunch,
BundleFreshness, WindowVisibility, ChromiumOptIn, DisplayServer,
TerminalEmulator, Clipboard, AudioInput, ModelFiles, DiskSpace, CpuFeatures,
PlatformVersion`). Verdict per check below. Good news first: **most of
these already got a Windows pass during earlier work** — 9 of 15 correctly
detect non-macOS and either skip cleanly or give a real Windows-specific
answer. The gaps are narrower than "go check by check" implied, but they are
real and one is a genuine confident-wrong-answer trap plus a privacy leak.

### 2.1 Already correct for Windows — no change needed

| Check | What it does on Windows | Verdict |
|---|---|---|
| `AccessibilityPermission` (`checks.rs:41-`) | Explicitly reports "no permission grant needed, but check UIPI/elevation" (`checks.rs:54-62`) | **Good.** This is exactly the "real Windows equivalent" item 2 asks for — no macOS pane named, correct concept (UIPI) substituted. |
| `MicrophonePermission` | `pass("non-macOS: no TCC microphone gate")` | Fine — accurate. Slightly incomplete: Windows *does* have a real microphone privacy toggle (Settings > Privacy > Microphone) that can deny access. See 2.2. |
| `CodeSignature` | `pass("non-macOS: no code signature gate")` | Accurate for the *TCC-pinning* concern this check exists for. Windows has Authenticode (see §3 build story) but that's a SmartScreen/distribution concern, not a runtime permission gate — correctly out of scope here. |
| `BundleLaunch` | `pass("non-macOS: bundles not applicable")` | Correct — no `.app`/responsible-process concept on Windows. |
| `WindowVisibility` | `pass("non-macOS: skipped")` | Correct — this is specifically the macOS Spaces/AX-tree-per-Space trap; no Windows analog exists (windows on other virtual desktops behave differently, not investigated, not the day's bug). |
| `ChromiumOptIn` | `pass("non-macOS: skipped")` | Correct for now: `AXManualAccessibility` is a macOS AX quirk. Note for later: Windows UIA has no equivalent opt-in gate, Chromium/Electron apps expose UIA fine by default — worth a one-line positive check eventually, not urgent. |
| `DisplayServer` | Real `WindowsDesktop` variant (`checks.rs:490-503`), reports "UI Automation and SendInput available" | **Good**, already Windows-native, unit-tested (`detect_display_on`, tests at `checks.rs:1126`). |
| `PlatformVersion` | `pass("non-macOS: no version gate defined yet")` | Honest — there's no known Windows version floor yet (untested on hardware per README), so "no gate" is the correct non-answer rather than inventing one. |
| `InputMonitoringPermission` | `pass("not macOS: no Input Monitoring grant exists here")` | Correct — Windows' `WH_KEYBOARD_LL` hook needs no comparable grant (today's fixed liveness probe is the real Windows equivalent of this check's *purpose*, but it isn't in `diag` — see 2.4). |

### 2.2 Wrong or thin on Windows — need a change

**`Clipboard` (`checks.rs:640-687`) — a real bug, not just a gap.**
The branch structure is `if cfg!(macos) { pbpaste } else if WAYLAND_DISPLAY
{ .. } else if DISPLAY { .. } else { fail("no display: no clipboard") }`.
There is no Windows branch at all. On a normal Windows desktop session (no
`DISPLAY`/`WAYLAND_DISPLAY`, both are X11/Wayland-only vars that will never
be set natively on Windows) this check falls through to the `else` arm and
reports **"no display: no clipboard"** — a confident, wrong, Fail-severity
answer on a machine where the clipboard obviously works. This is precisely
the failure mode item 2 warns about: it doesn't just fail to help, it
actively tells a Windows user their working clipboard is broken. Windows
clipboard is native (`OpenClipboard`/`GetClipboardData`, or the existing
`clip.exe`/`Get-Clipboard` your text-target crate already shells out to per
the README's transport table) and virtually always available on an
interactive session. Fix: add `else if cfg!(target_os = "windows") {
CheckOutcome::pass("Windows clipboard (native, no install needed)") }`
before the WAYLAND/DISPLAY branches — or probe it for real via
`clip.exe`/`powershell Get-Clipboard`, mirroring the `pbpaste` probe, if a
live check is preferred over a static pass. Effort: **10 min** for the
static pass, **~30 min** to make it a live probe like macOS's.

**`AudioInput` (`checks.rs:688-727`) — thin but honest, not wrong.**
Off macOS: `warn("audio device probe unimplemented off macOS", ...,
"verify ... with e.g. arecord -l")`. `arecord` is Linux-only (ALSA); a
Windows user gets a remedy naming a tool they do not have, which is a
smaller version of the same trap (not as bad as Clipboard's Fail, but the
"next action" is wrong for this platform). Fix: branch three ways —
Windows (`Get-CimInstance Win32_SoundDevice` or point at Settings > Sound >
Input, and/or use `cpal`'s own device enumeration, which the `audio` crate
already links, to do a *real* probe instead of a remedy string), Linux
(`arecord -l`), else generic. A live Windows probe via `cpal::default_host()
.input_devices()` is probably the best answer here since the dependency
is already in the tree (`crates/audio`) — reuses real code instead of
inventing a new shell-out. Effort: **~30-45 min** for a live `cpal`-based
probe shared across platforms (arguably an improvement over macOS's
`system_profiler` shell-out too, but that's a larger refactor than this
task needs — the minimal fix is just the message/remedy branch, ~10 min).

**`ModelFiles` (`checks.rs:740-778`) via `model_dir()` — path bug, not a
message bug.** `model_dir()` builds `PathBuf::from(std::env::var("HOME")
.unwrap_or_else(|_| ".".into())).join(".aqua-oss/models")`. `HOME` is not
a standard Windows environment variable — Windows sets `USERPROFILE`
(`C:\Users\name`), not `HOME` (some environments, like Git Bash/MSYS,
synthesize `HOME`, but a plain PowerShell/cmd launch, which is exactly how
this project's own NSIS installer and `outloud.exe` will be run, will not
have it). Unwrapping to `"."` means the check silently looks in the
**current working directory** instead of the user's profile on a native
Windows launch — every real user gets `warn("no recognizer model in
./.aqua-oss/models")`, technically true but for a location nobody would
think to create, and worse, if the daemon ever *writes* a downloaded model
there too (not yet true, since ASR is a stub — but this is exactly the
kind of latent bug that surfaces the moment whisper.cpp model management
lands) it would scatter model files by whatever directory happened to be
CWD at launch. This is the **same root defect as `config::paths::
user_config_path()`** (`crates/config/src/paths.rs:28-31`, also `HOME`-only)
and the same as `diag::redact::bundle()` (`redact.rs:72`, `HOME`+`USER`)
and `diag::replay.rs:610`. Fix, once, in one place: add a tiny
cross-platform home-dir helper (either take the `dirs` crate as a new
dependency — MIT, tiny, exactly this problem — or a 5-line function trying
`HOME` then `USERPROFILE`) and route `model_dir()`, `user_config_path()`,
`redact::bundle()`, and `replay.rs`'s home lookup through it. This is
listed under the doctor because `ModelFiles` is where it was found, but it
is really a **repo-wide home-directory bug**, not diag-specific — see §4
for the config-path angle and the full list of call sites. Effort:
**~1 hr** for the shared helper plus updating the ~4 call sites (mechanical,
same pattern each time) plus a unit test proving `USERPROFILE` resolves
when `HOME` is absent.

**`DiskSpace` (`checks.rs:789-835`) via `free_bytes_at`/`parse_df_free_kib`
— shells to `df -k`, which does not exist on native Windows.** No
`cfg!(windows)` branch at all. `Command::new("df")` will fail to spawn on
a plain Windows install (no `df.exe` on PATH unless Git-for-Windows/WSL is
present), so `free_bytes_at` returns `None` and the check correctly falls
back to `warn("could not stat filesystem", ..., "check free space manually
with `df -h /`")` — not a Fail, not a lie, but the remedy names a command
that also does not exist on Windows and points at `/` which is not a
Windows path either. Not as bad as Clipboard (it degrades to an honest "I
don't know" rather than a false "it's broken"), but still the
message/remedy trap. Fix: add a Windows path using `GetDiskFreeSpaceExW`
(already have the `windows` crate; feature `Win32_Storage_FileSystem`,
likely already enabled for other Win32 storage calls — verify) against
whatever drive the exe/config lives on, with remedy text `"check free
space in File Explorer (right-click the drive > Properties)"`. Effort:
**~30-45 min**.

### 2.3 What's missing entirely — a Windows-specific check worth adding

**UIPI / elevation state has no dedicated check**, even though it's the
single Windows failure mode called out by name in the README's "trap that
will bite first" section and referenced from `AccessibilityPermission`'s
own remedy text (`checks.rs:59-62`, "check whether the focused window is
running as..."). Today the doctor *mentions* UIPI as a caveat inside the
accessibility check but never actually **tests** whether the current
process is elevated (`OpenProcessToken` + `GetTokenInformation
(TokenElevation)`) or reports the focused window's process, which would let
the doctor say "you are NOT elevated; if dictation goes silent, the focused
window IS" definitively rather than as a hypothetical footnote. This is the
Windows-specific check item 2 is really asking for, distinct from repurposing
a macOS one. Effort: **~45 min-1 hr**, new `ElevationState` check,
Windows-only (`pass`/non-answer elsewhere), using APIs already available via
the `windows` crate dependency.

**Autostart / "does the daemon start on login" has no check on any
platform** (see §4 — there is no autostart mechanism implemented at all
yet), so there is nothing to check today. Once §4's autostart work lands,
the doctor should gain a check for "is the scheduled task / registry Run
key present and pointing at the currently-installed exe" — flagged here so
it isn't forgotten, but it is blocked on §4, not independent work.

### 2.4 Doctor-adjacent bug: bug-report redaction leaks the Windows
username

`diag::redact::bundle()` (`redact.rs:70-74`) reads `std::env::var("HOME")`
and `std::env::var("USER")` to know what to scrub from the "generate a
bug report" output (`menuhost.rs`'s `run_diagnostics`, and the CLI doctor's
own `--report`). On Windows neither is set by default (`USERPROFILE` and
`USERNAME` are the equivalents). The result: `home`/`user` are both empty
strings, `scrub_free_text` has nothing to redact against
(`redact.rs:60-63`'s `out.replace(user, "[user]")` becomes a no-op replace
of `""`), and **the Windows user's real home path and username ship
verbatim** in a file explicitly designed to be pasted into a public GitHub
issue. This is worse than a cosmetic gap — it's the redaction feature
silently not redacting, on the one platform where the doctor is being
positioned as the primary bug-report tool for people running unverified,
untested-on-hardware code. Fix: same shared home-dir helper as §2.2/§4,
plus reading `USERNAME` (Windows) as a fallback for `USER`. Effort:
**~20 min once the shared helper from §2.2 exists** (this becomes two more
call sites using it).

### 2.5 `scripts/doctor.sh` itself: not runnable from a native Windows shell

`scripts/doctor.sh:22` already branches on `uname != Darwin` to skip the
macOS bundle dance and just `cargo run --bin doctor`, which is correct
in spirit — but the script is bash. A user on cmd.exe or PowerShell without
Git Bash cannot run it at all (`.sh` has no default file association,
`bash` may not be on PATH). Two options: (a) document `cargo run --release
--bin doctor` directly as the Windows invocation in the README's Windows
section (near-zero effort, and already technically correct per the
script's own logic), or (b) add a thin `scripts/doctor.ps1` mirroring the
non-Darwin branch for a native experience. Given `build-windows.sh` already
sets the precedent of "bash script, run via Git Bash on the runner" for
this project's Windows tooling, (a) matches existing project convention
and is enough — a `.ps1` doubles maintenance for a 2-line script. Effort:
**~10 min**, README doc change only.

### 2.6 Priority / effort summary for this section

| Check | Problem | Fix effort | Severity |
|---|---|---|---|
| `Clipboard` | Confident wrong Fail on every Windows machine | 10-30 min | **High** — false alarm on a check most likely to run first |
| `redact::bundle` (HOME/USER) | Bug reports don't redact the Windows username | 20 min (after shared helper) | **High** — privacy leak in a public-facing artifact |
| `model_dir`/config paths (HOME-only) | Silently wrong directory on native Windows | ~1 hr (shared helper + 4 call sites) | **High** — silent, compounds once models/config actually get written |
| `AudioInput` | Wrong remedy tool name (`arecord`) for Windows | 10-45 min | Medium |
| `DiskSpace` | `df` doesn't exist; degrades to unhelpful but not false | 30-45 min | Medium |
| New `ElevationState`/UIPI check | Missing; the #1 named Windows failure mode has no direct test | 45 min-1 hr | Medium-high, directly serves README's own warning |
| `scripts/doctor.sh` on native shells | Not runnable without Git Bash | 10 min (doc only) | Low |
| Autostart check | Blocked on §4 existing at all | n/a yet | Low now, becomes Medium once §4 ships |

## 3. Install and launch / distribution

### 3.0 The build/signing/packaging story is already mature — bigger gap is upstream of it

`scripts/build-windows.sh` and `docs/build-and-release.md`'s Windows
section are genuinely good and need no rework: portable zip, NSIS installer
(primary, per-user, no UAC), optional MSI via WiX (enterprise), Authenticode
signing with RFC3161 timestamping gated on a CI secret, honest SmartScreen-
reputation discussion (EV/Trusted Signing budgeted like the Apple Developer
ID), and a documented defense against the Handy AVX2-crash class (no
`-C target-cpu` anywhere, `ort` banned in `deny.toml`, baseline verified by
disassembling the shipped exe). This is not something item 3 needs to
redesign. What follows is the gap this packaging pipeline does not cover:
**what actually happens when a user runs the resulting `outloud.exe`.**

### 3.1 The real problem: the officially-built Windows binary cannot transcribe anything, out of the box

Trace it exactly:

- `scripts/build-windows.sh:55`: `cargo build --release --locked --package
  spike-cli --package outloud --target "$TARGET"` — **no `--features
  whisper`**. Plain `cargo build -p outloud` on Windows compiles with
  `outloud`'s `default = ["display"]` only (`crates/outloud/Cargo.toml:17`);
  `whisper` is off by default in both `outloud`'s and `asr`'s Cargo.toml
  (`crates/outloud/Cargo.toml:22`, `crates/asr/Cargo.toml:17`, both
  deliberately, since whisper-rs needs cmake+LLVM+MSVC to even compile —
  see docs/asr-integration.md's own table).
- `main.rs:52`: the default `--asr` value is `"apple"`, unconditionally,
  on every platform — there is no `#[cfg(target_os = "windows")]` override
  defaulting to `"whisper"` or anything else.
- `make_recognizer_factory("apple", ..)` (`main.rs:160-163`) calls
  `asr::backends::apple::AppleRecognizer::new()`, which is compiled
  unconditionally too (`asr/src/backends/mod.rs:7`, `pub mod apple;`, no
  `#[cfg(target_os = "macos")]` gate on the module itself — it compiles
  fine on Windows because it's plain Rust + `std::process::Command`, it
  just can never find the helper binary there).
- `AppleRecognizer::new()` calls `find_helper()` (`apple.rs:64-83`), which
  looks for `outloud-speech-helper` (a **Swift** binary, built by `swiftc`,
  which does not exist on Windows) next to the exe or in the dev tree.
  On Windows this is always `None`, so `new()` immediately returns
  `Err("outloud-speech-helper not found; build it with `swiftc -O ...`")`
  (`apple.rs:98-103`) — an error message that tells a Windows user to run
  a macOS-only compiler.

**Net result: `outloud.exe` built exactly as `build-windows.sh` builds it,
launched with no flags (the default, matching how a real user double-clicks
it or an NSIS-installed shortcut runs it), fails on the very first
utterance** with a Swift-toolchain error message that makes no sense on
Windows. This is a strictly worse first-run experience than "recognizer not
implemented" — it's "recognizer misconfigured for the wrong OS," on the
literal happy path the README's own "Install" section describes for macOS
but has no Windows equivalent for. The task brief's framing ("Windows
dictates correctly end to end... verified into Discord") is true only because
that verification was necessarily done with `--asr whisper` and a
hand-built binary+model — not with what ships from the documented build
script.

### 3.2 Fix: make the shipped default actually work, in priority order

1. **Change the Windows-built binary's default `--asr` to `whisper`, and
   build it with the feature on.** Concretely: `main.rs:52`'s
   `asr: "apple".into()` should become platform-conditional (`#[cfg(target_os
   = "macos")] "apple"`, `#[cfg(not(target_os = "macos"))] "whisper"`), and
   `build-windows.sh:55` needs `--features whisper` (or `whisper-cuda` on a
   GPU runner — see 3.3 on which one to actually ship). Effort: **~15 min**
   for the default-arg change (mechanical, one `cfg` split), but this is
   gated on 3.3's toolchain decision for the build script itself since
   turning on `whisper` in CI needs cmake+LLVM+MSVC on the Windows runner
   image, which is a CI infrastructure change, not just a flag flip.
2. **Not doing #1, only failing more usefully:** at minimum, `main.rs`
   should refuse to default to `"apple"` on non-macOS and print a clear
   error naming `--asr whisper` and `OUTLOUD_WHISPER_MODEL` instead of
   attempting the Apple path and surfacing a Swift-toolchain error. This is
   the minimum honesty fix if the CI/toolchain work in #1 can't land today.
   Effort: **~15 min**, pure `main.rs` change, no CI dependency.
3. **The model file itself is the remaining hard problem** (see 3.3):
   `OUTLOUD_WHISPER_MODEL` requires the user to already have a 142MiB+
   `.bin` file somewhere and know to set an env var. Shipping a *working*
   default needs either bundling the model (rejected below, too big for a
   git-tracked repo/simple zip) or a first-run downloader.

### 3.3 What "install" should look like, and what must be bundled vs. fetched

**The model (142MiB minimum, `ggml-base.en.bin`) cannot live in the repo or
the git-tracked build output** — this is already implicit in
`docs/asr-integration.md:167`'s statement that model weights are "fetched
at runtime... never vendored into this repository," which is correct
policy and should not change. What's missing is the *fetching* half for
Windows: today `OUTLOUD_WHISPER_MODEL` is a manual `curl` + env var
(`docs/asr-integration.md:99-105`), which is a fine developer workflow and
a bad first-run experience for a packaged installer's target audience.

Recommended shape, cheapest-to-most-complete:

- **(a) NSIS/MSI installer downloads the model post-install.** NSIS
  supports an `inetc` plugin (or a bundled PowerShell `Invoke-WebRequest`
  call) to fetch a URL after file copy, writing into
  `%LOCALAPPDATA%\OutLoud\models\ggml-base.en.bin` and setting
  `OUTLOUD_WHISPER_MODEL` via the same per-user registry mechanism the
  installer already uses for its uninstall entry
  (`build-windows.sh`'s existing `WriteRegStr HKCU`). This keeps the
  142MiB out of the signed installer artifact (smaller download, and a
  changed model doesn't require a re-signed re-released installer), at
  the cost of needing network access during install and a "downloading
  the speech model (142MiB)..." progress step. **This is the recommended
  default path** — closest to what Handy/VoiceInk-style local tools
  already do, and reuses the installer infrastructure that already exists
  rather than inventing a separate updater. Effort: **~2-3 hrs** (NSIS
  scripting + testing the download step + wiring the env var / a config
  default so the daemon finds it without the user setting anything).
- **(b) First-run in-app download**, i.e. the daemon itself detects no
  model configured and offers (via the tray menu, once it exists per
  `windows-tray.md`) a "Download recognizer model" action that fetches
  into `model_dir()` (once that function is fixed per §2.2/§4 to resolve
  correctly on Windows) and updates `config.toml`. More user-friendly
  (works even for a portable-zip install with no installer step) but is
  real new product code, not packaging — a model-manager module does not
  exist yet anywhere in the tree (`docs/asr-integration.md`'s own
  "Backends" table lists Parakeet/model-manager as "stub + model registry
  entry", not built). Effort: **~1-2 days**, genuinely new work, not a
  packaging task — flagging it here as the more complete answer but it is
  scoped beyond "packaging."
- **(c) Ship the model in the zip/installer anyway.** Rejected: doubles
  every release artifact by 142MiB minimum (more for larger models),
  bloats the signed-and-timestamped installer that already has SmartScreen
  reputation concerns to manage, and conflicts with the project's own
  stated policy of never vendoring weights. Only worth it if network
  access during install turns out to be unacceptable for the target
  audience — not assumed here.

**Recommendation:** ship (a) first since it needs no new product code, only
build-script work already 90% analogous to what `build-windows.sh` does for
signing; treat (b) as the eventual "proper" UX once a model manager exists
for other reasons (Parakeet support, per docs/asr-integration.md's own
roadmap) rather than building one just for this.

### 3.4 cmake/LLVM/MSVC/CUDA: a real barrier, but only for *building*, not
*installing*

The task brief's framing ("a Windows user currently clones a repo and needs
cmake, LLVM, MSVC Build Tools and the CUDA toolkit") describes the
**developer/build experience**, not what an end user installing a signed
release needs. This distinction matters for the plan: an end user who
downloads `outloud-$VERSION-x86_64-pc-windows-msvc-setup.exe` from a
release page never touches cmake or LLVM at all — those are only needed by
whoever runs `build-windows.sh --features whisper`, which after 3.2 is the
project's own CI/release pipeline, not the user's machine. So the four
prerequisites in `docs/asr-integration.md`'s table are correctly documented
already, for the correct audience (contributors building from source), and
need no change. What needs to change is making sure the CI machine building
official Windows releases has cmake+LLVM+MSVC+CUDA (or ships CPU-only, see
below) so that *its* output — the thing users actually download — works.
This is a CI/infra task (Windows runner image provisioning), not a docs
task, and is properly scoped under 3.2/3.3, not this subsection; noted here
only to correct the framing.

**CPU-only vs CUDA for the shipped default:** `whisper` (CPU) measured
8970ms for a 5s utterance on a Ryzen 9 9950X3D (`docs/asr-integration.md`'s
own table) — unusably slow regardless of correctness. `whisper-cuda`
measured 346-364ms on the same machine with an RTX 5090 — usable, but NVIDIA-
only, and a release built with `whisper-cuda` presumably fails to even link
or at minimum silently underperforms on AMD/Intel/no-GPU machines (not
verified here; worth confirming whether whisper-rs's CUDA feature falls
back to CPU gracefully or hard-fails without an NVIDIA GPU present, since
that changes whether shipping ONE default build with CUDA baked in is safe
for the general Windows population or whether two release artifacts
— CPU and CUDA — are needed). This is a real open question the packaging
plan surfaces but doesn't answer: **recommend explicitly deciding and
testing this before wiring 3.2**, since shipping a CUDA-only build to a
non-NVIDIA machine would trade "doesn't transcribe" for "doesn't launch,"
which is not an improvement.

### 3.5 What a realistic Windows release actually needs, end to end

Putting 3.1-3.4 together, the concrete list of changes for a Windows user
to go from "download a link" to "dictating," in the order they'd be hit:

1. Fix the default `--asr` selection (3.2#1/#2) — otherwise nothing below
   matters, the app never gets past the first utterance.
2. Decide CPU-vs-CUDA-vs-both release artifacts (3.4) — otherwise step 1's
   fix ships a build that's either too slow or crashes on the wrong GPU
   vendor.
3. Enable `--features whisper[,-cuda]` in `build-windows.sh` and provision
   the CI Windows runner with cmake/LLVM/MSVC(/CUDA) to build it (3.1/3.4)
   — infra work, blocks 1-2 from having anything to ship.
4. Add model acquisition to the installer (3.3a) so a user does not need to
   manually `curl` a `.bin` file and set an env var — otherwise steps 1-3
   produce a binary that still can't transcribe until the user does the
   developer workflow from `docs/asr-integration.md` by hand.
5. (Already fine, no change) signing/SmartScreen/artifact packaging itself.

### 3.6 Priority / effort summary for this section

| Change | Blocks | Effort | Impact |
|---|---|---|---|
| Default `--asr` off `"apple"` on non-macOS (min: honest error; ideal: `"whisper"`) | Everything below | 15 min (honest error) / +CI work (real default) | **Critical** — the entire "does dictation work at all" question for a Windows release |
| Decide + verify CPU vs CUDA (or both) release artifacts | Enabling whisper in CI | Investigation ~30-60 min, then per-artifact CI cost | **High** — wrong choice ships a build that crashes or is unusably slow |
| Enable `--features whisper[-cuda]` + provision CI Windows runner toolchain | Shipping a working default | CI infra, not scoped here in hours | **Critical**, but infra-owned |
| Installer-driven model download (3.3a) | First-run UX | ~2-3 hrs | **High** — without this, users still need the manual curl+env-var workflow even after 1-3 land |
| (Optional, larger) in-app model manager (3.3b) | — | 1-2 days | Medium, better UX, not required for a working v1 |
| Correct the docs framing (asr-integration.md's Windows table is already fine, just for the wrong audience if read as "what users need") | — | 0 — no change needed, just noting it | n/a |

## 4. Autostart, config paths, uninstall, logs

### 4.1 Config path: broken on native Windows, not just "different"

`config::paths::user_config_path()` (`crates/config/src/paths.rs:28-31`):

```rust
let dir = match std::env::var_os("XDG_CONFIG_HOME") {
    Some(x) if !x.is_empty() => PathBuf::from(x),
    _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
};
```

`docs/configuration.md:14-17` documents this as deliberate: `~/.config/
outloud/` **on macOS too**, because "this is a file you are meant to read,
edit, diff and sync like any dotfile." That's a fine, defensible design
choice and this plan does not recommend moving Windows to `%APPDATA%` just
because it's conventional — the project has explicitly already rejected
"conventional per-OS location" for macOS's `~/Library/Application Support`
for the same reason, and a Windows user who wants their config in one
`~/.config` tree to sync across machines (the exact audience Settings-as-
dotfiles is for) would be annoyed if Windows silently used a different
scheme. **The bug is not the choice of `~/.config`, it's that `HOME` isn't
how you spell "home directory" on Windows.**

On a native (non-Git-Bash, non-WSL) Windows process, `HOME` is not set by
default — `USERPROFILE` (`C:\Users\name`) is the standard variable, `HOME`
is a Unix-world convention some environments (Git Bash/MSYS/Cygwin/WSL)
synthesize but a plain double-clicked `outloud.exe` or NSIS-installed
shortcut will not have. `PathBuf::from(std::env::var_os("HOME")?)` returns
`None` via the `?` the moment `HOME` is absent, so `user_config_path()`
returns `None` entirely. Trace the fallout:

- `ensure_user_config()` (`paths.rs:55-58`) turns that `None` into
  `Err("no HOME or XDG_CONFIG_HOME, so there is no user config directory")`
  — every settings read/write in `menuhost.rs` (`reload`, `write_setting`,
  `run_diagnostics`, `config_path_for_display`, at least 6 call sites per
  the earlier grep) fails.
- The daemon still runs (config load failures are handled, not fatal, per
  `menuhost.rs:247-253`'s `problems.push(...)` pattern) but with **zero
  persisted settings ever**: every launch starts from schema defaults, no
  hotkey customization survives a restart, no profile ever loads, and the
  **menu bar's "Edit config file..." action has no file to open** — it was
  built specifically so the daemon and the menu agree on one path
  (`paths.rs`'s own doc comment: "the daemon, `outloud set`, the menu-bar
  Settings items, and the docs all have to name the same path or the
  user's edit lands somewhere nothing reads") and on a native Windows
  launch there IS no path to agree on.
- This is the **same root cause identified independently in §2.2 and §2.4**
  (`diag::checks::model_dir()`, `diag::redact::bundle()`,
  `diag::replay.rs:610`) — four call sites, one bug, one fix.

**Fix:** one shared helper, e.g. `fn home_dir() -> Option<PathBuf>` trying
`HOME` then `USERPROFILE` (both via `std::env::var_os`, no new dependency
needed — the whole fix is ~8 lines), used by all four sites
(`config::paths::user_config_path`, `diag::checks::model_dir`,
`diag::redact::bundle`, `diag::replay`'s equivalent). Where to put it:
`crates/config/src/paths.rs` is the natural home since `config` is already
the lowest-level crate that needs it and `diag` already depends on
`ax_edit`/`hotkey` (check `diag`'s existing deps) — if `diag` doesn't
already depend on `config`, either add that dependency or duplicate the
5-line helper (duplicating something this small is defensible to avoid a
new inter-crate edge, but a single source of truth is preferable if the
dependency is cheap to add). Effort: **~1 hr total** including a unit test
that clears `HOME`, sets `USERPROFILE`, and asserts the fallback fires
(mirrors the existing `HOME`/`XDG_CONFIG_HOME` test at `paths.rs:180-197`).
This is the **single highest-leverage fix in this entire plan** — it's
small, it's the same root cause behind three separately-discovered
symptoms (dead profiles' config storage, wrong model directory, leaky bug
reports), and every other Windows feature that touches config or models is
silently broken until it lands.

### 4.2 No autostart mechanism exists on any platform — this is a real gap, not just a Windows one

Searched for `LaunchAgent`/`launchd`/`SMAppService`/`autostart`/`login
item` across the whole repo: nothing, on any platform. There is no code
that registers OutLoud to start at login on macOS, Windows, or Linux. This
means today, on every platform, a user must manually re-launch the app
after every reboot — for macOS this is `open -a OutLoud.app` or clicking it
in Finder; there is no first-class "start at login" checkbox anywhere in
the menu (`menubar.rs`'s menu model has no such item; confirmed by its
`build()` output shape). This is worth surfacing plainly rather than
treating as Windows-specific scope creep, per the instruction to say so if
something is already fine (it is not) — but implementing full autostart on
both platforms is bigger than this task's remaining budget, so the
recommendation below is scoped to "make the gap visible and cheap to close
later," not to build it now.

**What Windows would need, when this is picked up:** a Task Scheduler entry
(`schtasks /create` or the `windows` crate's `ITaskService` COM API) at
logon, pointed at the installed `outloud.exe` path — NOT a `HKCU\...\Run`
registry key, because Run-key programs launch with no dependency ordering
and no delay, which for an app that opens a microphone and installs a
keyboard hook can race the audio subsystem or Explorer's shell hooks coming
up; Task Scheduler's "at logon" trigger with a short delay is the more
robust primitive Windows itself recommends for this class of app (this
recommendation is not verified against a real hardware boot sequence for
this specific app, and should be tested when implemented, not taken as
given). The NSIS installer (`build-windows.sh`) is the natural place to
register it (an install-time task-scheduler entry, removed by the
uninstaller — see 4.4), consistent with the per-user, no-UAC installer
already shipping. Effort estimate for the Windows half alone, when
scheduled: **~3-4 hrs** (NSIS scripting the task registration, an uninstall
counterpart, and a menu-bar toggle so it's user-controllable rather than
install-time-only — matching the macOS side, which would need
`SMAppService.mainApp.register()`/`LaunchAgent` plist work of comparable
size). Not estimating a combined number since the macOS half is equally
unbuilt and equally out of this task's stated scope (item 4 says
"Windows"), but noting the asymmetry would look strange to ship Windows-
only.

### 4.3 Log location: nothing exists yet, and the tray plan's own analysis already covers the Windows-specific mechanism

There is no persistent log file on any platform today. The only "log" a
user can see is: (a) stderr, visible only when launched from a terminal, or
(b) macOS's `OUTLOUD_SPIKE_LOG` env-var mirroring trick used by
`scripts/run.sh`/`scripts/doctor.sh` to capture a LaunchServices-launched
process's stdout into a tailable file — a **developer/debugging
convenience**, not a shipped feature (it requires launching through the
script, not through the normal `open -a OutLoud.app` a real user does).
`diag::run_diagnostics()`'s `diagnostics.txt` (`menuhost.rs:420-445`) is the
closest thing to a real log artifact, and it's a point-in-time snapshot
generated on demand, not a running log.

**This is already the exact subject of `docs/plans/windows-tray.md` §4**
("Console removal sequencing"), which independently arrives at "add a log
file beside `config.toml`" as step 2 of console removal, for reasons
specific to Windows (a GUI-subsystem process's `eprintln!` panics with no
console attached — a Windows-specific correctness issue, not a nice-to-
have). Rather than duplicate that analysis, this plan defers to it and
notes the connection: once §4.1's fix lands (`user_config_path()` resolves
correctly on Windows), the tray plan's "beside config.toml" log file has
somewhere real to go — on a broken native Windows launch today, "beside
config.toml" is `None`, so the tray plan's step 2 is silently blocked on
this section's 4.1 fix landing first. **Sequencing note for whoever
executes both plans: land 4.1 before windows-tray.md §4.2.**

For macOS, no equivalent "real log file" exists either — same gap, not
raised as Windows-specific, out of this section's scope but worth a
one-line mention since "where's the log" is a natural doctor/support
question on any platform and today the honest answer is "there isn't one
outside a debug launch."

### 4.4 Uninstall: NSIS installer's uninstaller is minimal; no equivalent of `uninstall-macos.sh`

`scripts/uninstall-macos.sh` is thorough: stops running processes (current
AND legacy binary names), removes the app bundle, resets TCC grants,
removes the shell-bridge plugin line from rc files, and optionally purges
config with `--purge`, all gated behind `--dry-run`. It exists because (per
its own comment) "removing OutLoud by hand means knowing about four
separate locations... how do I uninstall it going unanswered is how a beta
earns a reputation for being invasive."

The Windows NSIS `Section "Uninstall"` in `build-windows.sh` (generated
inline, ~6 lines) does exactly three things: delete the two exes, delete
the uninstaller, remove the install directory, remove one registry
uninstall-entry key. It does **not**: stop a running `outloud.exe` process
first (if the daemon is running and holding the exe file open, deleting it
can fail or leave a locked file needing a reboot — the exact class of bug
`uninstall-macos.sh`'s step 1 exists to prevent), touch `%LOCALAPPDATA%
\OutLoud`'s config directory at all (so `~/.config/outloud` — once 4.1 is
fixed — is orphaned, silently, forever, unless the user finds it by hand;
no `--purge` equivalent exists to even offer the choice), or clean up
whatever autostart mechanism 4.2 eventually adds (currently moot since 4.2
doesn't exist yet, but will need updating together).

Also worth noting: this NSIS installer/uninstaller currently ships
*`outloud-spike`*, i.e. targets the spike-cli harness (`installer.nsi`'s
own header comment: "Generated by build-windows.sh... a spike harness must
not persist" — the installer's own text explicitly disclaims persistence
intent, which is at odds with treating it as the real product's installer).
`build-windows.sh` does already copy `outloud.exe` alongside
`outloud-spike.exe` into the same install dir (`build-windows.sh:57-58`),
so the daemon IS included in what ships — but the NSIS script's own
comments and naming ("OutLoud Spike", no services, no autostart "by
design") read as a development harness's installer that gained the real
daemon as a rider, not a production installer built for the daemon as the
primary artifact. This mismatch in framing should be resolved (rename/
reframe the installer metadata to be about OutLoud the product, not "OutLoud
Spike") before this is presented as a real release vehicle, independent of
any functional gap.

**Fix, in priority order:**
1. Stop the running process before deleting files (`nsProcess` plugin's
   `_FindProcess`/`_KillProcess`, or a `taskkill /IM outloud.exe /F`
   shelled from the `.nsi` — mirrors `uninstall-macos.sh`'s step 1
   conceptually). Effort: **~30 min**.
2. Prompt (or at minimum document) whether to remove
   `%LOCALAPPDATA%\OutLoud` config — an NSIS checkbox page mirroring
   `--purge`, or simplest: leave config alone by default (matches macOS's
   default-keep behavior) and just document that `~/.config/outloud`
   (once 4.1 lands) survives uninstall, so a manual removal instruction
   exists somewhere findable. Effort: **~20 min doc-only / ~1 hr for a
   real purge checkbox**.
3. Rename/reframe the installer's product identity away from "OutLoud
   Spike" language once the daemon is the thing actually being installed
   for users, not developers. Effort: **~20 min**, text/metadata only.
4. When 4.2 (autostart) lands, the uninstaller must remove that
   registration too — flagged for sequencing, not separately estimated.

### 4.5 Priority / effort summary for this section

| Change | File(s) | Effort | Impact |
|---|---|---|---|
| Shared `HOME`/`USERPROFILE` fallback helper, used by config paths + all 3 diag call sites | `config/src/paths.rs` (+ diag call sites) | ~1 hr | **Critical** — highest-leverage single fix in the whole plan; unblocks config persistence, correct model dir, and bug-report redaction simultaneously |
| Document/flag the missing autostart mechanism (both platforms); scope full implementation as follow-up | docs only now | ~15 min to document the gap; ~3-4 hrs (Windows half) when actually built | Medium now (visibility), High once picked up (real UX gap on both platforms) |
| Sequence note: land 4.1 before `windows-tray.md`'s log-file step | cross-plan coordination | 0 (just ordering) | Prevents building a log file with nowhere real to go |
| NSIS uninstaller: stop running process first | `build-windows.sh`'s generated `.nsi` | ~30 min | **High** — prevents a locked-file uninstall failure, the exact bug class the macOS script's step 1 exists for |
| NSIS uninstaller: config purge option or at least documented manual path | same | 20 min (doc) / ~1 hr (real option) | Medium |
| Reframe installer identity from "OutLoud Spike" to the real product | same | ~20 min | Low-medium, honesty/professionalism fix before wider release |

## Priority summary

Ranked by user impact, across all four sections, cheapest-highest-impact
first. "Effort" is implementation time only, not review/CI turnaround.

| # | Fix | Section | Effort | Why it's ranked here |
|---|---|---|---|---|
| 1 | Shared `HOME`/`USERPROFILE` home-dir helper | §4.1 | ~1 hr | Single highest-leverage fix: unblocks config persistence, correct model directory, AND bug-report redaction simultaneously. Nothing else in this plan matters if settings never save. |
| 2 | Fix `Clipboard` doctor check's Windows false-Fail | §2.2 | 10-30 min | Actively lies to users on a check that runs on every doctor invocation; the exact confident-wrong-answer trap the task called out. |
| 3 | Default `--asr` off `"apple"` on non-macOS (min: honest error) | §3.2 | 15 min | Without this, a Windows user's very first utterance fails with a Swift-toolchain error. Minimum fix is trivial; full fix (real `whisper` default) is gated on CI work. |
| 4 | Wire `AppIdentity.process_name` on Windows | §1.3 | 30-60 min | Whole per-app-profiles feature is unreachable without this; small, isolated, no regression risk to macOS. |
| 5 | Fix `redact::bundle()`'s HOME/USER (after #1's helper exists) | §2.4 | 20 min | Privacy leak: Windows bug reports ship the real username/home path unredacted. |
| 6 | NSIS uninstaller: stop running process before deleting files | §4.4 | ~30 min | Prevents a locked-file uninstall failure — same bug class `uninstall-macos.sh` was written to prevent. |
| 7 | Fix `AudioInput`/`DiskSpace` doctor remedy text for Windows | §2.2 | 30-90 min combined | Wrong tool names in remedies (`arecord`, `df`); degrades gracefully today but still misleads. |
| 8 | Add `ElevationState`/UIPI check to the doctor | §2.3 | 45 min-1 hr | Directly tests the #1 named Windows failure mode (README's own "trap that will bite first"), currently only mentioned as a footnote. |
| 9 | Decide + verify CPU vs CUDA release artifact strategy | §3.4 | 30-60 min investigation | Blocks safely enabling `whisper` in CI; wrong choice trades "doesn't transcribe" for "doesn't launch" on non-NVIDIA machines. |
| 10 | Enable `--features whisper[-cuda]` + provision CI Windows toolchain | §3.1/§3.2/§3.5 | CI infra, not hour-scoped | Makes the *shipped* binary actually transcribe; currently only hand-built binaries do. |
| 11 | Installer-driven model download | §3.3 | ~2-3 hrs | Closes the gap between "binary can transcribe" and "user never has to manually curl a 142MiB file." |
| 12 | Doc corrections: `whoami` example is macOS-only, `match.bundle-id` examples need a Windows caveat | §1.5 | 20-50 min | Prevents a documented dead-end (item 2's own concern) for the one config feature this task was asked to unblock. |
| 13 | Document/flag missing autostart (both platforms) | §4.2 | ~15 min to document | Real gap, but bigger than "packaging" scope; flagging beats silent omission. |
| 14 | Reframe NSIS installer identity away from "OutLoud Spike" | §4.4 | ~20 min | Professionalism/honesty fix, not functional, before wider release. |
| 15 | `scripts/doctor.sh` Windows-native-shell doc note | §2.5 | ~10 min | Low severity, easy fix. |

### What's already fine — explicitly, per the instructions

- Windows build/signing/packaging pipeline (`build-windows.sh`,
  `docs/build-and-release.md`'s Windows section): mature, well-reasoned,
  no changes needed (§3.0).
- 9 of 15 doctor checks already give correct, honest, Windows-aware
  answers (§2.1) — including the accessibility-permission check, which is
  exactly the "real Windows equivalent, not a macOS pane" the task asked
  for.
- The single-instance mutex, keyboard-hook message pump, hook liveness
  probe, and `accepts()` write-path fix (today's prior work) are outside
  this section's scope and not re-litigated, per the brief.
- The `foreground_process_name()` Windows API call (landed today) is
  correct and sufficient to fix §1 with no new Win32 code.
- `docs/asr-integration.md`'s Windows whisper.cpp build-toolchain table
  (cmake/LLVM/MSVC/CUDA) is accurate for its actual audience (contributors
  building from source) and needs no correction — only the framing that it
  describes *user* requirements was wrong (§3.4).

### Cross-plan dependency

`docs/plans/windows-tray.md` §4's planned log-file-beside-config.toml step
is silently blocked until §4.1's `HOME`/`USERPROFILE` fix lands (today,
"beside config.toml" resolves to nowhere on a native Windows launch).
Whoever executes both plans should land §4.1 first.
