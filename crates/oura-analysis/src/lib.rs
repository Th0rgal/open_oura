//! `oura-analysis` — the *interpretation (high level)* layer: turning decoded
//! samples into daily metrics and derived insights.
//!
//! Everything here is [`ported`] — algorithms **reverse-engineered from Oura's
//! own software** (the on-device `ecore` engine). These aim to reproduce Oura's
//! results and cite the source function `@ address`. See `docs/algorithms/`.
//!
//! (Activity-session detection used to live in an `original` namespace of
//! open_oura's own heuristics; it was dropped in favor of running Oura's real
//! `automatic_activity_detection` model — see `tools/run_activity_model.py`,
//! which backs `oura sessions`.)

pub mod ported;
