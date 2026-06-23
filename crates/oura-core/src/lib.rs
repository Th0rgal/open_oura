//! `oura-core` — a reusable, version-agnostic client for the Oura ring BLE protocol.
//!
//! The crate is layered so the protocol logic is testable without a real ring:
//!
//! - [`protocol`] — packet framing, service/characteristic UUIDs, request builders.
//! - [`auth`] — the per-connection app-auth challenge (nonce + AES).
//! - [`transport`] — the [`transport::Transport`] trait abstracting the BLE link,
//!   plus the [`transport::transact`] request/response helper. A concrete
//!   `btleplug` implementation lives in [`ble`] (feature `ble`).
//! - [`device`] — parsers for firmware, battery, product/serial, capabilities.
//! - [`events`] — the ring history-event envelope and tag taxonomy.
//! - [`client`] — [`OuraClient`], the high-level API tying it together.
//! - [`storage`] — optional SQLite persistence (feature `storage`).
//!
//! ## Version-agnostic design
//!
//! Ring 3/4/5 share the same GATT layout, framing, and auth flow. Differences are
//! handled by *capability* rather than hard-coded model checks: connect, read
//! [`device::Capabilities`], and branch on what the ring reports. Unknown event
//! payloads are always stored raw (lossless) so new decoders can be added later
//! without re-syncing.

pub mod auth;
pub mod client;
pub mod device;
pub mod error;
pub mod events;
pub mod protocol;
pub mod transport;

#[cfg(feature = "ble")]
pub mod ble;

#[cfg(feature = "storage")]
pub mod storage;

pub use client::OuraClient;
pub use error::{Error, Result};
