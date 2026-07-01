#!/bin/bash
# Package the device + simulator libtorch builds into xcframeworks, wrapping each dylib
# in a proper .framework bundle — the App Store rejects bare embedded .dylib files
# (ITMS-90426 "SwiftSupport folder is missing" / invalid bundle), it wants dynamic libs
# inside frameworks. Xcode then picks the right slice per SDK and embeds+signs them.
#
# Prereq: build BOTH slices first —
#   apps/ios/spike/build_libtorch_ios.sh          # simulator → build_ios/install
#   apps/ios/spike/build_libtorch_ios.sh device   # device    → build_ios_device/install
#
# Output: apps/ios/libtorch-xcframeworks/<name>.xcframework (gitignored, local artifact).
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../.." && pwd)"   # repo root from this script's location
LT="$REPO/local/libtorch-ios/pytorch"
SIM="$LT/build_ios/install/lib"
DEV="$LT/build_ios_device/install/lib"
OUT="$REPO/apps/ios/libtorch-xcframeworks"
WORK="$REPO/apps/ios/.libtorch-frameworks-build"
MIN=17.0
LIBS="libtorch libtorch_cpu libc10 libtorch_global_deps"

for d in "$SIM" "$DEV"; do
    [ -d "$d" ] || { echo "missing $d — build that slice first"; exit 1; }
done

# Wrap one dylib in a flat iOS .framework: binary named after the framework, install
# name @rpath/<name>.framework/<name>, inter-lib deps rewritten to the framework paths,
# minos normalized, and a minimal Info.plist. $4 = iPhoneOS | iPhoneSimulator, $5 = vtool
# platform (2 device / 7 simulator).
make_framework() {
    local src="$1" name="$2" outdir="$3" plat="$4" vtoolplat="$5"
    local fw="$outdir/$name.framework"
    rm -rf "$fw"; mkdir -p "$fw"
    cp "$src" "$fw/$name"
    vtool -set-build-version "$vtoolplat" "$MIN" "$MIN" -replace -output "$fw/$name" "$fw/$name" >/dev/null
    install_name_tool -id "@rpath/$name.framework/$name" "$fw/$name"
    for dep in $LIBS; do
        install_name_tool -change "@rpath/$dep.dylib" "@rpath/$dep.framework/$dep" "$fw/$name" 2>/dev/null || true
    done
    local bid="org.pytorch.${name//_/-}"
    cat > "$fw/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key><string>en</string>
	<key>CFBundleExecutable</key><string>$name</string>
	<key>CFBundleIdentifier</key><string>$bid</string>
	<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
	<key>CFBundleName</key><string>$name</string>
	<key>CFBundlePackageType</key><string>FMWK</string>
	<key>CFBundleShortVersionString</key><string>1.0</string>
	<key>CFBundleVersion</key><string>1</string>
	<key>MinimumOSVersion</key><string>$MIN</string>
	<key>CFBundleSupportedPlatforms</key><array><string>$plat</string></array>
</dict>
</plist>
PLIST
}

rm -rf "$WORK" "$OUT"; mkdir -p "$WORK/device" "$WORK/sim" "$OUT"
for name in $LIBS; do
    echo "==> $name.framework (device + sim) → xcframework"
    make_framework "$DEV/$name.dylib" "$name" "$WORK/device" "iPhoneOS" 2
    make_framework "$SIM/$name.dylib" "$name" "$WORK/sim"    "iPhoneSimulator" 7
    xcodebuild -create-xcframework \
        -framework "$WORK/device/$name.framework" \
        -framework "$WORK/sim/$name.framework" \
        -output "$OUT/$name.xcframework" >/dev/null
done
rm -rf "$WORK"
echo "==> done:"; ls "$OUT"
