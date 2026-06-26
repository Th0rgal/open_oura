//! C ABI over the tested Oura protocol core, for linking into the iOS app.
//!
//! We expose only the byte-level pieces that are genuinely hard to reproduce —
//! the AES auth and the event-body decoders — as pure, synchronous functions.
//! BLE transport, packet framing (trivial `tag|len|payload`), the request
//! builders (tiny byte arrays), and the connect/sync orchestration are all done
//! natively in Swift, so nothing async crosses the FFI boundary.

use std::ffi::{c_char, CString};
use std::slice;

use oura_protocol::auth::encrypt_nonce;
use oura_protocol::events::{decode_event_body, event_name};

/// Encrypt a ring auth nonce (AES-128/ECB/PKCS7) into `out` (must hold 16 bytes).
/// `key` must be exactly 16 bytes; `nonce` is typically 15. Returns 0 on success,
/// negative on bad arguments.
#[no_mangle]
pub extern "C" fn oura_encrypt_nonce(
    key: *const u8,
    key_len: usize,
    nonce: *const u8,
    nonce_len: usize,
    out: *mut u8,
) -> i32 {
    if key.is_null() || nonce.is_null() || out.is_null() || key_len != 16 {
        return -1;
    }
    // SAFETY: caller guarantees the pointers are valid for the given lengths.
    let key_slice = unsafe { slice::from_raw_parts(key, 16) };
    let nonce_slice = unsafe { slice::from_raw_parts(nonce, nonce_len) };
    let mut k = [0u8; 16];
    k.copy_from_slice(key_slice);
    let res = encrypt_nonce(&k, nonce_slice);
    unsafe { std::ptr::copy_nonoverlapping(res.as_ptr(), out, 16) };
    0
}

/// Decode an event body for `tag` into a JSON C string, or null if the tag has no
/// decoder / the body is malformed. The returned string is owned by the caller and
/// must be released with [`oura_string_free`].
#[no_mangle]
pub extern "C" fn oura_decode_event(tag: u8, body: *const u8, body_len: usize) -> *mut c_char {
    let body: &[u8] = if body.is_null() || body_len == 0 {
        &[]
    } else {
        // SAFETY: caller guarantees `body` is valid for `body_len` bytes.
        unsafe { slice::from_raw_parts(body, body_len) }
    };
    match decode_event_body(tag, body) {
        Some(v) => CString::new(v.to_string())
            .map(|s| s.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        None => std::ptr::null_mut(),
    }
}

/// Human-readable event name for `tag` (owned C string; release with
/// [`oura_string_free`]).
#[no_mangle]
pub extern "C" fn oura_event_name(tag: u8) -> *mut c_char {
    CString::new(event_name(tag))
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Release a C string previously returned by this library.
#[no_mangle]
pub extern "C" fn oura_string_free(ptr: *mut c_char) {
    if !ptr.is_null() {
        // SAFETY: `ptr` came from `CString::into_raw` in this library.
        unsafe { drop(CString::from_raw(ptr)) };
    }
}
