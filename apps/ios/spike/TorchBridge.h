#ifndef OURA_TORCH_BRIDGE_H
#define OURA_TORCH_BRIDGE_H

#ifdef __cplusplus
extern "C" {
#endif

// Loads the .ptl at `ptl_path`, runs steps_motion_decoder on a fixed input, and
// returns the sum of output[1] (~303.09 — matches macOS/Python). NAN on failure.
double oura_torch_smoke(const char* ptl_path);

#ifdef __cplusplus
}
#endif

#endif
