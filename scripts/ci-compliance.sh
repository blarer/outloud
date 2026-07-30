#!/usr/bin/env bash
# CI: licence + CVE compliance and SBOM generation.
#
# Two distinct tools on purpose:
#   - cargo-deny: policy engine (licences, bans, sources) driven by deny.toml.
#     This is where "any GPL/AGPL dependency is a build failure" is enforced.
#   - cargo-audit: RustSec advisory check against Cargo.lock. Overlaps with
#     deny's advisories check, but audit's output maps cleanly into SARIF and
#     the overlap is cheap insurance if one database lags the other.
#
# SBOM: CycloneDX from the actual lockfile, so the SBOM describes what was
# built rather than what the manifest wished for. Attached to every release.
#
# Failure modes:
#   - tool not installed        -> installed here, pinned, so CI and laptops
#     agree on the tool version and policy semantics.
#   - advisory DB fetch flaking -> audit retried once; a persistent failure
#     should fail CI, since silently skipping the CVE gate is worse.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Pinned tool versions: an unpinned `cargo install` means the compliance
# policy can change semantics under us between two CI runs.
DENY_VERSION="0.20.2"   # >=0.20 required: older releases choke on CVSS 4.0 advisory entries
AUDIT_VERSION="0.22.2"
CYCLONEDX_VERSION="0.5.9"

ensure() { # ensure <binary> <crate> <version>
    local bin="$1" crate="$2" version="$3"
    if ! "$bin" --version 2>/dev/null | grep -q "$version"; then
        echo "==> installing $crate $version"
        cargo install --locked "$crate" --version "$version"
    fi
}

ensure cargo-deny cargo-deny "$DENY_VERSION"
ensure cargo-audit cargo-audit "$AUDIT_VERSION"
ensure cargo-cyclonedx cargo-cyclonedx "$CYCLONEDX_VERSION"

echo "==> cargo deny (licences, bans, sources, advisories)"
cargo deny check

echo "==> cargo audit (RustSec advisories)"
cargo audit || { echo "retrying audit once (advisory DB fetch can flake)"; sleep 5; cargo audit; }

echo "==> generating CycloneDX SBOM"
mkdir -p dist/sbom
# --spec-version 1.5 for broad ingestion support (Dependency-Track, Grype).
cargo cyclonedx --spec-version 1.5 --format json --override-filename sbom
# cyclonedx writes one sbom.json next to EVERY workspace member's
# Cargo.toml; collect them under dist/sbom/ with stable names for release
# upload.
#
# `find` over tracked manifests rather than a crates/*/ glob: the glob
# missed the `tests/` member, so its sbom.json was left behind in the tree.
# Being tracked by git, it then re-dirtied the working tree on every
# compliance run with a fresh random serialNumber and timestamp, and rode
# along in unrelated commits -- noise that hides exactly the dependency
# changes an SBOM diff exists to show.
#
# Any member added later is picked up automatically, so this cannot go
# stale the way the hardcoded glob did.
while IFS= read -r sbom; do
    name="$(basename "$(dirname "$sbom")")"
    mv "$sbom" "dist/sbom/$name.cdx.json"
done < <(find . -name sbom.json -not -path './dist/*' -not -path './target/*')

ls -l dist/sbom/

echo "==> ci-compliance OK"
