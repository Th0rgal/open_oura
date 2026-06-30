#!/bin/bash
# Build + run OuraApp WITH on-device torch: links the LibTorch lite interpreter so
# SleepNet runs on the simulator and the hypnogram renders from real inference.
# Needs the iOS libtorch from apps/ios/spike/build_libtorch_ios.sh. The plain
# build_run.sh (no torch, smaller, model-free) stays as the default.
set -euo pipefail
cd "$(dirname "$0")/../../.."
REPO="$PWD"
APPDIR="$REPO/apps/ios/OuraApp"
GEN="$REPO/apps/ios/generated"
XCF="$REPO/apps/ios/OuraCore.xcframework/ios-arm64-simulator"
LT="${LIBTORCH_IOS:-$REPO/local/libtorch-ios/pytorch/build_ios/install}"
BUILD="$APPDIR/build"
APP="$BUILD/OuraApp.app"
TRIPLE="arm64-apple-ios17.0-simulator"
DEV="${1:-Codex-iPhone-17}"

[ -d "$LT/lib" ] || { echo "missing libtorch iOS at $LT (run apps/ios/spike/build_libtorch_ios.sh)"; exit 1; }

echo "==> refresh xcframework staticlib (release, iOS sim)"
cargo build -p oura-core --release --target aarch64-apple-ios-sim >/dev/null
cp "$REPO/target/aarch64-apple-ios-sim/release/liboura_core.a" "$XCF/liboura_core.a"

echo "==> compile TorchBridge.mm (lite interpreter)"
rm -rf "$BUILD"; mkdir -p "$APP"
xcrun -sdk iphonesimulator clang++ -std=c++17 -fobjc-arc -O1 -target "$TRIPLE" \
    -I"$LT/include" -I"$LT/include/torch/csrc/api/include" \
    -c "$APPDIR/TorchBridge.mm" -o "$BUILD/TorchBridge.o"

echo "==> compile SwiftUI app (TORCH) + UniFFI bindings, link core + torch"
xcrun -sdk iphonesimulator swiftc \
    -target "$TRIPLE" -parse-as-library -D TORCH \
    -import-objc-header "$APPDIR/TorchBridge.h" \
    -I "$GEN/headers" \
    "$GEN/oura_core.swift" "$APPDIR/Theme.swift" "$APPDIR/OuraApp.swift" \
    "$APPDIR/SleepStaging.swift" "$BUILD/TorchBridge.o" \
    -L "$XCF" -loura_core \
    -lc++ -lsqlite3 \
    -L "$LT/lib" -ltorch -ltorch_cpu -lc10 \
    -Xlinker -rpath -Xlinker "@executable_path/Frameworks" \
    -o "$APP/OuraApp"
cp "$APPDIR/Info.plist" "$APP/Info.plist"

echo "==> bundle torch dylibs + model + data"
mkdir -p "$APP/Frameworks"
cp "$LT/lib/libtorch.dylib" "$LT/lib/libtorch_cpu.dylib" \
   "$LT/lib/libc10.dylib" "$LT/lib/libtorch_global_deps.dylib" "$APP/Frameworks/"
cp "$REPO/notes/models/mobile/sleepnet_moonstone_1_2_0.ptl" "$APP/sleepnet_moonstone_1_2_0.ptl"
cp "$REPO/oura.db" "$APP/oura.db"
# app icon (asset catalog) for completeness
[ -d "$APPDIR/Assets.xcassets" ] && xcrun actool "$APPDIR/Assets.xcassets" \
    --compile "$APP" --platform iphonesimulator --minimum-deployment-target 17.0 \
    --app-icon AppIcon --output-partial-info-plist "$BUILD/icon.plist" >/dev/null 2>&1 || true

echo "==> boot + install + launch + screenshot"
xcrun simctl boot "$DEV" 2>/dev/null || true
xcrun simctl bootstatus "$DEV" -b >/dev/null 2>&1 || true
xcrun simctl install "$DEV" "$APP"
xcrun simctl launch "$DEV" md.thomas.openoura
sleep 3
xcrun simctl io "$DEV" screenshot "$BUILD/screenshot.png"
echo "screenshot: $BUILD/screenshot.png"
