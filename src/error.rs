//! Error type returned by decode/encode entry points.
//!
//! Every parsing path in the decoder and every validation path in the
//! encoder maps to one of these variants — there is no `panic!` on
//! malformed input (the fuzz harness `decode_panic_free` enforces this).

use core::fmt;

/// Result alias used throughout the crate.
pub type Result<T> = core::result::Result<T, Error>;

/// All recoverable failures produced by encoding or decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The byte stream is structurally invalid for GIF87a / GIF89a as
    /// defined by the CompuServe specifications.
    InvalidData(String),

    /// The byte stream is structurally well-formed but uses a feature
    /// outside the implemented subset.
    Unsupported(String),

    /// The byte stream ended before a required field could be read.
    UnexpectedEof,

    /// An encoder input violates a constraint declared by the spec
    /// (palette > 256 entries, dimensions > 65535, pixel index outside
    /// the colour table, etc.).
    InvalidInput(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidData(s) => write!(f, "invalid GIF data: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported GIF feature: {s}"),
            Error::UnexpectedEof => write!(f, "unexpected end of stream"),
            Error::InvalidInput(s) => write!(f, "invalid encoder input: {s}"),
        }
    }
}

impl std::error::Error for Error {}
