//! `oura-analysis` — the *interpretation (high level)* layer: turning decoded
//! samples into daily metrics and derived insights.
//!
//! Everything here is [`ported`] — algorithms **reverse-engineered from Oura's
//! own software** (the on-device `ecore` engine). These aim to reproduce Oura's
//! results and cite the source function `@ address`. See `docs/algorithms/`.
//!
//! (Activity-session detection used to live in an `original` namespace of
//! open_oura's own heuristics; app-level activity classification now lives in
//! `open_health`.)

pub mod ported;
