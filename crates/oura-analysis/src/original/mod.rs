//! **Original algorithms — open_oura's own logic, NOT from Oura.**
//!
//! Everything here is heuristic logic *we* designed; it does not reproduce any
//! Oura algorithm and will not match Oura's app output. Modules here must say so in
//! their doc-comment. Use this namespace for derived features that Oura computes in
//! its cloud (and therefore can't be ported) or that Oura doesn't offer at all.
//!
//! Contrast with [`crate::ported`], which faithfully reproduces Oura's own
//! computations from the decompiled engine.
