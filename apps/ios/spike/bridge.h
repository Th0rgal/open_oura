#ifndef OURA_BRIDGE_H
#define OURA_BRIDGE_H
#include <stdint.h>
#include <stddef.h>

// Exposed by liboura_ffi.a (crates/oura-ffi). Pure shared analysis core.
double oura_rmssd(const uint16_t *ibi_ptr, size_t len);

// Exposed by TorchBridge.mm (LibTorch lite). Only linked in the torch build;
// declared weak so the plain Rust-only build still links.
double oura_torch_smoke(const char *ptl_path) __attribute__((weak_import));

#endif
