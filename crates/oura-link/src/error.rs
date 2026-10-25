//! Error and result types for the BLE link layer.
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
    #[error("ble error: {0}")]
    Ble(String),
    #[error("no matching Oura ring found")]
    DeviceNotFound,
    #[error("timed out connecting to the ring (is it still linked to a phone? take it off/on the charger and retry)")]
    ConnectTimeout,
    #[error("characteristic not found: {0}")]
    CharacteristicNotFound(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(feature = "ble")]
impl From<btleplug::Error> for Error {
    fn from(e: btleplug::Error) -> Self {
        Error::Ble(e.to_string())
    }
}

impl Error {
    /// Whether this error represents a connection or service discovery condition
    /// that is potentially retryable.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Error::ConnectTimeout | Error::CharacteristicNotFound(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_errors() {
        assert!(Error::ConnectTimeout.is_retryable());
        assert!(Error::CharacteristicNotFound("dummy".into()).is_retryable());
        assert!(!Error::DeviceNotFound.is_retryable());
        assert!(!Error::Auth("bad key".into()).is_retryable());
    }
}

