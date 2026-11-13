//! The error type of every fallible operation.

use std::fmt;

/// An error reading a Probe file. Corrupt or truncated input always gives an error, never a panic.
#[derive(Debug)]
pub enum Error {
    /// Reading the file failed.
    Io(std::io::Error),
    /// The bytes are not a valid Probe file: bad magic, a range outside the file, a corrupt block,
    /// counts that disagree, a side stream used inexactly.
    Format(String),
    /// A valid-looking file that this reader does not implement: an unknown version, another
    /// coding family or layout, or a flag bit or parameter it does not decode.
    Unsupported(String),
    /// The request does not match the file: an unknown market, a segment out of range.
    NotFound(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io: {}", e),
            Error::Format(m) => write!(f, "probe: {}", m),
            Error::Unsupported(m) => write!(f, "probe: unsupported: {}", m),
            Error::NotFound(m) => write!(f, "probe: not found: {}", m),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// The result type of this crate.
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn format_err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Format(msg.into()))
}
