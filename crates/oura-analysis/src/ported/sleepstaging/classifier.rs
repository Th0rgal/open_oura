//! The classifier boundary of the sleep stager.
//!
//! The stager turns a night of ring data into one row of 182 features per 30 s epoch, scores each
//! row with a four-class classifier (deep, light, REM, wake), and post-processes the scores. The
//! feature pipeline and the post-processing are in this crate. The classifier is not: it is any
//! type that implements [`StageClassifier`]. [`SleepClassifier`] is one implementation, a
//! decision-tree ensemble read from a flat blob file (`docs/algorithms/staging-classifier.md`
//! specifies the format).
//!
//! The feature order is the one of [`KEEP_COLUMNS`] without its timestamp entry, so feature `k`
//! is column `k + 1` of that list. A classifier must treat NaN as a missing value.

use super::feature_columns::{COMBINED_COLUMNS, KEEP_COLUMNS};
use crate::ported::sleep_classifier::{ClassifierError, SleepClassifier, N_CLASSES, N_FEATURES};
use std::path::Path;

// The classifier input is the kept columns without the timestamp, and every kept column exists.
const _: () = assert!(KEEP_COLUMNS.len() == N_FEATURES + 1);
const _: () = assert!(KEEP_COLUMNS.len() <= COMBINED_COLUMNS.len());

/// Number of input features the stager passes to a classifier.
pub const STAGER_FEATURES: usize = N_FEATURES;
/// Number of output classes, in the order deep, light, REM, wake.
pub const STAGER_CLASSES: usize = N_CLASSES;

/// A four-class sleep-stage classifier over the stager's feature rows.
pub trait StageClassifier {
    /// Number of input features the classifier expects. The stager checks it against
    /// [`STAGER_FEATURES`] before it runs.
    fn n_features(&self) -> usize {
        STAGER_FEATURES
    }

    /// Number of classes the classifier returns. The stager checks it against [`STAGER_CLASSES`].
    fn n_classes(&self) -> usize {
        STAGER_CLASSES
    }

    /// Class probabilities of one feature row (deep, light, REM, wake). NaN entries are
    /// missing values.
    fn predict_proba(&self, features: &[f32; STAGER_FEATURES]) -> [f32; STAGER_CLASSES];

    /// Unnormalized class scores of one feature row, for implementations that have them.
    /// The stager does not call this.
    fn margins(&self, _features: &[f32; STAGER_FEATURES]) -> Option<[f64; STAGER_CLASSES]> {
        None
    }

    /// Probabilities of every row of a row-major matrix with [`STAGER_FEATURES`] columns.
    /// Implementations may override this to batch the work.
    fn predict_proba_rows(&self, rows: &[f32]) -> Vec<[f32; STAGER_CLASSES]> {
        rows.chunks_exact(STAGER_FEATURES)
            .map(|r| {
                let mut x = [0f32; STAGER_FEATURES];
                x.copy_from_slice(r);
                self.predict_proba(&x)
            })
            .collect()
    }
}

impl StageClassifier for SleepClassifier {
    fn predict_proba(&self, features: &[f32; STAGER_FEATURES]) -> [f32; STAGER_CLASSES] {
        SleepClassifier::predict_proba(self, features)
    }

    fn margins(&self, features: &[f32; STAGER_FEATURES]) -> Option<[f64; STAGER_CLASSES]> {
        Some(SleepClassifier::margins(self, features))
    }
}

impl<T: StageClassifier + ?Sized> StageClassifier for &T {
    fn n_features(&self) -> usize {
        (**self).n_features()
    }
    fn n_classes(&self) -> usize {
        (**self).n_classes()
    }
    fn predict_proba(&self, features: &[f32; STAGER_FEATURES]) -> [f32; STAGER_CLASSES] {
        (**self).predict_proba(features)
    }
    fn margins(&self, features: &[f32; STAGER_FEATURES]) -> Option<[f64; STAGER_CLASSES]> {
        (**self).margins(features)
    }
}

/// The fields of a blob header that identify it, read without parsing the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobHeader {
    pub version: u32,
    pub n_features: usize,
    pub n_classes: usize,
    pub n_rounds: usize,
}

/// Reads the header of a classifier blob and checks that its feature and class counts equal the
/// stager's. It does not validate the sections that follow the header.
pub fn inspect_blob(data: &[u8]) -> Result<BlobHeader, ClassifierError> {
    if data.len() < 20 {
        return Err(ClassifierError::Size);
    }
    if &data[..4] != b"OSCF" {
        return Err(ClassifierError::BadMagic);
    }
    let u = |at: usize| u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
    let h = BlobHeader { version: u(4), n_features: u(8) as usize, n_classes: u(12) as usize, n_rounds: u(16) as usize };
    if h.version != 1 {
        return Err(ClassifierError::UnsupportedVersion(h.version));
    }
    if h.n_features != STAGER_FEATURES {
        return Err(ClassifierError::Invalid("feature count differs from the stager's"));
    }
    if h.n_classes != STAGER_CLASSES {
        return Err(ClassifierError::Invalid("class count differs from the stager's"));
    }
    Ok(h)
}

/// Parses and validates a blob. Any malformed input returns an error; nothing panics.
pub fn load_classifier_bytes(data: &[u8]) -> Result<Box<dyn StageClassifier + Send + Sync>, ClassifierError> {
    inspect_blob(data)?;
    Ok(Box::new(SleepClassifier::from_bytes(data)?))
}

/// Reads and validates a blob file.
pub fn load_classifier(path: impl AsRef<Path>) -> Result<Box<dyn StageClassifier + Send + Sync>, ClassifierError> {
    let data = std::fs::read(path).map_err(|e| ClassifierError::Io(e.to_string()))?;
    load_classifier_bytes(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small valid blob: one round, constant leaves, program `[0]`.
    pub(crate) fn tiny_blob() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"OSCF");
        for v in [1u32, N_FEATURES as u32, N_CLASSES as u32, 1, 0, N_CLASSES as u32, 1, 0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&0f64.to_le_bytes());
        b.resize(64, 0);
        b.extend_from_slice(&0u16.to_le_bytes()); // prog
        for _ in 0..N_CLASSES {
            b.extend_from_slice(&0x8000u16.to_le_bytes()); // roots: leaf 0
        }
        for _ in 0..=N_CLASSES {
            b.extend_from_slice(&0u32.to_le_bytes()); // node_off
        }
        for k in 0..=N_CLASSES as u32 {
            b.extend_from_slice(&k.to_le_bytes()); // leaf_off
        }
        for v in [0.0f64, 1.0, 2.0, 3.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b
    }

    #[test]
    fn loads_a_valid_blob_through_the_trait() {
        let clf = load_classifier_bytes(&tiny_blob()).unwrap();
        assert_eq!((clf.n_features(), clf.n_classes()), (STAGER_FEATURES, STAGER_CLASSES));
        let x = [0f32; STAGER_FEATURES];
        assert_eq!(clf.margins(&x), Some([0.0, 1.0, 2.0, 3.0]));
        let p = clf.predict_proba(&x);
        assert!(p[3] > p[2] && p[2] > p[1] && p[1] > p[0]);
        let rows = vec![0f32; 3 * STAGER_FEATURES];
        assert_eq!(clf.predict_proba_rows(&rows), vec![p; 3]);
    }

    #[test]
    fn header_mismatches_are_errors() {
        let blob = tiny_blob();
        assert_eq!(inspect_blob(&blob).unwrap().n_rounds, 1);
        assert_eq!(inspect_blob(&blob[..8]).unwrap_err(), ClassifierError::Size);
        let mut b = blob.clone();
        b[8] = 100;
        assert!(matches!(inspect_blob(&b), Err(ClassifierError::Invalid(m)) if m.contains("feature")));
        assert!(load_classifier_bytes(&b).is_err());
        let mut b = blob.clone();
        b[12] = 3;
        assert!(matches!(inspect_blob(&b), Err(ClassifierError::Invalid(m)) if m.contains("class")));
        let mut b = blob;
        b[0] = b'Z';
        assert_eq!(load_classifier_bytes(&b).err(), Some(ClassifierError::BadMagic));
        assert!(matches!(load_classifier("/nonexistent/blob.bin"), Err(ClassifierError::Io(_))));
    }

    #[test]
    fn corrupted_blobs_never_panic() {
        let blob = tiny_blob();
        for n in 0..blob.len() {
            assert!(load_classifier_bytes(&blob[..n]).is_err(), "truncated to {n}");
        }
        // every single-byte change either loads or returns an error
        for i in 0..blob.len() {
            for v in [0u8, 1, 0x7f, 0x80, 0xff] {
                let mut b = blob.clone();
                b[i] = v;
                let _ = load_classifier_bytes(&b);
            }
        }
        // header counts that would overflow or allocate hugely
        for field in [16usize, 20, 24, 28, 32] {
            let mut b = blob.clone();
            b[field..field + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(load_classifier_bytes(&b).is_err(), "field at {field}");
        }
    }
}
