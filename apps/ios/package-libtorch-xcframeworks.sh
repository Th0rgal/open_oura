#!/bin/bash
# Package the device + simulator libtorch builds into xcframeworks (one per dylib),
# so an Xcode archive links/embeds the right slice per SDK automatically.
#
# Prereq: build BOTH slices first —
#   apps/ios/spike/build_libtorch_ios.sh          # simulator → build_ios/install
#   apps/ios/spike/build_libtorch_ios.sh device   # device    → build_ios_device/install
#
# Output: apps/ios/libtorch-xcframeworks/<name>.xcframework (gitignored, local artifact).
set -euo pipefail
REPO="$HOME/work/open_oura"
LT="$REPO/local/libtorch-ios/pytorch"
SIM="$LT/build_ios/install/lib"
DEV="$LT/build_ios_device/install/lib"
OUT="$REPO/apps/ios/libtorch-xcframeworks"

for d in "$SIM" "$DEV"; do
    [ -d "$d" ] || { echo "missing $d — build that slice first"; exit 1; }
done

# libtorch builds with minos = the SDK's iOS (e.g. 26.4); normalize to the app's
# deployment target so it links cleanly and runs on iOS 17+ (vtool platform 2=iOS,
# 7=iOS-simulator). The lite CPU interpreter uses no iOS-26-only symbols.
MIN=17.0
echo "==> normalize minos → $MIN"
for lib in "$DEV"/*.dylib; do vtool -set-build-version 2 "$MIN" "$MIN" -replace -output "$lib" "$lib" >/dev/null; done
for lib in "$SIM"/*.dylib; do vtool -set-build-version 7 "$MIN" "$MIN" -replace -output "$lib" "$lib" >/dev/null; done

rm -rf "$OUT"; mkdir -p "$OUT"
for dy in libtorch libtorch_cpu libc10 libtorch_global_deps; do
    echo "==> $dy.xcframework"
    xcodebuild -create-xcframework \
        -library "$DEV/$dy.dylib" \
        -library "$SIM/$dy.dylib" \
        -output "$OUT/$dy.xcframework"
done
echo "==> done:"; ls "$OUT"
