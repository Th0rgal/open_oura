//! `oura-analysis` — the *interpretation (high level)* layer: turning decoded
//! samples into daily metrics and derived insights.
//!
//! Code here is split by **provenance**, so it is always clear whether a result is
//! Oura's or ours:
//!
//! - [`ported`] — algorithms **reverse-engineered from Oura's own software** (the
//!   on-device `ecore` engine). These aim to reproduce Oura's results and cite the
//!   source function `@ address`. See `docs/algorithms/`.
//! - [`original`] — open_oura's **own heuristics**, which Oura does not ship and
//!   which will not match the Oura app. Used for things Oura computes in its cloud
//!   (so they can't be ported) or doesn't offer.
//!
//! Call sites keep the distinction visible: `ported::hrv::rmssd(..)` vs
//! `original::activity_session::detect(..)`.

pub mod original;
pub mod ported;
