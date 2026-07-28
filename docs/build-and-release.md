# Build and release

How this project is built, packaged, signed, and shipped on every supported
platform, and why each decision was made. Every section names the failure mode
the decision guards against, because a build system is a collection of
prevented incidents.

Quick map of what lives where:

| Concern | File |
|---|---|
| PR CI (lint, test, MSRV, cross-target compile, headless, repro) | `.github/workflows/ci.yml` |
| Release pipeline (build, sign, notarize, package, publish) | `.github/workflows/release.yml` |
| Nightly CVE scan | `.github/workflows/audit.yml` |
| Toolchain pin | `rust-toolchain.toml` |
| Licence and CVE policy | `deny.toml` |
| Cross-compilation containers | `Cross.toml` |
| Per-target cargo settings | `.cargo/config.toml` |
| Hermetic dev shell / nix build | `flake.nix` |
| Lint + test entry point | `scripts/ci-check.sh` |
| Compliance + SBOM entry point | `scripts/ci-compliance.sh` |
| macOS release artifact | `scripts/build-macos-release.sh` |
| Windows release artifacts | `scripts/build-windows.sh` |
| Linux release artifacts | `scripts/build-linux.sh` |
| Headless daemon | `scripts/build-headless.sh` |
| Reproducibility flags + double-build check | `scripts/build-repro.sh` |
| AVX2 baseline verification | `scripts/ci-verify-baseline.sh` |

## CI

### Matrix

Six shipped platforms: macOS arm64 and x86_64, Windows x86_64 and arm64,
Linux x86_64 and aarch64, with Linux built against both glibc and musl. The
PR-time `build-matrix` job compiles all eight triples on every pull request.
This is deliberately cheap (compile only, no packaging, no signing) because
its job is to catch `cfg()` and dependency breakage the day it is written.
The failure mode without it: a release tag is the first time anyone learns
the Windows build broke three weeks ago.

Cross-compilation strategy, per target:

- **macOS x86_64 and arm64** both build natively on the `macos-15` (Apple
  Silicon) runner. Apple's toolchain cross-compiles between its own
  architectures with only a `rustup target add`, so no container or extra
  linker is involved.
- **Windows aarch64** cross-compiles on the x86_64 `windows-2025` runner.
  MSVC ships the arm64 cross-linker in every Visual Studio install, so again
  only `rustup target add` is needed. There are no arm64 Windows GitHub
  runners to test on, which means arm64 Windows is *built* in CI but only
  *executed* by users. That gap is acceptable for a spike and is written down
  here so nobody mistakes a green build for a tested build.
- **Linux x86_64 (glibc and musl)** builds natively on `ubuntu-24.04`, with
  `musl-tools` supplying the musl C toolchain.
- **Linux aarch64 (both libcs)** goes through
  [`cross`](https://github.com/cross-rs/cross) with images pinned in
  `Cross.toml`. Why cross rather than installing `gcc-aarch64-linux-gnu` on
  the runner: the container pins the sysroot, so the glibc *floor* of the
  shipped binary (2.31, from the Ubuntu 20.04 image) is a decision recorded
  in a file rather than an accident of the runner's package archive. The
  failure mode being avoided: a runner image update silently raising the
  glibc requirement and breaking every Debian 11 user with a linker error
  they report as "the app does not start".

QEMU-based test execution for aarch64 was considered and rejected for now:
the crates with interesting behaviour (`edit-intent`) are pure and already
tested on x86_64, and the FFI crate (`ax-edit`) cannot be exercised in a
container anyway because there is no macOS accessibility bus in one.

### Cache strategy

`Swatinem/rust-cache@v2` everywhere, instead of hand-rolled `actions/cache`
keys, because it already solves the problems hand-rolled keys always develop:
it keys on lockfile + toolchain + job, prunes artifacts that no longer match
the dependency graph, and skips caching the workspace's own crates (which
change every commit and would otherwise churn the cache to death).

Layout decisions:

- **One shared cache per cheap job family** (`check`, `compliance`, `msrv`,
  `headless`) because they compile the same host-target artifacts.
- **One cache per target triple** for the build matrix and release jobs
  (`key: <triple>`), because object files for different triples never hit,
  and mixing them just evicts useful entries. The failure mode: a "shared"
  cache that thrashes so hard every job is effectively cold while still
  paying upload/download time.
- **`CARGO_INCREMENTAL=0`** in CI. Incremental artifacts are large, keyed to
  mtimes that never match across runners, and the restored registry cache
  already provides the warm start.
- The **repro job uses no cache at all**, deliberately: proving a clean
  double-build is deterministic requires the build to actually be clean.

### One command, two places

Anything longer than two lines runs through a `scripts/ci-*.sh` script rather
than inline workflow YAML. Contributors run the identical script locally.
The failure mode: flag drift between the workflow and the README ("works on
my machine" in both directions), which is among the most expensive classes of
CI bug because each occurrence wastes a full push-wait-read cycle.

## MSRV policy

- **MSRV: Rust 1.85** (edition-2021 workspace; 1.85 also covers the
  edition-2024 dependencies that are starting to appear on crates.io).
- CI enforces it with a real build: `cargo +1.85.0 build --workspace
  --locked` in the `msrv` job. A claimed-but-untested MSRV is worse than
  none, because downstream packagers discover the lie instead of us.
- The **development toolchain is pinned separately** in
  `rust-toolchain.toml` (currently 1.95.0) and is intentionally newer. The
  MSRV is a promise to downstream (Debian, Fedora, nixpkgs ship old
  compilers); the pin is what we develop and release with. Coupling them
  would either freeze development on an old compiler or turn every routine
  toolchain bump into a compatibility break.
- Raising the MSRV is allowed but is a minor-version event with a changelog
  entry, and never for convenience of a single dependency without checking
  what downstream ships.

## Licence and CVE compliance

`deny.toml` + `scripts/ci-compliance.sh`, run on every PR, on every release,
and nightly (`audit.yml`) because advisories are published against code we
already shipped, with no commit to trigger PR CI.

Key policy decisions:

- **Allow-list, not deny-list.** The licence check enumerates the permitted
  MIT-compatible licences (MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0,
  CC0). Anything else, including every GPL, AGPL, LGPL, SSPL, and unknown
  licence, **fails the build**. A deny-list fails open when a licence id we
  never thought of appears; an allow-list fails closed, which is the correct
  direction for legal exposure in an MIT project.
- **All eight shipped targets are checked** (`[graph] targets` in
  `deny.toml`), because a dependency that is copyleft only behind a Linux
  `cfg` would pass a laptop check on macOS.
- **Git dependencies are banned** (`unknown-git = "deny"`). They are
  invisible to the advisory database and unpinnable by version. Exceptions
  require a rev-pinned entry in `allow-git` with a written justification.
  (Handy's Cargo.toml is a tour of why teams end up with git deps; when we
  need one, we will take it consciously, the same way.)
- **Both `cargo deny` and `cargo audit`** run, despite overlap: deny is the
  policy engine, audit maps cleanly to the RustSec database and serves as
  cheap insurance if either database lags. Both tools are version-pinned in
  the script so policy semantics cannot change between two CI runs without a
  diff.

## macOS

`scripts/build-macos-release.sh` produces: universal binary → `.app` bundle →
Developer ID signature → notarization → stapling → signed DMG.

- **Universal binary via `lipo`.** Both slices are built from the same
  commit and toolchain, then merged. One download works on every Mac and,
  more importantly for this project, TCC sees *one* bundle identity instead
  of two per-arch apps with separate permission grants.
- **Hardened runtime + secure timestamp** (`--options runtime --timestamp`)
  because notarization requires the former and certificate expiry breaks
  non-timestamped signatures retroactively.
- **Notarization via `notarytool --wait`**, then **stapling**. Stapling
  attaches the ticket to the bundle so Gatekeeper validates offline; without
  it, first launch on a network-restricted machine fails even though
  notarization succeeded. That failure mode is nasty precisely because the
  release "worked" for everyone who tested it online.
- **DMG via `hdiutil`** (no third-party dmg tooling): a read-only UDZO image
  with an `/Applications` symlink is everything a CLI-sized app needs, and
  one fewer unpinned dependency in the release path.
- CI keeps the certificate in an **ephemeral keychain** that dies with the
  runner, never the login keychain.

### How real signing solves the TCC problem

`docs/macos-permissions.md` documents the M0 finding: TCC records an
Accessibility grant against the binary's `cdhash` when the signature is
ad-hoc. Every rebuild produces a new cdhash, so the grant silently orphans
itself while the System Settings toggle still reads "on". During the spike
this cost real debugging hours and the workaround is
`tccutil reset Accessibility dev.hexavoice.spike` after every rebuild.

A Developer ID signature changes what TCC pins to. With a certificate-backed
signature, the app's *designated requirement* is anchored to the signing
certificate chain and team identifier plus the bundle id, not to a per-build
hash. TCC stores the approval against that requirement, so **every future
build signed by the same team satisfies the stored grant**. Users grant
Accessibility once; updates keep it. This is why the certificate is a
permissions requirement, not merely a distribution nicety, and why the
release script treats ad-hoc signing as a loudly-labelled local-only mode.

## Windows

`scripts/build-windows.sh` produces a portable zip, an NSIS installer, and an
MSI when WiX is present.

- **NSIS primary, MSI secondary.** NSIS is small, per-user (no UAC), and
  needs no runtime. MSI exists for enterprise deployment tooling (Intune,
  GPO). The MSI `UpgradeCode` is fixed forever; changing it orphans
  installed copies from the upgrade path.
- **Authenticode**: signed with `signtool /fd sha256` plus an RFC3161
  timestamp so signatures survive certificate expiry. Signing is gated on a
  secret so forks build unsigned artifacts instead of failing.
- **SmartScreen reputation** is per-certificate and starts at zero for OV
  certificates, building only with install volume. An EV certificate (or
  Azure Trusted Signing) grants reputation immediately. For software whose
  whole purpose is reading other apps' text and injecting input, exactly the
  behaviours AV heuristics flag, budget for EV/Trusted Signing before the
  first external Windows tester, the same way the Apple Developer ID is
  budgeted before the first external macOS tester. The failure mode of
  skipping this: weeks of "Windows protected your PC" walls that users
  reasonably interpret as malware.

### The pre-Haswell AVX2 crash class

Handy's `Cargo.toml` documents the incident this section prevents from
recurring. Their Windows build pulled pyke's prebuilt ONNX Runtime, which is
compiled with a global `/arch:AVX2` baseline. AVX2/BMI2 instructions executed
inside a **static initializer**, before `main()`, so on any pre-Haswell CPU
(Sandy Bridge, Ivy Bridge, older AMD) the process died with an
illegal-instruction fault before any runtime CPU-detection code could run.
The crash is undebuggable from a user report: no log line, no window,
"nothing happens".

Three layered defences here, because the threat arrives via dependencies, not
via our own flags:

1. **Never raise the compile baseline**: no `-C target-cpu` anywhere in the
   repo, documented in `.cargo/config.toml` so a well-meaning "optimize the
   build" PR has to argue with the comment. Shipped x86_64 binaries target
   the SSE2 baseline; ISA selection belongs to runtime dispatch inside
   whichever ML crates arrive later.
2. **Ban the known-bad packaging** in `deny.toml`: the `ort` crate (pyke's
   prebuilt runtime) is on the ban list with a pointer here, so adding ONNX
   support forces a review conversation about which runtime build gets
   linked.
3. **Verify the artifact**: `scripts/ci-verify-baseline.sh` disassembles the
   shipped x86_64 binaries (Windows and Linux release jobs) and fails on any
   AVX/AVX2/BMI2 instruction. Today the correct count is zero because nothing
   in the dependency graph does SIMD. When a legitimately runtime-dispatching
   SIMD dependency is added, the check tightens to scan only initializer
   sections rather than being deleted.

## Linux

`scripts/build-linux.sh` produces a tarball, AppImage, `.deb`, `.rpm`, and a
Flatpak manifest per target.

- **glibc floor is 2.31** (Debian 11 / Ubuntu 20.04), set by the `cross`
  image for aarch64 and by the runner for x86_64. Raising it is treated like
  an MSRV bump. The **musl builds are fully static** and exist precisely for
  machines outside that floor.
- **AppImage** for "download one file, run it anywhere without root".
- **`.deb` and `.rpm`** because distro users expect them, and because the
  package metadata is where runtime dependencies get declared once the input
  backend lands.
- **Flatpak** is the hard case and is documented in the generated manifest
  itself: an input-injection accessibility tool is the worst possible
  sandbox citizen. The manifest requests `--socket=wayland`,
  `--socket=fallback-x11`, `--share=ipc`, and `--talk-name=org.a11y.Bus`
  (AT-SPI2, for *reading* the focused field). Input *injection* on Wayland
  from the sandbox goes through the **RemoteDesktop portal**
  (`org.freedesktop.portal.RemoteDesktop`), which brokers a user consent
  prompt and, with xdg-desktop-portal ≥ 1.16, can persist the grant. We
  explicitly do not request `--device=all` or `/dev/uinput`: Flathub review
  would reject it, and rightly.

### Wayland vs X11 runtime detection

Both backends compile into every Linux binary; the choice is made at
process start, not at build time, because a compile-time choice would force
distros to ship two packages and users to know which session type they run.

Detection order: `WAYLAND_DISPLAY` first, then `DISPLAY`. Under XWayland both
variables are set, and native Wayland must win or the app silently degrades
to the XWayland compatibility path and cannot see native-Wayland windows.

Capability reality to design against: X11 offers XTEST plus AT-SPI2 with no
permission model. Wayland has no global input injection by design; the
options are the RemoteDesktop portal (works everywhere, consent prompt),
`zwp_virtual_keyboard_v1` / `input-method-v2` (wlroots-family compositors
only), and libei (GNOME 45+). The packaging above keeps all three paths
open.

## Headless builds

`scripts/build-headless.sh` builds the daemon used over SSH with zero display
server, preferring the static musl target so the binary can be `scp`'d to any
Linux box.

The contract: `spike-cli` grows a **real cargo feature named `headless`**
(`--no-default-features --features headless`) that compiles out every
display-touching path. The feature lives in `crates/`, which this build
system does not own, so the script probes `cargo metadata` for it: when
present it is used, when absent the script builds default features. Either
way the script then **mechanically verifies** the produced binary links no
display libraries (`ldd` grep for X11/Wayland/GTK on Linux, `otool` grep for
AppKit/CoreGraphics on macOS), and CI smoke-tests the binary with `DISPLAY`
and `WAYLAND_DISPLAY` explicitly unset. That link check is the tripwire: the
day a GUI dependency lands un-gated, the headless CI job fails at build time
instead of a user discovering it at runtime on a server.

Verification is on the artifact rather than the feature graph because
transitive default features are exactly how display libraries sneak into
"headless" builds in practice.

## Reproducible builds and SBOM

Reproducibility has three legs, each closed in a named place:

1. **Toolchain**: pinned exactly in `rust-toolchain.toml`; the nix flake
   derives its compiler from the same file so the two cannot drift.
2. **Paths**: `scripts/build-repro.sh` remaps the checkout and `CARGO_HOME`
   to stable names with `--remap-path-prefix`, so two checkouts at different
   paths (or two CI runners) produce identical binaries.
3. **Time**: `SOURCE_DATE_EPOCH` is pinned to the commit timestamp, the
   reproducible-builds.org convention honoured by tar and dpkg. rustc itself
   does not embed wall-clock time, but packaging steps do.

The claim is *tested*, not asserted: the `repro` CI job builds twice from
clean with `REPRO_VERIFY=1` and fails unless the hashes match. When it fails,
`diffoscope` on the two binaries locates the nondeterministic section. The
nix flake is the stronger, opt-in version of the same property: `nix build`
runs in a sandbox with no network and no host toolchain.

**SBOM**: `scripts/ci-compliance.sh` generates CycloneDX 1.5 JSON from
`Cargo.lock` via `cargo-cyclonedx`, one document per workspace crate, and
uploads them with every CI run and release. The SBOM is generated from the
lockfile that was actually built (`--locked` everywhere), so it describes
the shipped artifact rather than the manifest's wishes. Consumers: licence
review, and matching future CVEs against released versions without
re-resolving old dependency graphs.

## Release flow

```
git tag v0.2.0 && git push --tags
```

1. `compliance` job re-runs lint, tests, deny, audit, SBOM. A release must
   never ship what PR CI would have rejected.
2. Platform jobs build, sign (when secrets exist), and package in parallel.
3. `publish` collects artifacts, writes `SHA256SUMS`, and creates a **draft**
   GitHub Release. A human inspects artifacts before publishing; the
   automation's job is to make that inspection five minutes, not to remove
   it. `workflow_dispatch` runs the whole pipeline without tagging, for
   rehearsal.

Secrets required for fully-signed releases are listed at the top of
`.github/workflows/release.yml`. With no secrets configured the pipeline
still goes green and produces unsigned artifacts, each script printing a
loud warning, because a fork or a pre-certificate tag failing red teaches
people to ignore red.
