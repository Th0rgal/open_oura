//! The classifier that ships with the crate.
//!
//! The blob `data/sleep_stage_gbt.bin` is a gradient-boosted tree ensemble (4 classes, 800 rounds
//! of trees with up to 32 leaves, 99,200 nodes, 1,846,470 bytes) in the format of
//! `docs/algorithms/staging-classifier.md`. It was trained by the maintainer on simulated nights
//! only, so no recorded night is in its training data. On 14 nights from one user it agrees with
//! the official app's per-30-second stages on 92.6% of epochs (pooled; 95% bootstrap interval over nights
//! 91.6% to 93.6%; Cohen's kappa 0.89). The previous bundled model, trained on 8 of those nights,
//! agrees on 92.4% of the 14 and 90.5% of the 6 it was not trained on. The figures describe
//! agreement with the official app's stages for one user, not accuracy against polysomnography, and other
//! users are not covered by the evaluation.
//!
//! Another model can replace it through `classifier::load_classifier` and
//! `pipeline::stage_night_with`.

use super::classifier::inspect_blob;
use crate::ported::sleep_classifier::SleepClassifier;
use std::sync::OnceLock;

static BLOB: &[u8] = include_bytes!("../../../data/sleep_stage_gbt.bin");
static PARSED: OnceLock<SleepClassifier> = OnceLock::new();

/// The bytes of the bundled classifier blob.
pub fn bundled_classifier_bytes() -> &'static [u8] {
    BLOB
}

/// The bundled classifier, parsed on first use and shared afterwards.
///
/// The blob is checked by the unit tests, so a failure to parse is a build defect and panics.
pub fn bundled_classifier() -> &'static SleepClassifier {
    PARSED.get_or_init(|| {
        inspect_blob(BLOB).expect("bundled classifier header");
        SleepClassifier::from_bytes(BLOB).expect("bundled classifier blob")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ported::sleepstaging::classifier::{StageClassifier, STAGER_FEATURES};

    /// SHA-256 of `data/sleep_stage_gbt.bin`. Update it together with the blob.
    const BLOB_SHA256: &str = "8fca22f7e9c873d594f6eb4a9d5b21768f4dcf14be0082c8e26c295a8d923f19";

    fn sha256_hex(data: &[u8]) -> String {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
            0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
            0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
            0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
            0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
            0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
            0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
        ];
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
        ];
        let mut msg = data.to_vec();
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
        for block in msg.chunks_exact(64) {
            let mut w = [0u32; 64];
            for (i, c) in block.chunks_exact(4).enumerate() {
                w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
            }
            for i in 16..64 {
                let sig0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let sig1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16].wrapping_add(sig0).wrapping_add(w[i - 7]).wrapping_add(sig1);
            }
            let mut v = h;
            for i in 0..64 {
                let big1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
                let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
                let t1 = v[7].wrapping_add(big1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
                let big0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
                let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
                let t2 = big0.wrapping_add(maj);
                v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
            }
            for i in 0..8 {
                h[i] = h[i].wrapping_add(v[i]);
            }
        }
        h.iter().map(|x| format!("{x:08x}")).collect()
    }

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn blob_is_unchanged() {
        assert_eq!(bundled_classifier_bytes().len(), 1_846_470);
        assert_eq!(sha256_hex(bundled_classifier_bytes()), BLOB_SHA256);
    }

    #[test]
    fn blob_loads_and_has_the_documented_shape() {
        let h = inspect_blob(bundled_classifier_bytes()).unwrap();
        assert_eq!((h.version, h.n_features, h.n_classes, h.n_rounds), (1, 182, 4, 800));
        let clf = bundled_classifier();
        assert_eq!((clf.n_rounds(), clf.n_nodes()), (800, 99_200));
        assert!(std::ptr::eq(clf, bundled_classifier()));
        let dynamic: &dyn StageClassifier = clf;
        assert_eq!((dynamic.n_features(), dynamic.n_classes()), (STAGER_FEATURES, 4));
    }

    fn rows() -> Vec<[f32; STAGER_FEATURES]> {
        let zeros = [0f32; STAGER_FEATURES];
        let nan = [f32::NAN; STAGER_FEATURES];
        let mut ramp = [0f32; STAGER_FEATURES];
        let mut alt = [0f32; STAGER_FEATURES];
        for k in 0..STAGER_FEATURES {
            ramp[k] = (k as f32 - 90.0) * 0.37;
            alt[k] = if k % 3 == 0 { f32::NAN } else { ((k * 7919) % 101) as f32 / 10.0 - 5.0 };
        }
        vec![zeros, nan, ramp, alt]
    }

    #[test]
    fn margins_of_fixed_rows_are_stable() {
        let expected: [[f64; 4]; 4] = [
            [-1.5501620865035761, 1.4096348618392085, -4.934963018257122, -1.0969842428730772],
            [-3.2329283619343983, -0.11144537625489193, -2.562564092184573, 2.7781006852933956],
            [-4.851369719716786, -0.40904513644909457, -1.2741748006792477, 2.980949903289911],
            [-3.5847939437667264, 0.3284785843785869, -4.121143422224577, 2.359841650598554],
        ];
        for (x, e) in rows().iter().zip(expected) {
            let m = bundled_classifier().margins(x);
            for c in 0..4 {
                assert!((m[c] - e[c]).abs() < 1e-9, "class {c}: {} vs {}", m[c], e[c]);
            }
            let p = bundled_classifier().predict_proba(x);
            assert!((p.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        }
    }
}
