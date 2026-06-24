//! **Ported algorithms — reverse-engineered from Oura's own software.**
//!
//! Every module here reproduces a computation that Oura performs, ported from the
//! decompiled on-device `ecore` engine (`libappecore.so`). Each function's
//! doc-comment cites its source as `<ecore function> @ <address>`. These are *not*
//! our inventions: the intent is bit-for-bit (or as close as the recovered
//! constants allow) fidelity to Oura's results. Anything we could not fully recover
//! is flagged in the module docs and in `docs/algorithms/`.
//!
//! Contrast with [`crate::original`], which holds open_oura's *own* heuristics that
//! Oura does not ship.

pub mod baseline;
pub mod hrv;
pub mod metabolic;
pub mod sleep;
pub mod sleep_debt;
pub mod spo2;
pub mod temperature;
