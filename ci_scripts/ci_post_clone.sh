#!/bin/sh
# Xcode Cloud post-clone: a clean cloud checkout has no OuraCore.xcframework and no
# OuraApp.xcodeproj (both gitignored), so build the Rust UniFFI xcframework and
# generate the MODEL-FREE Xcode project the workflow archives. The torch models
# (libtorch + .ptl) are NOT part of CI — they live only in the local project.yml.
set -e
echo "=== ci_post_clone: Rust xcframework + xcodegen (model-free) ==="

# xcodegen, to generate the project from project-ci.yml
brew install xcodegen

# Rust toolchain + the iOS targets build-xcframework.sh links
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
. "$HOME/.cargo/env"
rustup target add aarch64-apple-ios aarch64-apple-ios-sim

REPO="${CI_PRIMARY_REPOSITORY_PATH:-$PWD}"

# OuraCore.xcframework (device + sim) from the committed UniFFI bindings
bash "$REPO/apps/ios/build-xcframework.sh"

# the model-free Xcode project the Xcode Cloud workflow builds + archives
cd "$REPO/apps/ios/OuraApp"
xcodegen generate --spec project-ci.yml

echo "=== ci_post_clone done ==="
