//! Sleep staging: feature extraction, post-processing and a pluggable classifier for the 30 s
//! stager.
//!
//! The pipeline turns cleaned ring data (motion, inter-beat intervals, temperature) into a
//! table of 30 s epochs with 182 classifier features ([`pipeline::stage_features`]), scores each
//! row with a [`classifier::StageClassifier`], and post-processes the probabilities into a
//! hypnogram ([`pipeline::stage_night`]). [`pipeline::stage_night_default`] uses the classifier in
//! [`bundled`]; any other [`classifier::StageClassifier`] can be passed to [`pipeline::stage_night_with`].
//!
//! All arithmetic is float32 in a fixed operation order. The build uses ordinary
//! `std` arithmetic, a self-contained FFT ([`numeric_ops::rfft`]) and plain summation.
//! `docs/algorithms/staging-classifier.md` describes the classifier interface.

pub mod bundled;
pub mod classifier;
pub mod events;
pub mod feature_columns;
pub mod features;
pub mod hrv_low;
pub mod mat;
pub mod ops;
pub mod pipeline;
pub mod postprocess;
pub mod rolling;
pub mod vendor_forest;
pub mod numeric_ops;


