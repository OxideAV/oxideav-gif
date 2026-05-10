//! Crate-local error type used by `oxideav-gif`'s standalone (no
//! `oxideav-core`) public API.
//!
//! When the `registry` feature is enabled, [`GifError`] gains a
//! `From<GifError> for oxideav_core::Error` impl (defined in
//! [`crate::registry`]) so the trait-side surface (`Decoder` /
//! `Encoder`) can keep returning `oxideav_core::Result<T>` while the
//! underlying decode/encode functions stay framework-free.

use core::fmt;

/// `Result` alias scoped to `oxideav-gif`.
pub type Result<T> = core::result::Result<T, GifError>;

/// Error variants returned by `oxideav-gif`'s standalone API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GifError {
    /// The byte stream is malformed (bad magic, truncated header,
    /// LZW data overruns the table, pixel index out of range, …).
    InvalidData(String),
    /// The byte stream uses a feature this codec doesn't implement
    /// (e.g. an encoder asked for an unsupported palette size).
    Unsupported(String),
}

impl GifError {
    /// Construct a [`GifError::InvalidData`] from a stringy message.
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidData(msg.into())
    }

    /// Construct a [`GifError::Unsupported`] from a stringy message.
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }
}

impl fmt::Display for GifError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidData(s) => write!(f, "invalid data: {s}"),
            Self::Unsupported(s) => write!(f, "unsupported: {s}"),
        }
    }
}

impl std::error::Error for GifError {}
