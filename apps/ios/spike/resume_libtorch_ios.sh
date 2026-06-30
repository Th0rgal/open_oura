#!/bin/bash
# Resume the iOS-simulator libtorch lite build: the pytorch source is already
# cloned; finish the needed submodules (GPU-only flash-attention stays deinited)
# and drive CMake directly. PyTorch 2.9 REMOVED scripts/build_ios.sh, so we use the
# same kept cmake/iOS.cmake flow as build_libtorch_ios.sh. Tracked background job.
set -uo pipefail
REPO="$HOME/work/open_oura"
SRC="$REPO/local/libtorch-ios/pytorch"
cd "$SRC"

export PATH="$REPO/.venv/bin:$PATH"
export CMAKE_POLICY_VERSION_MINIMUM=3.5
PY="$REPO/.venv/bin/python"; NINJA="$(which ninja)"

echo "==> finishing submodules (flash-attention left deinited)"
git submodule update --init --recursive --depth 1 2>&1 | tail -5

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
echo "==> BUILD_EXIT=$?"
find "$SRC/build_ios/install" -name "libtorch_cpu.a" 2>/dev/null
