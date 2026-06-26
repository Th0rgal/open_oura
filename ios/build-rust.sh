#!/usr/bin/env bash
# Build the Rust core (oura-ffi) for iOS device + simulator and package it as an
# XCFramework that the Xcode project links. Re-run after changing any Rust code.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DEVICE_TARGET="aarch64-apple-ios"
SIM_TARGET="aarch64-apple-ios-sim"   # Apple-Silicon simulator
LIB="libouraffi.a"
HEADERS="$ROOT/ios/OuraFFI/include"
OUT="$ROOT/ios/OuraFFI/OuraFFI.xcframework"

echo "▸ building Rust core for $DEVICE_TARGET and $SIM_TARGET ..."
cargo build --release -p oura-ffi --target "$DEVICE_TARGET"
cargo build --release -p oura-ffi --target "$SIM_TARGET"

rm -rf "$OUT"
echo "▸ creating XCFramework ..."
xcodebuild -create-xcframework \
  -library "$ROOT/target/$DEVICE_TARGET/release/$LIB" -headers "$HEADERS" \
  -library "$ROOT/target/$SIM_TARGET/release/$LIB" -headers "$HEADERS" \
  -output "$OUT"

echo "✓ $OUT"
