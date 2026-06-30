//! Minimal C-ABI surface over the shared Rust analysis core.
//!
//! This is the spike proof that the *same* `oura-analysis` code the web
//! dashboard computes with links and runs inside an iOS app. The real FFI layer
//! would be generated with UniFFI; here we hand-roll one pure function (HRV RMSSD)
//! to keep the simulator test self-contained.

use std::os::raw::c_double;

/// Compute RMSSD (ms) over a buffer of inter-beat intervals (ms).
///
/// `ibi_ptr` points to `len` `u16` samples. Returns the RMSSD, or `-1.0` when the
/// input is too short / null (mirrors `oura_analysis`'s `Option::None`).
///
/// # Safety
/// `ibi_ptr` must be valid for `len` contiguous `u16` reads, or null.
#[no_mangle]
pub unsafe extern "C" fn oura_rmssd(ibi_ptr: *const u16, len: usize) -> c_double {
    if ibi_ptr.is_null() || len == 0 {
        return -1.0;
    }
    let ibis = std::slice::from_raw_parts(ibi_ptr, len);
    oura_analysis::ported::hrv::rmssd(ibis).unwrap_or(-1.0)
}
