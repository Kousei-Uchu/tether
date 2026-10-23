#!/usr/bin/env bash
# Builds the Rust library for iPhone and the iOS simulator, generates the Swift bindings, and
# packages everything as apps/ios/Frameworks/TetherFFI.xcframework. Run on a Mac with Xcode.
set -euo pipefail
cd "$(dirname "$0")/.."

TARGETS=(aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios)
rustup target add "${TARGETS[@]}"
for t in "${TARGETS[@]}"; do
  cargo build --release -p tether-ffi --target "$t"
done

# The bindings are generated from a library built for this Mac, which carries the same interface.
cargo build -p tether-ffi
GEN=apps/ios/Tether/Generated
rm -f "$GEN"/*.swift
cargo run -q -p tether-ffi --features bindgen --bin uniffi-bindgen -- \
  generate --library target/debug/libtether_ffi.dylib --language swift --out-dir build/ios/generated
cp build/ios/generated/tether_ffi.swift "$GEN/"

HEADERS=build/ios/headers
rm -rf "$HEADERS" && mkdir -p "$HEADERS"
cp build/ios/generated/tether_ffiFFI.h "$HEADERS/"
cp build/ios/generated/tether_ffiFFI.modulemap "$HEADERS/module.modulemap"

# One library per platform slice: device, and a fat simulator library for both Mac chips.
lipo -create \
  target/aarch64-apple-ios-sim/release/libtether_ffi.a \
  target/x86_64-apple-ios/release/libtether_ffi.a \
  -output build/ios/libtether_ffi_sim.a

rm -rf apps/ios/Frameworks/TetherFFI.xcframework
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libtether_ffi.a -headers "$HEADERS" \
  -library build/ios/libtether_ffi_sim.a -headers "$HEADERS" \
  -output apps/ios/Frameworks/TetherFFI.xcframework

echo "Done. Next: cd apps/ios && xcodegen && open Tether.xcodeproj"
