//! `oura-link` — the *fetch* layer: how bytes get off the ring.
//!
//! BLE transport (btleplug) behind a [`transport::Transport`] trait, the
//! connection + app-auth handshake, and the high-level [`OuraClient`] (device
//! info, battery, the history-event sync drain, live HR/ACM, features, RData).
#[cfg(feature = "ble")]
pub mod ble;
pub mod client;
pub mod error;
pub mod transport;

pub use client::OuraClient;
pub use error::{Error, Result};
