#!/bin/bash
# Final step of the runtime spike: link the LibTorch lite interpreter (built for
# the iOS simulator by build_libtorch_ios.sh) into the spike app, load a real
# .ptl on the simulator, and confirm the on-device result matches macOS/Python.
# Run this AFTER build_libtorch_ios.sh succeeds.
set -euo pipefail
cd "$(dirname "$0")/../../.."
REPO="$PWD"
SPIKE="$REPO/apps/ios/spike"
LT="${LIBTORCH_IOS:-$REPO/local/libtorch-ios/pytorch/build_ios/install}"
# torch 2.9 builds .dylib (not .a); we link those + set an rpath to find them.
BUILD="$SPIKE/build-torch"
APP="$BUILD/OuraSpike.app"
TRIPLE="arm64-apple-ios17.0-simulator"
DEV="${1:-Codex-iPhone-17}"

[ -d "$LT/lib" ] || { echo "missing libtorch iOS install at $LT (run build_libtorch_ios.sh)"; exit 1; }

echo "==> rust core (iOS sim)"
cargo build -p oura-ffi --release --target aarch64-apple-ios-sim >/dev/null

echo "==> compile TorchBridge.mm (lite interpreter)"
rm -rf "$BUILD"; mkdir -p "$APP"
xcrun -sdk iphonesimulator clang++ -std=c++17 -fobjc-arc -O1 \
    -target "$TRIPLE" \
    -I"$LT/include" -I"$LT/include/torch/csrc/api/include" \
    -c "$SPIKE/TorchBridge.mm" -o "$BUILD/TorchBridge.o"

echo "==> compile + link SwiftUI app (rust + torch + bridge)"
xcrun -sdk iphonesimulator swiftc \
    -target "$TRIPLE" -parse-as-library -D TORCH \
    -import-objc-header "$SPIKE/bridge.h" \
    "$SPIKE/App.swift" "$BUILD/TorchBridge.o" \
    -L "$REPO/target/aarch64-apple-ios-sim/release" -loura_ffi \
    -lc++ \
    -L "$LT/lib" -ltorch -ltorch_cpu -lc10 \
    -Xlinker -rpath -Xlinker "@executable_path/Frameworks" \
    -o "$APP/OuraSpike"
cp "$SPIKE/Info.plist" "$APP/Info.plist"
# embed the torch dylibs so the app finds them via @rpath on the simulator
mkdir -p "$APP/Frameworks"
cp "$LT/lib/libtorch.dylib" "$LT/lib/libtorch_cpu.dylib" \
   "$LT/lib/libc10.dylib" "$LT/lib/libtorch_global_deps.dylib" "$APP/Frameworks/"
# ship a model so the app can load it on-device
cp "$REPO/notes/models/mobile/steps_motion_decoder_2_0_0.ptl" "$APP/model.ptl"

echo "==> install + launch + screenshot"
xcrun simctl boot "$DEV" 2>/dev/null || true
xcrun simctl bootstatus "$DEV" -b >/dev/null 2>&1 || true
xcrun simctl install "$DEV" "$APP"
xcrun simctl launch "$DEV" com.openoura.spike
sleep 2
xcrun simctl io "$DEV" screenshot "$BUILD/torch_screenshot.png"
echo "screenshot: $BUILD/torch_screenshot.png"
