#!/usr/bin/env bash
# Packages the Electron app for the machine it runs on: .dmg and .zip on macOS, AppImage and
# .deb on Linux. Needs Rust, Node 22+. Output lands in apps/desktop/dist/.
# Native code cannot be cross compiled here, so run this on each OS (CI does).
set -euo pipefail
cd "$(dirname "$0")/../apps/desktop"
npm ci
npm run dist
echo "Done. See apps/desktop/dist/"
