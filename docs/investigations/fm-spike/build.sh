#!/usr/bin/env bash
# Build and run the Foundation Models feasibility spike.
#
#   ./build.sh
#
# Requires macOS 26+ and a Swift toolchain. Does NOT require the user to have
# enabled Apple Intelligence: demonstrating clean degradation when they have
# not is one of the things this spike checks.
set -euo pipefail
cd "$(dirname "$0")"

echo "== building the Swift C-ABI shim"
swiftc -O -emit-library -static -o liboutloud_fm.a shim.swift

echo "== building and running the Rust caller"
cd rust
exec cargo run --release
