// Objective-C++ bridge: load a .ptl and run it through the LibTorch lite
// interpreter, on iOS. Exposes a plain C function callable from Swift.
// Mirrors apps/ios/spike/test_lite.cpp, which already proved this runtime runs
// our bytecode-v10 models bit-identically on macOS.
#import "TorchBridge.h"
#include <torch/csrc/jit/mobile/import.h>
#include <torch/csrc/jit/mobile/module.h>
#include <ATen/ATen.h>
#include <string>

// Runs steps_motion_decoder on a deterministic input and returns the sum of the
// second output tensor — the same value test_lite.cpp / Python report (~303.09),
// so the caller can assert parity on-device. Returns NAN on failure.
double oura_torch_smoke(const char* ptl_path) {
    try {
        auto m = torch::jit::_load_for_mobile(std::string(ptl_path), c10::nullopt);
        auto ts = at::arange(128, at::kLong);
        auto data = (at::arange(128 * 27, at::kFloat).remainder(100) / 100.0)
                        .reshape({128, 27});
        std::vector<c10::IValue> inputs{ts, data};
        auto out = m.forward(inputs).toTuple();
        return out->elements()[1].toTensor().to(at::kDouble).sum().item<double>();
    } catch (const std::exception& e) {
        return std::nan("");
    }
}
