//! Decode-side options: resource limits, strictness and recovery
//! ([`DecodeOptions`]). The encode-side [`crate::EncodeOptions`] lives
//! next to the encoder.

use crate::error::{GifError, Result};

/// Limits, strictness and recovery mode for [`crate::decode_with`] /
/// [`crate::decode_all_with`] / [`crate::parse_with`].
///
/// Every limit is checked against the header fields **before** any
/// pixel buffer is allocated, so a hostile Logical Screen Descriptor or
/// Image Descriptor fails with [`GifError::LimitExceeded`] instead of
/// committing memory. The defaults are: no dimension / pixel-count
/// limit, decoded bytes capped at [`DecodeOptions::DEFAULT_MAX_BYTES`]
/// (1 GiB), `strict = false`, `lenient = false`.
///
/// What each limit measures:
///
/// * `max_width` / `max_height` / `max_pixels` — the §18 Logical
///   Screen (the dimensions of every image this crate returns);
///   `max_pixels` is also applied to each §20 image rectangle.
/// * `max_bytes` — every buffer the call produces: each frame's §22
///   index raster as it is expanded, the running total of those
///   rasters across the file, the `Pal8` / `Rgba` canvas of
///   [`crate::decode`], and `frames × canvas` for
///   [`crate::decode_all`].
///
/// `strict` and `lenient` select how departures from the CompuServe
/// specifications are treated:
///
/// * always: the §17 header, §18 Logical Screen Descriptor and §19
///   Global Color Table must parse; every §20 image must fit the
///   Logical Screen, reference a colour table and stay inside it;
/// * `strict = false`, `lenient = false` (default): a stream must
///   be structurally complete (every block well-formed, §27 Trailer
///   present), while §7–§26 *field* rules the spec words as "should"
///   or that do not affect rendering (reserved disposal values,
///   out-of-range background / transparent indices, Plain Text
///   grid oddities, version-label mismatches) are tolerated;
/// * `strict = true`: additionally run the conformance walk of
///   [`crate::GifFile::validate_strict`] — every error-level
///   [`crate::ConformanceIssue`] becomes [`GifError::InvalidData`];
/// * `lenient = true`: the recovery parser of
///   [`crate::parse_lenient`] — a malformed block past the header /
///   LSD / GCT prefix is skipped and parsing resumes at the next §20
///   Image Separator or §27 Trailer; a missing Trailer is accepted.
///   `strict` and `lenient` together is an error.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecodeOptions {
    /// Reject Logical Screens wider than this (pixels).
    pub max_width: Option<u32>,
    /// Reject Logical Screens taller than this (pixels).
    pub max_height: Option<u32>,
    /// Reject Logical Screens / image rectangles with more than this
    /// many pixels (`width × height`).
    pub max_pixels: Option<u64>,
    /// Reject decodes whose buffers would exceed this many bytes (see
    /// the type docs for what is measured).
    pub max_bytes: Option<u64>,
    /// Enforce the §7–§26 field rules via the conformance walk (see
    /// the type docs).
    pub strict: bool,
    /// Recover from malformed blocks instead of failing (see the type
    /// docs).
    pub lenient: bool,
}

impl DecodeOptions {
    /// Default [`Self::max_bytes`]: 1 GiB of decoded data.
    pub const DEFAULT_MAX_BYTES: u64 = 1 << 30;

    /// The defaults (see the type docs).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or lift with `None`) the width limit.
    pub fn with_max_width(mut self, max_width: impl Into<Option<u32>>) -> Self {
        self.max_width = max_width.into();
        self
    }

    /// Set (or lift with `None`) the height limit.
    pub fn with_max_height(mut self, max_height: impl Into<Option<u32>>) -> Self {
        self.max_height = max_height.into();
        self
    }

    /// Set (or lift with `None`) the pixel-count limit.
    pub fn with_max_pixels(mut self, max_pixels: impl Into<Option<u64>>) -> Self {
        self.max_pixels = max_pixels.into();
        self
    }

    /// Set (or lift with `None`) the decoded-bytes limit.
    pub fn with_max_bytes(mut self, max_bytes: impl Into<Option<u64>>) -> Self {
        self.max_bytes = max_bytes.into();
        self
    }

    /// Set strict mode.
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Set lenient (recovery) mode.
    pub fn with_lenient(mut self, lenient: bool) -> Self {
        self.lenient = lenient;
        self
    }

    /// Lift every limit (`max_*` all `None`).
    pub fn unlimited(mut self) -> Self {
        self.max_width = None;
        self.max_height = None;
        self.max_pixels = None;
        self.max_bytes = None;
        self
    }

    /// Check the Logical Screen geometry against the dimension / pixel
    /// limits and `bytes` against the byte limit.
    pub(crate) fn check_screen(&self, width: u32, height: u32, bytes: u64) -> Result<()> {
        if let Some(m) = self.max_width {
            if width > m {
                return Err(GifError::limit(format!(
                    "GIF: logical screen width {width} exceeds max_width {m}"
                )));
            }
        }
        if let Some(m) = self.max_height {
            if height > m {
                return Err(GifError::limit(format!(
                    "GIF: logical screen height {height} exceeds max_height {m}"
                )));
            }
        }
        self.check_pixels(width, height)?;
        self.check_bytes(bytes)
    }

    /// Check a rectangle's pixel count against `max_pixels`.
    pub(crate) fn check_pixels(&self, width: u32, height: u32) -> Result<()> {
        let pixels = u64::from(width) * u64::from(height);
        if let Some(m) = self.max_pixels {
            if pixels > m {
                return Err(GifError::limit(format!(
                    "GIF: {pixels} pixels exceed max_pixels {m}"
                )));
            }
        }
        Ok(())
    }

    /// Check a buffer size against `max_bytes`.
    pub(crate) fn check_bytes(&self, bytes: u64) -> Result<()> {
        if let Some(m) = self.max_bytes {
            if bytes > m {
                return Err(GifError::limit(format!(
                    "GIF: {bytes} decoded bytes exceed max_bytes {m}"
                )));
            }
        }
        Ok(())
    }
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            max_width: None,
            max_height: None,
            max_pixels: None,
            max_bytes: Some(Self::DEFAULT_MAX_BYTES),
            strict: false,
            lenient: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_fire_in_order() {
        let o = DecodeOptions::default()
            .with_max_width(10u32)
            .with_max_height(10u32)
            .with_max_pixels(50u64)
            .with_max_bytes(100u64);
        assert!(o.check_screen(5, 5, 75).is_ok());
        assert!(matches!(
            o.check_screen(11, 1, 1),
            Err(GifError::LimitExceeded(_))
        ));
        assert!(matches!(
            o.check_screen(1, 11, 1),
            Err(GifError::LimitExceeded(_))
        ));
        assert!(matches!(
            o.check_screen(8, 8, 1),
            Err(GifError::LimitExceeded(_))
        ));
        assert!(matches!(
            o.check_screen(5, 5, 101),
            Err(GifError::LimitExceeded(_))
        ));
        assert!(o
            .unlimited()
            .check_screen(u32::MAX, u32::MAX, u64::MAX)
            .is_ok());
    }
}
