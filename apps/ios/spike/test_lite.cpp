// Proof that the C++ lite-interpreter runtime (the exact code path an iOS app
// links, just compiled for macOS here) loads a bytecode-v10 .ptl and runs it.
// Uses a deterministic input so the result can be matched against Python.
#include <torch/csrc/jit/mobile/import.h>
#include <torch/csrc/jit/mobile/module.h>
#include <ATen/ATen.h>
#include <iostream>

int main(int argc, char** argv) {
    const char* path = argv[1];
    auto m = torch::jit::_load_for_mobile(path, c10::nullopt);

    // steps_motion_decoder: timestamps int64[128], data float32[128,27]
    auto ts = at::arange(128, at::kLong);
    auto data = (at::arange(128 * 27, at::kFloat).remainder(100) / 100.0)
                    .reshape({128, 27});

    std::vector<c10::IValue> inputs{ts, data};
    auto out = m.forward(inputs);

    auto tup = out.toTuple();
    std::cout << "loaded + ran .ptl via C++ lite interpreter\n";
    std::cout << "outputs=" << tup->elements().size() << "\n";
    for (size_t i = 0; i < tup->elements().size(); ++i) {
        auto t = tup->elements()[i].toTensor();
        std::cout << "  out[" << i << "] shape=" << t.sizes()
                  << " sum=" << t.to(at::kDouble).sum().item<double>() << "\n";
    }
    return 0;
}
