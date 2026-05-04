//! Local error type used by `oxideav-gif`'s standalone (no
//! `oxideav-core`) public API.
//!
//! When the `registry` feature is enabled, [`GifError`] gains a
//! `From<GifError> for oxideav_core::Error` impl (defined in
//! [`crate::registry`]) so the trait-side surface (`Decoder` /
//! `Encoder` / `Demuxer` / `Muxer`) can keep returning
//! `oxideav_core::Result<T>` while the underlying decode/encode
//! functions stay framework-free.

use std::fmt;

/// `Result` alias scoped to `oxideav-gif`. Standalone (no
/// `oxideav-core`) callers see this; framework callers convert via
/// the gated `From<GifError> for oxideav_core::Error` impl.
pub type Result<T> = std::result::Result<T, GifError>;

/// Error variants returned by `oxideav-gif`'s standalone API.
///
/// The variants mirror the subset of `oxideav_core::Error` the codec
/// can hit. The crate intentionally avoids surfacing transport (`Io`)
/// or framework-specific (`FormatNotFound`, `CodecNotFound`) errors —
/// those originate in callers that are already linking `oxideav-core`.
#[derive(Debug)]
pub enum GifError {
    /// The input bitstream / chunk stream is malformed (bad signature,
    /// truncated block, bad LZW code, etc.).
    InvalidData(String),
    /// The bitstream uses a feature this decoder doesn't implement,
    /// or the encoder was asked to emit a frame format it doesn't
    /// support.
    Unsupported(String),
    /// End of stream — no more packets / frames forthcoming.
    Eof,
    /// More input is required before another frame can be produced
    /// (decoder) or another packet can be flushed (encoder).
    NeedMore,
    /// Catch-all for everything else (e.g. caller protocol violations
    /// the trait surface needs to surface as `Other`).
    Other(String),
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

    /// Construct a [`GifError::Other`] from a stringy message.
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}

impl fmt::Display for GifError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidData(s) => write!(f, "invalid data: {s}"),
            Self::Unsupported(s) => write!(f, "unsupported: {s}"),
            Self::Eof => write!(f, "end of stream"),
            Self::NeedMore => write!(f, "need more data"),
            Self::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for GifError {}
