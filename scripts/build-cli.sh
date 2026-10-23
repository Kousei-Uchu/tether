#!/usr/bin/env bash
# Builds the `tether` command in release mode and copies it to dist/ named for this platform.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release -p tether-cli
mkdir -p dist
os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
cp target/release/tether "dist/tether-$os-$arch"
echo "Done. dist/tether-$os-$arch"
