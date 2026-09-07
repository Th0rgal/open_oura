//! Error type for the storage layer.
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
    #[error("storage error: {0}")]
    Storage(String),
    #[error("storage error: sqlite={code} extended={extended_code}: {message}")]
    Sqlite {
        code: i32,
        extended_code: i32,
        message: String,
    },
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        match e {
            rusqlite::Error::SqliteFailure(code, message) => Error::Sqlite {
                code: code.extended_code & 0xff,
                extended_code: code.extended_code,
                message: message.unwrap_or_else(|| code.to_string()),
            },
            other => Error::Storage(other.to_string()),
        }
    }
}
