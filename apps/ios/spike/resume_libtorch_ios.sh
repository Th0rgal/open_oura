#!/bin/bash
# Resume the iOS-simulator libtorch lite build: the pytorch source is already
# cloned; finish the needed submodules (GPU-only flash-attention stays deinited)
# and run build_ios.sh. Meant to run as a tracked background job.
set -uo pipefail
REPO="$HOME/work/open_oura"
SRC="$REPO/local/libtorch-ios/pytorch"
cd "$SRC"

export PATH="$REPO/.venv/bin:$PATH"
export CMAKE_POLICY_VERSION_MINIMUM=3.5
export BUILD_LITE_INTERPRETER=1
export USE_DISTRIBUTED=0 USE_MKLDNN=0 USE_NNPACK=0 USE_QNNPACK=0
export USE_PYTORCH_QNNPACK=0 USE_COREML_DELEGATE=0 USE_FLASH_ATTENTION=0 USE_MEM_EFF_ATTENTION=0

echo "==> finishing submodules (flash-attention left deinited)"
git submodule update --init --recursive --depth 1 2>&1 | tail -5

echo "==> build_ios.sh SIMULATOR/arm64/lite  (long)"
IOS_PLATFORM=SIMULATOR IOS_ARCH=arm64 ./scripts/build_ios.sh
echo "==> BUILD_EXIT=$?"
find "$SRC/build_ios" -name "libtorch_cpu.a" 2>/dev/null
