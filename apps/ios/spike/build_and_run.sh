#!/bin/bash
# Build the shared Rust core for the iOS simulator, link it into a tiny SwiftUI
# app, and run that app on a booted simulator — proof the core runs on iOS.
set -euo pipefail
cd "$(dirname "$0")/../../.."   # repo root
REPO="$PWD"
SPIKE="$REPO/apps/ios/spike"
BUILD="$SPIKE/build"
APP="$BUILD/OuraSpike.app"
TRIPLE="arm64-apple-ios17.0-simulator"

echo "==> 1/4 cargo build (iOS sim staticlib)"
cargo build -p oura-ffi --release --target aarch64-apple-ios-sim >/dev/null

echo "==> 2/4 swiftc + link"
rm -rf "$BUILD"; mkdir -p "$APP"
xcrun -sdk iphonesimulator swiftc \
    -target "$TRIPLE" \
    -parse-as-library \
    -import-objc-header "$SPIKE/bridge.h" \
    "$SPIKE/App.swift" \
    -L "$REPO/target/aarch64-apple-ios-sim/release" -loura_ffi \
    -o "$APP/OuraSpike"
cp "$SPIKE/Info.plist" "$APP/Info.plist"

echo "==> 3/4 boot simulator + install"
DEV="${1:-Codex-iPhone-17}"
xcrun simctl boot "$DEV" 2>/dev/null || true
xcrun simctl bootstatus "$DEV" -b >/dev/null 2>&1 || true
xcrun simctl install "$DEV" "$APP"

echo "==> 4/4 launch + screenshot"
xcrun simctl launch "$DEV" com.openoura.spike
sleep 2
SHOT="$BUILD/spike_screenshot.png"
xcrun simctl io "$DEV" screenshot "$SHOT"
echo "screenshot: $SHOT"
