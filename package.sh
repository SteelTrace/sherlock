#!/usr/bin/env bash
# Local packaging helper. Official multi-platform archives use the names and layout
# produced by .github/workflows/release.yml (sherlock-{linux,macos}-*.tar.gz).
set -euo pipefail
cargo build --release
mkdir -p dist/sherlock
cp target/release/sherlock dist/sherlock/
tar -czf sherlock.tar.gz -C dist sherlock
