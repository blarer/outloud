# v0.1.0 release checklist and audit verdict

Audited 2026-07-29 on macOS 26.5.2 (M-series, arm64), commit `7504a41`.
Every claim below marked **verified** was reproduced on this machine by
running the thing, not by reading the docs. The mechanical half of a release
is still `./scripts/preflight.sh`; this document records what that run and a
manual README-claims audit actually found, and ends with the verdict.

## 1. What was verified (evidence, not assertion)

| Claim | What was run | What was seen |
|---|---|---|
| Bundle builds | `./scripts/bundle-outloud-macos.sh` | `dist/OutLoud.app` built, ad-hoc signed, helper `aqua-speech-helper` present, `codesign` valid on disk. **Verified** |
| Dictation | `dist/OutLoud.app/Contents/MacOS/OutLoud --once --say "hello from a local dictation daemon" --no-overlay` | Text delivered via set-value, `release->text 131ms`. (Recognizer heard "demon" for "daemon", which is an ASR accuracy note, not a pipeline bug.) **Verified** |
| Edit-by-voice | TextEdit doc "the quick brown fox", select-all, `--once --say "change quick to slow"` | Document read back through AppleScript: `the slow brown fox`, via set-selected-text, 119ms. **Verified** |
| Shell bridge protocol + zsh plugin | bridge `serve` on a temp socket, real interactive zsh in a temp ZDOTDIR, `shell-bridge intent`, `^X^A`, `^Xu` | Line rewrote `prod-web` → `staging-web` in place; zsh undo restored it; daemon peek confirmed. **Verified, but only after fixing the broken verify script by hand — see blocker 2** |
| Doctor | `./scripts/doctor.sh` | Ran via its own LaunchServices bundle; correctly FAILed accessibility (grant invalidated by the rebuild, exactly as its own remedy text predicts) and named remedies for each check. **Verified** |
| Streaming write path | `insertion.mode = "stream"` in config, `cargo run --release -p outloud --example stream_probe` against TextEdit | 5 writes + revision splice landed: `the slick brown fox jumps over the lazy dog.`, p50 3.0ms/write. **Verified.** Daemon-integrated streaming is intentionally off for `--once` and README already lists it as not yet wired; consistent. |
| Licences | `cargo deny check` | `advisories ok, bans ok, licenses ok, sources ok`. LICENSE is MIT; every crate uses `license.workspace = true` → MIT. Weight licences stated separately: Parakeet CC-BY-4.0 (docs/asr-integration.md), Qwen3 Apache-2.0 (docs/llm.md), Apple weights closed and disclosed in the README. **Verified** |
| Gatekeeper | `spctl -a -t exec dist/OutLoud.app` | `rejected`, `Signature=adhoc`, no TeamIdentifier. Matches the README's own honest disclosure. |
| Shell-bridge threat model vs code | read `crates/shell-bridge/src/{server,peer}.rs` against docs/shell-integration.md | Matches: dir 0700 + socket 0600 before first accept, kernel peer-cred check before any read, root rejected, non-unix refused, 30s intent TTL, first-EDIT consumption, 1 MiB cap, 2s timeouts, no execution verb anywhere in the protocol. Permission and TTL claims are covered by passing unit tests (17 pass). |
| diag redaction | read `crates/diag/src/redact.rs` + ran its tests | Tests are real: they assert the secret/username/home strings are *absent* from output, not merely that a function ran. 45 diag tests pass. Caveat in blocker 4. |

## 2. Blockers, ranked

### NO-GO blockers

1. **The README's shell edit-by-voice flow does not work by voice.**
   README ("Editing a shell command line"): run the bridge, type a command,
   *speak an edit*, press `^X^A`. Reproduction: bridge serving on its default
   socket, terminal focused, daemon given "change prod-web to staging-web".
   The daemon typed the transcript into the terminal as text
   (`via synthetic-keys-paced`) and `shell-bridge status` showed
   `staged=<none>`: the daemon never sends `INTENT` to the bridge. Nothing in
   `crates/outloud` references the bridge socket at all (grep confirms). The
   pipeline only works when the intent is staged manually with
   `shell-bridge intent "..."`, which is how `verify-zsh.exp` does it.
   This is the project's headline differentiator ("no other tool does this")
   claimed as working end to end. **Code fix** (wire daemon → bridge INTENT
   when a terminal is focused) **or doc fix** (README must say the voice half
   is not wired and show the manual `shell-bridge intent` flow). One of the
   two must land before release.

2. **`./scripts/verify-shell-bridge.sh` is broken: it verifies files that do
   not exist.** It copies `shell/outloud.zsh` and exports
   `OUTLOUD_BRIDGE_SOCKET`; the repo ships `shell/aqua.zsh` reading
   `AQUA_BRIDGE_SOCKET`. Reproduction: `./scripts/verify-shell-bridge.sh` →
   `cp: cannot stat '.../shell/outloud.zsh': No such file or directory`.
   docs/shell-integration.md has the same rot (documents `shell/outloud.*`,
   `OUTLOUD_BRIDGE_SOCKET`, `OUTLOUD_BRIDGE_KEY`; code implements
   `aqua.*`/`AQUA_BRIDGE_*`), and the documented socket path
   (`.../outloud/shell.sock`) differs from the real one
   (`.../aqua/shell.sock`). This is exactly the incomplete-rename class that
   already bit this repo twice. **Code + doc fix.** (The underlying bridge is
   sound: rerunning the same script with the real filenames passes the full
   pty verification including undo.)

3. **The project's own gate says no.** `./scripts/preflight.sh` →
   `VERDICT: NOT SAFE TO SHIP`, failing `stale-product-names` with 19
   user-visible "Aqua" strings, including the menu-bar diagnostics header
   ("Aqua diagnostics"), the error dialog title ("Aqua"), the config file
   header comment, the rc-file marker `# aqua shell-bridge`, and the
   `shell/aqua.*` plugin filenames. Either finish the rename or explicitly
   allowlist each survivor; shipping with the gate red makes the gate
   meaningless. **Code fix**, mostly string renames. Note the plugin
   filenames interact with blocker 2: fix them together, in one direction.

### Should fix, would not hold the release alone

4. **Menu-bar "Run Diagnostics" writes an unredacted report to disk.**
   `menuhost.rs::run_diagnostics` formats raw `outcome.detail`/`remedy`
   strings (which contain absolute home paths, hence the username) into a
   file beside the config, explicitly intended to be attached to bug reports.
   The redaction layer exists (`diag::redact::bundle`) and the CLI doctor
   uses it for `--report`; this path bypasses it. No transcript content is
   at risk today (checks never capture field text), but the privacy story
   says redacted *by construction*. One-line **code fix**: run the output
   through `redact::bundle` / `scrub_free_text`.

5. **Bundle identifier is `dev.hexavoice.hexad`.** TCC pins Accessibility
   grants to the identifier, so renaming it *after* v0.1.0 silently kills
   every user's grant. Decide the final identifier now, before first public
   release, not after. **Human decision + code fix.**

### Human-only, already honestly disclosed

6. **Unsigned, un-notarized builds.** Confirmed: `spctl` rejects the bundle.
   README and the known-limitations table already state this plainly and the
   build-locally path works, so a *source-only* v0.1.0 is honest without it.
   Distributing a downloadable .app requires Apple Developer Program
   enrolment, a Developer ID certificate, and notarization
   (docs/signing-runbook.md). **Only the human can do this.**

## 3. Not blockers (checked, fine)

- Dictation, GUI edit-by-voice, doctor, streaming probe: all reproduce (§1).
- Licence hygiene: deny clean, MIT everywhere, weight licences separated and
  the closed Apple recognizer disclosed in the README's second paragraph.
- Shell-bridge security posture matches its threat model, including the
  no-execution-verb invariant and same-uid peer gating; residual risk is
  stated honestly in docs/shell-integration.md.
- Preflight's other nine checks pass, including overlay focus-stealing,
  idle CPU (0.4%), headless build, and the latency gate.
- README latency numbers (131–215ms) are consistent with what this audit
  measured (119–163ms across transports).

## 4. Release steps, once blockers 1–3 (and ideally 4–5) land

1. `./scripts/preflight.sh` → must say SAFE TO SHIP (this now also proves
   blocker 3 is gone).
2. `./scripts/verify-shell-bridge.sh` → must pass as shipped (proves 2).
3. Re-run the README shell flow *as written* by a human (proves 1).
4. Fresh-clone check: `./scripts/verify-head.sh` and
   `cargo test -p outloud --test docs_paths` on a clean clone.
5. Tag `v0.1.0`, source-only release notes stating: macOS 26+ for
   recognition, build locally (unsigned), Windows untested, Linux not yet.

## Verdict

**NO-GO** for v0.1.0 today. The core product is real: dictation, in-place
edit-by-voice, and the bridge protocol all reproduce with measured latencies
that beat the README's own claims. But the headline shell feature does not
work by voice as the README describes it (blocker 1), the script that is
supposed to prove it is broken by an incomplete rename (blocker 2), and the
project's own preflight gate currently fails (blocker 3). All three are
hours of work, not weeks: fix, re-run §4, and this flips to GO for a
source-only release. Signed binary distribution (blocker 6) remains a
separate, human-gated milestone.
