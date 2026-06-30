#!/bin/bash
# Build LibTorch + lite interpreter for the iOS *simulator* (arm64) from torch
# 2.9.0 source — the runtime that runs our bytecode-v10 .ptl models on-device.
#
# NOTE: PyTorch 2.9 REMOVED scripts/build_ios.sh; we drive CMake directly with
# the kept cmake/iOS.cmake toolchain. In a cross-compile context CMake loses the
# host tools, so ninja + python must be passed explicitly. Configure is validated
# working; the compile below is the long part (tens of minutes).
#
# Simulator-only here (matches this Mac's simulator); add an IOS_PLATFORM=OS pass
# + xcframework for shipping to devices.
set -euo pipefail

REPO="$HOME/work/open_oura"
ROOT="$REPO/local/libtorch-ios"
SRC="$ROOT/pytorch"

echo "==> clone pytorch v2.9.0 (shallow; flash-attention/GPU submodules skipped)"
if [ ! -d "$SRC/.git" ]; then
    git clone --depth 1 --branch v2.9.0 https://github.com/pytorch/pytorch.git "$SRC"
fi
cd "$SRC"
git submodule deinit -f third_party/flash-attention 2>/dev/null || true
git submodule update --init --recursive 2>&1 | tail -3

source "$REPO/.venv/bin/activate" 2>/dev/null || true
"$REPO/.venv/bin/pip" install -q pyyaml typing_extensions 2>/dev/null || true
PY="$REPO/.venv/bin/python"; NINJA="$(which ninja)"
export CMAKE_POLICY_VERSION_MINIMUM=3.5   # cmake 4.x compat for old submodules

echo "==> cmake configure (iOS simulator / arm64 / lite / CPU)"
cmake -GNinja -S . -B build_ios \
  -DCMAKE_MAKE_PROGRAM="$NINJA" -DPython_EXECUTABLE="$PY" -DPYTHON_EXECUTABLE="$PY" \
  -DCMAKE_TOOLCHAIN_FILE="$PWD/cmake/iOS.cmake" \
  -DIOS_PLATFORM=SIMULATOR -DIOS_ARCH=arm64 \
  -DCMAKE_BUILD_TYPE=Release \
  -DINTERN_BUILD_MOBILE=ON -DBUILD_LITE_INTERPRETER=ON \
  -DBUILD_PYTHON=OFF -DBUILD_TEST=OFF -DBUILD_BINARY=OFF \
  -DUSE_DISTRIBUTED=OFF -DUSE_MKLDNN=OFF -DUSE_NNPACK=OFF \
  -DUSE_PYTORCH_QNNPACK=OFF -DUSE_XNNPACK=ON \
  -DUSE_CUDA=OFF -DUSE_MPS=OFF -DUSE_NUMPY=OFF -DUSE_OPENMP=OFF \
  -DUSE_BLAS=OFF -DUSE_LAPACK=OFF \
  -DCMAKE_INSTALL_PREFIX="$PWD/build_ios/install"

echo "==> compile + install (long)"
cmake --build build_ios --target install -- -j"$(sysctl -n hw.ncpu)"

echo "==> done:"
find "$SRC/build_ios/install" -name "*.a" | head
