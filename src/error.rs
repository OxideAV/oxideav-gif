//! Error type returned by every decode / encode entry point.
//!
//! Every parsing path in the decoder and every validation path in the
//! encoder maps to one of these variants — there is no `panic!` on
//! malformed input (the fuzz harness `decode_panic_free` enforces this).
//!
//! The type follows the OxideAV image-crate contract (`IMAGE_CRATE_API`):
//! the enum is [`GifError`], [`Error`] is its alias, and the four
//! contract variants (`InvalidData`, `Unsupported`, `LimitExceeded`,
//! `Io`) are present alongside the GIF-specific `UnexpectedEof` and
//! `InvalidInput`. When the `registry` feature is enabled,
//! [`crate::registry`] adds `From<GifError> for oxideav_core::Error` so
//! the framework `Decoder` / `Encoder` can bubble these up unchanged.

use core::fmt;

/// Result alias used throughout the crate.
pub type Result<T> = core::result::Result<T, GifError>;

/// The contract name for [`GifError`].
pub type Error = GifError;

/// All recoverable failures produced by encoding or decoding.
///
/// Carries a `std::io::Error` in [`GifError::Io`], so the enum does not
/// implement `Clone` / `PartialEq`; tests match on variants or on
/// `Display`.
#[derive(Debug)]
#[non_exhaustive]
pub enum GifError {
    /// The byte stream is structurally invalid for GIF87a / GIF89a as
    /// defined by the CompuServe specifications.
    InvalidData(String),

    /// The byte stream is structurally well-formed but uses a feature
    /// outside the implemented subset, or the encoder was asked for an
    /// image GIF cannot represent (a layout other than `Pal8` /
    /// `Rgb24` / `Rgba`, dimensions above 65 535).
    Unsupported(String),

    /// A [`crate::DecodeOptions`] limit (dimensions / pixels / bytes)
    /// would be exceeded; nothing was allocated.
    LimitExceeded(String),

    /// A read / write on a caller-supplied stream failed
    /// ([`crate::decode_from`] / [`crate::encode_to`]).
    Io(std::io::Error),

    /// The byte stream ended before a required field could be read.
    UnexpectedEof,

    /// An encoder input violates a constraint declared by the spec
    /// (palette > 256 entries, dimensions > 65535, pixel index outside
    /// the colour table, etc.).
    InvalidInput(String),
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

    /// Construct a [`GifError::LimitExceeded`] from a stringy message.
    pub fn limit(msg: impl Into<String>) -> Self {
        Self::LimitExceeded(msg.into())
    }

    /// Construct a [`GifError::InvalidInput`] from a stringy message.
    pub fn invalid_input(msg: impl Into<String>) -> Self {
        Self::InvalidInput(msg.into())
    }
}

impl From<std::io::Error> for GifError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl fmt::Display for GifError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GifError::InvalidData(s) => write!(f, "invalid GIF data: {s}"),
            GifError::Unsupported(s) => write!(f, "unsupported GIF feature: {s}"),
            GifError::LimitExceeded(s) => write!(f, "limit exceeded: {s}"),
            GifError::Io(e) => write!(f, "io: {e}"),
            GifError::UnexpectedEof => write!(f, "unexpected end of stream"),
            GifError::InvalidInput(s) => write!(f, "invalid encoder input: {s}"),
        }
    }
}

impl std::error::Error for GifError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GifError::Io(e) => Some(e),
            _ => None,
        }
    }
}
