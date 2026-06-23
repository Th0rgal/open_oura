//! App-level authentication.
//!
//! The ring challenges with a 15-byte nonce; the client encrypts it with
//! `AES-128/ECB/PKCS7` under the shared 16-byte key and returns the 16-byte
//! ciphertext. This module holds the pure crypto so it is unit-testable without
//! a ring.

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;

/// Result of an `Authenticate` exchange, as reported by the ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthResult {
    Success,
    AuthenticationError,
    InFactoryReset,
    NotOriginalOnboardedDevice,
    Unknown(u8),
}

impl From<u8> for AuthResult {
    fn from(b: u8) -> Self {
        match b {
            0x00 => AuthResult::Success,
            0x01 => AuthResult::AuthenticationError,
            0x02 => AuthResult::InFactoryReset,
            0x03 => AuthResult::NotOriginalOnboardedDevice,
            other => AuthResult::Unknown(other),
        }
    }
}

impl AuthResult {
    pub fn is_success(self) -> bool {
        matches!(self, AuthResult::Success)
    }
}

/// Encrypt a ring nonce (typically 15 bytes) into the 16-byte response block.
///
/// Mirrors the Android app's `AES/ECB/PKCS5Padding`: the nonce is PKCS7-padded to
/// one 16-byte block, then a single AES-128 block-encrypt is applied.
pub fn encrypt_nonce(key: &[u8; 16], nonce: &[u8]) -> [u8; 16] {
    let mut block = [0u8; 16];
    let n = nonce.len().min(16);
    block[..n].copy_from_slice(&nonce[..n]);
    // PKCS7: pad the remaining bytes with the pad length.
    let pad = (16 - n) as u8;
    for b in block.iter_mut().skip(n) {
        *b = pad;
    }

    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut ga = GenericArray::clone_from_slice(&block);
    cipher.encrypt_block(&mut ga);

    let mut out = [0u8; 16];
    out.copy_from_slice(&ga);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_answer_vector() {
        // Verified against the Python reference (AES/ECB/PKCS5) used in tools/.
        let key = hex::decode("4431967d8bacc2659743142b68391d9a").unwrap();
        let key: [u8; 16] = key.try_into().unwrap();
        let nonce = hex::decode("0e2d6a0a08c99b4365f458e6e97382").unwrap();
        assert_eq!(nonce.len(), 15);
        let out = encrypt_nonce(&key, &nonce);
        assert_eq!(hex::encode(out), "a38a8772d3acb6db5c2b516dd56987c8");
    }
}
