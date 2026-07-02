#!/bin/bash
# Build the shared Rust core (oura-core) for iOS device + simulator and package the
# OuraCore.xcframework the app links. Re-run after changing any Rust core code.
#
# The simulator-only dev harness (OuraApp/build_run.sh) refreshes just the sim slice;
# this produces BOTH slices, which a device build / TestFlight archive needs.
set -euo pipefail
cd "$(dirname "$0")/../.."
REPO="$PWD"
HEADERS="$REPO/apps/ios/generated/headers"   # UniFFI header + module.modulemap
OUT="$REPO/apps/ios/OuraCore.xcframework"
LIB="liboura_core.a"

for t in aarch64-apple-ios aarch64-apple-ios-sim; do
  rustup target list --installed | grep -qx "$t" || rustup target add "$t"
done

echo "==> build oura-core (release) for device + simulator"
cargo build -p oura-core --release --target aarch64-apple-ios
cargo build -p oura-core --release --target aarch64-apple-ios-sim

rm -rf "$OUT"
echo "==> create xcframework"
xcodebuild -create-xcframework \
  -library "$REPO/target/aarch64-apple-ios/release/$LIB"     -headers "$HEADERS" \
  -library "$REPO/target/aarch64-apple-ios-sim/release/$LIB" -headers "$HEADERS" \
  -output "$OUT"
echo "✓ $OUT"
