//! Error and result types for the crate.

use thiserror::Error;

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can arise talking to an Oura ring.
#[derive(Error, Debug)]
pub enum Error {
    /// The Bluetooth transport (adapter, connection, GATT) failed.
    #[error("ble error: {0}")]
    Ble(String),

    /// No matching ring was found during scanning.
    #[error("no matching Oura ring found")]
    DeviceNotFound,

    /// A required GATT characteristic was missing on the device.
    #[error("characteristic not found: {0}")]
    CharacteristicNotFound(String),

    /// App-level authentication failed or was rejected by the ring.
    #[error("authentication failed: {0}")]
    Auth(String),

    /// A response was malformed or unexpected for the request.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// Persistence (SQLite) failed.
    #[error("storage error: {0}")]
    Storage(String),

    /// An I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(feature = "ble")]
impl From<btleplug::Error> for Error {
    fn from(e: btleplug::Error) -> Self {
        Error::Ble(e.to_string())
    }
}

#[cfg(feature = "storage")]
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Storage(e.to_string())
    }
}
