#!/bin/bash
# Build the real OuraApp: shared Rust core via the UniFFI OuraCore.xcframework,
# SwiftUI "Observatory" UI, real data from the bundled oura.db. Runs on the sim.
set -euo pipefail
cd "$(dirname "$0")/../../.."
REPO="$PWD"
APPDIR="$REPO/apps/ios/OuraApp"
GEN="$REPO/apps/ios/generated"
XCF="$REPO/apps/ios/OuraCore.xcframework/ios-arm64-simulator"
BUILD="$APPDIR/build"
APP="$BUILD/OuraApp.app"
TRIPLE="arm64-apple-ios17.0-simulator"
DEV="${1:-Codex-iPhone-17}"

echo "==> refresh xcframework staticlib (release, iOS sim)"
cargo build -p oura-core --release --target aarch64-apple-ios-sim >/dev/null
cp "$REPO/target/aarch64-apple-ios-sim/release/liboura_core.a" "$XCF/liboura_core.a"

echo "==> compile SwiftUI app + UniFFI bindings, link core"
rm -rf "$BUILD"; mkdir -p "$APP"
xcrun -sdk iphonesimulator swiftc \
    -target "$TRIPLE" -parse-as-library \
    -I "$GEN/headers" \
    "$GEN/oura_core.swift" "$APPDIR/Theme.swift" "$APPDIR/OuraApp.swift" \
    "$APPDIR/BLETransport.swift" "$APPDIR/RingSync.swift" \
    -L "$XCF" -loura_core \
    -o "$APP/OuraApp"
cp "$APPDIR/Info.plist" "$APP/Info.plist"

echo "==> bundle oura.db (real synced data)"
cp "$REPO/oura.db" "$APP/oura.db"

echo "==> boot + install + launch + screenshot"
xcrun simctl boot "$DEV" 2>/dev/null || true
xcrun simctl bootstatus "$DEV" -b >/dev/null 2>&1 || true
xcrun simctl install "$DEV" "$APP"
xcrun simctl launch "$DEV" md.thomas.openoura
sleep 2
xcrun simctl io "$DEV" screenshot "$BUILD/screenshot.png"
echo "screenshot: $BUILD/screenshot.png"
