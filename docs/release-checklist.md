# Release checklist

The ordered human steps for cutting a release. One command answers the
mechanical half: `./scripts/preflight.sh`. This document is the other half,
plus the order in which everything happens.

Split rationale: preflight covers everything a script can verify (gates,
bundle shape, focus behaviour, idle CPU, stale names, doctor remedies).
Humans cover everything that needs eyes or judgement: visual quality, real
dictation feel, and the decision to accept a SKIP. Anything found automatable
during a release should move out of this file and into `scripts/preflight.sh`
in the same PR.

## 0. Preconditions

- [ ] Working tree clean, on the release commit. Do not release from a tree
      with sibling swarms' uncommitted changes: preflight tests the tree it
      is run in, so an uncommitted tree makes its verdict about nothing.
- [ ] All in-flight UI work (overlay redesign, menu-bar mark, framework
      evaluation) either merged or explicitly excluded from this release.
- [ ] `./scripts/verify-head.sh` and `cargo test -p outloud --test docs_paths`
      run on a **fresh clone** (docs/beta-readiness.md: the rename once made
      the documented install path wrong at HEAD while looking fine locally).

## 1. Automated: run preflight

```bash
./scripts/preflight.sh
```

- [ ] Verdict line reads no FAIL. Every FAIL prints a named next action; fix
      it or file it against the owning swarm, then rerun.
- [ ] Read the SKIP lines. A skip is a judgement call, not a pass. Typical
      acceptable skip: `idle-cpu` on a machine without the mic grant.
      Never-acceptable skip on a release machine: `overlay-focus`.

What preflight covers, so nobody re-checks it by hand:

| Check | What it proves |
|---|---|
| ci-check | fmt, clippy -D warnings, `cargo test --workspace` |
| ci-compliance | licences, CVEs, SBOM |
| headless-build | headless binary links no display libraries |
| latency-gate | p50/p99 within budget against a real text field |
| overlay-focus | overlay never steals focus; keystrokes still land in TextEdit while it is on screen |
| app-bundle | binary name, speech helper matches what crates/asr looks for, Info.plist valid, icon present, signature verifies |
| idle-cpu | daemon idles near 0% CPU |
| stale-product-names | no aqua/hexavoice leftovers in user-visible strings (LEGACY_DIRS and `.aqua-oss` model dir are deliberate and allowlisted) |
| doctor-remedies | every remedy names a script and bundle that actually exist |

## 2. Human eyes: visual checks (cannot be automated)

Run `cargo run -p overlay --bin overlay-demo` and watch a full cycle (~40s):

- [ ] **Overlay legibility.** Partial text is readable at arm's length on a
      standard-DPI external display, not only on Retina. Correct: you can
      read the rolling transcript without leaning in; the text never clips
      or overflows the card.
- [ ] **Overlay fade.** State transitions (Listening -> Transcribing ->
      Injecting -> hidden) fade smoothly, no flash of an empty frame, no
      lingering ghost after Injecting. Correct: the panel is simply gone
      within about a second of the text landing; wrong: it pops, flickers,
      or leaves a translucent rectangle behind.
- [ ] **Overlay placement.** The panel sits near the caret / bottom-center
      without covering the text being dictated into, on both a laptop panel
      and an external display, and follows a Space switch.
- [ ] **Error state.** The Error state is visually distinct (red accent) and
      its message fits the card.

Run `cargo run -p overlay --bin status-demo` and watch the menu bar:

- [ ] **Menu-bar mark reads at 18pt.** The glyph is recognisable at actual
      menu-bar size, in both light and dark menu bars, and does not blur or
      alias. Correct: at a glance you can tell idle from listening from
      error; wrong: the states are only distinguishable side by side.
- [ ] **Mark state changes are visible.** Trigger listening and error;
      confirm each is noticeable in peripheral vision without being a
      strobe.

## 3. Human hands: one real dictation

From the built bundle, not from cargo:

```bash
open dist/OutLoud.app
```

- [ ] Grant flow: on a machine (or fresh TCC state, `tccutil reset
      Accessibility <bundle id>`) the permission prompts appear, name the
      right app, and the doctor's advice matches what the screen shows.
- [ ] Dictate one real sentence into TextEdit and one into a terminal.
      Correct: text lands where the cursor was, punctuation is sane, and
      the overlay never becomes the focused window at any point.
- [ ] Speak one edit command ("delete the last word" or similar) and
      confirm it edits only the requested span.

## 4. Version, notes, tag

- [ ] Version bumped everywhere it lives (Cargo.toml workspace version;
      CFBundleVersion / CFBundleShortVersionString in
      scripts/bundle-outloud-macos.sh).
- [ ] Changelog generated from conventional commits; breaking protocol
      changes called out with the protocol version.
- [ ] Re-read docs/pre-release-audit.md blockers and confirm each is fixed
      or consciously accepted in writing.
- [ ] Tag, push the tag, and verify CI is green **on the tag**, not on a
      nearby commit.

## 5. Distribution reality check

- [ ] Signing: ad-hoc means every rebuild invalidates the TCC grant
      (docs/macos-permissions.md). Shipping a downloadable .app requires
      Developer ID + notarization; without it this is a source-install
      release only, and the README must say so.
- [ ] Install from scratch on a machine that has never seen the project
      (per-milestone DoD rule 4), following only the README.
- [ ] The uninstall script removes what this release actually installs:
      skim scripts/uninstall-macos.sh against any new paths added since
      the last release.

## 6. After tagging

- [ ] Re-pin the eval baselines (latency, WER, edit-accuracy) to the
      release's measured numbers so the next cycle's regression gates
      compare against them (per-milestone DoD rule 5).
- [ ] Write the retro: numbers, what was cut, what surprised us.
