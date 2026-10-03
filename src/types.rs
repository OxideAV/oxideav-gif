//! The standalone image types: the shapes every `oxideav-<format>`
//! image crate shares (`IMAGE_CRATE_API`), specialised for GIF.
//!
//! * [`GifImage`] — the native-layout image [`crate::decode`] returns
//!   and [`crate::encode`] consumes: dimensions, a [`PixelFormat`] tag,
//!   one packed [`Plane`], [`ColorInfo`], [`Metadata`] and (for `Pal8`)
//!   a [`Palette`].
//! * [`RgbImage`] / [`RgbaImage`] — the tightly packed 8-bit raw paths
//!   ([`crate::decode_rgb8`] / [`crate::decode_rgba8`],
//!   [`GifImage::to_rgb8`] / [`GifImage::to_rgba8`]).
//! * [`Frame`] — one entry of [`crate::decode_all`]: a composited
//!   logical-screen canvas plus its §23 Graphic Control timing.
//! * [`ImageInfo`] — what [`crate::info`] reads from the header walk.
//!
//! The parsed GIF Data Stream itself (every block, every colour table,
//! the raw §22 index rasters) is [`crate::GifFile`]; these types are
//! the common floor every image crate shares, not a replacement for it.
//!
//! When the `registry` feature is enabled, [`crate::registry`] adds the
//! `From<GifImage> for oxideav_core::VideoFrame` conversion and its
//! inverse so the framework `Decoder` / `Encoder` are thin adapters
//! over the same functions.

use core::time::Duration;

use crate::error::{GifError, Result};
use crate::image::{DisposalMethod, Rgb, Version};

/// Pixel layouts the standalone `oxideav-gif` API can produce / consume.
///
/// Variant names mirror `oxideav_core::PixelFormat` exactly, so the
/// [`crate::registry`] conversion layer is a 1:1 match-and-rebuild
/// rather than a re-pack. Every GIF layout is packed (one plane).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GifPixelFormat {
    /// 8-bit palette index, 1 byte per pixel. The matching palette
    /// lives on [`GifImage::palette`]. The layout [`crate::decode`]
    /// returns for a frame whose composition is representable with one
    /// colour table (see the crate docs).
    Pal8,
    /// 8-bit RGB, 3 bytes per pixel. Accepted by [`crate::encode`]
    /// (quantised to a colour table); never produced by the decoder.
    Rgb24,
    /// 8-bit RGBA, 4 bytes per pixel. The layout of every composited
    /// [`crate::decode_all`] canvas, and of [`crate::decode`] when the
    /// first frame's composition needs more than 256 colour-table
    /// entries; accepted by [`crate::encode`] (quantised, alpha below
    /// [`crate::quantize::ALPHA_OPAQUE_THRESHOLD`] → the §23.c.viii
    /// Transparency Index).
    Rgba,
}

/// The contract name for [`GifPixelFormat`].
pub type PixelFormat = GifPixelFormat;

impl GifPixelFormat {
    /// Bytes per pixel for the given pixel format.
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Pal8 => 1,
            Self::Rgb24 => 3,
            Self::Rgba => 4,
        }
    }

    /// `true` when the layout carries an alpha channel of its own
    /// (`Rgba`). Palette transparency is not part of the layout; see
    /// [`GifImage::has_alpha`].
    pub fn has_alpha(self) -> bool {
        matches!(self, Self::Rgba)
    }
}

/// One pixel plane: `stride` bytes per row, `data` holding
/// `stride × height` bytes (rows may carry padding past the visible
/// width). GIF layouts are packed, so a [`GifImage`] has exactly one.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Plane {
    /// Bytes per row.
    pub stride: usize,
    /// Row-major bytes, `stride × height` long.
    pub data: Vec<u8>,
}

impl Plane {
    /// Wrap a plane buffer with its row stride.
    pub fn new(stride: usize, data: Vec<u8>) -> Self {
        Self { stride, data }
    }
}

/// Nominal sample range (H.273 `VideoFullRangeFlag`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ColorRange {
    /// No range was signalled.
    #[default]
    Unspecified,
    /// Limited (video / studio) range: `VideoFullRangeFlag == 0`.
    Limited,
    /// Full (PC) range: `VideoFullRangeFlag == 1`.
    Full,
}

/// Colour signalling of an image: the sample range plus the H.273
/// `ColourPrimaries` / `TransferCharacteristics` /
/// `MatrixCoefficients` code points (`2` = unspecified).
///
/// GIF carries no colour signalling of its own (the CompuServe
/// specifications describe colour-table entries as device RGB), so
/// [`crate::decode`] always fills [`ColorInfo::gif_default`]: full-range
/// RGB (`matrix` 0) with unspecified primaries and transfer. An embedded
/// ICC profile (the `ICCRGBG1012` Application Extension) is surfaced on
/// [`Metadata::icc`] and governs the colours when present.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ColorInfo {
    /// Sample range.
    pub range: ColorRange,
    /// H.273 `ColourPrimaries` code point (`1` = BT.709 / sRGB, `2` =
    /// unspecified).
    pub primaries: u8,
    /// H.273 `TransferCharacteristics` code point (`13` = sRGB, `2` =
    /// unspecified).
    pub transfer: u8,
    /// H.273 `MatrixCoefficients` code point (`0` = identity / RGB).
    pub matrix: u8,
}

impl ColorInfo {
    /// H.273 "unspecified" code point.
    pub const UNSPECIFIED: u8 = 2;
    /// H.273 `MatrixCoefficients` identity (RGB / GBR) code point.
    pub const MATRIX_IDENTITY: u8 = 0;
    /// H.273 `ColourPrimaries` BT.709 / sRGB code point.
    pub const PRIMARIES_BT709: u8 = 1;
    /// H.273 `TransferCharacteristics` IEC 61966-2-1 sRGB code point.
    pub const TRANSFER_SRGB: u8 = 13;

    /// Build a description from its four parts.
    pub const fn new(range: ColorRange, primaries: u8, transfer: u8, matrix: u8) -> Self {
        Self {
            range,
            primaries,
            transfer,
            matrix,
        }
    }

    /// Every field unspecified.
    pub const fn unspecified() -> Self {
        Self::new(
            ColorRange::Unspecified,
            Self::UNSPECIFIED,
            Self::UNSPECIFIED,
            Self::UNSPECIFIED,
        )
    }

    /// GIF's documented default: full-range RGB (`matrix` 0) with
    /// unspecified primaries and transfer — the CompuServe
    /// specifications define colour-table entries as device RGB and
    /// carry no colour-space signalling.
    pub const fn gif_default() -> Self {
        Self::new(
            ColorRange::Full,
            Self::UNSPECIFIED,
            Self::UNSPECIFIED,
            Self::MATRIX_IDENTITY,
        )
    }

    /// sRGB (IEC 61966-2-1): BT.709 primaries, sRGB transfer, identity
    /// matrix, full range. Never produced by the decoder (GIF cannot
    /// signal it); offered for callers that assemble images.
    pub const fn srgb() -> Self {
        Self::new(
            ColorRange::Full,
            Self::PRIMARIES_BT709,
            Self::TRANSFER_SRGB,
            Self::MATRIX_IDENTITY,
        )
    }

    /// Set the range.
    pub fn with_range(mut self, range: ColorRange) -> Self {
        self.range = range;
        self
    }

    /// Set the primaries code point.
    pub fn with_primaries(mut self, primaries: u8) -> Self {
        self.primaries = primaries;
        self
    }

    /// Set the transfer code point.
    pub fn with_transfer(mut self, transfer: u8) -> Self {
        self.transfer = transfer;
        self
    }

    /// Set the matrix code point.
    pub fn with_matrix(mut self, matrix: u8) -> Self {
        self.matrix = matrix;
        self
    }

    /// `true` when both primaries and transfer are specified (`!= 2`).
    pub fn is_specified(&self) -> bool {
        self.primaries != Self::UNSPECIFIED && self.transfer != Self::UNSPECIFIED
    }
}

impl Default for ColorInfo {
    /// [`ColorInfo::gif_default`].
    fn default() -> Self {
        Self::gif_default()
    }
}

/// The metadata blobs every image crate surfaces: an ICC profile, an
/// Exif payload, an XMP packet and a file gamma. GIF sources the first
/// three from the de-facto §26 Application Extensions
/// (`ICCRGBG1012`, `Exif`, `XMP DataXMP` — see [`crate::app_ext`]);
/// GIF has no gamma field, so `gamma` is always `None` on decode and
/// ignored on encode.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Metadata {
    /// ICC profile bytes (`ICCRGBG1012` Application Extension payload).
    pub icc: Option<Vec<u8>>,
    /// Exif payload starting at the TIFF header (`Exif` Application
    /// Extension payload).
    pub exif: Option<Vec<u8>>,
    /// XMP packet bytes (`XMP DataXMP` Application Extension payload,
    /// without the 258-byte magic trailer).
    pub xmp: Option<Vec<u8>>,
    /// File gamma. GIF has no such field; always `None` on decode.
    pub gamma: Option<f32>,
}

impl Metadata {
    /// Empty metadata.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or clear) the ICC profile.
    pub fn with_icc(mut self, icc: impl Into<Option<Vec<u8>>>) -> Self {
        self.icc = icc.into();
        self
    }

    /// Set (or clear) the Exif payload.
    pub fn with_exif(mut self, exif: impl Into<Option<Vec<u8>>>) -> Self {
        self.exif = exif.into();
        self
    }

    /// Set (or clear) the XMP packet.
    pub fn with_xmp(mut self, xmp: impl Into<Option<Vec<u8>>>) -> Self {
        self.xmp = xmp.into();
        self
    }

    /// Set (or clear) the file gamma.
    pub fn with_gamma(mut self, gamma: impl Into<Option<f32>>) -> Self {
        self.gamma = gamma.into();
        self
    }

    /// `true` when no field is set.
    pub fn is_empty(&self) -> bool {
        self.icc.is_none() && self.exif.is_none() && self.xmp.is_none() && self.gamma.is_none()
    }
}

/// Colour table of an indexed (`Pal8`) image: RGBA entries, index `i`
/// at `entries[i]`. GIF builds it from the active §19 Global / §21 Local
/// Color Table (RGB, alpha 255) with the §23.c.viii Transparency Index
/// entry at alpha 0; the encoder writes the RGB back and marks every
/// entry with alpha below [`crate::quantize::ALPHA_OPAQUE_THRESHOLD`]
/// as the transparent index (see [`crate::encode`]).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Palette {
    /// `[r, g, b, a]` per entry, at most 256 entries.
    pub entries: Vec<[u8; 4]>,
}

impl Palette {
    /// Wrap a list of RGBA entries.
    pub fn new(entries: Vec<[u8; 4]>) -> Self {
        Self { entries }
    }

    /// Build from a GIF colour table and an optional transparent
    /// index: every entry opaque except `transparent`, which gets
    /// alpha 0 (an index past the table is ignored).
    pub fn from_color_table(table: &[Rgb], transparent: Option<u8>) -> Self {
        let entries = table
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let a = if transparent == Some(i as u8) { 0 } else { 255 };
                [c.r, c.g, c.b, a]
            })
            .collect();
        Self { entries }
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when the palette has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entry `index`, if present.
    pub fn get(&self, index: u8) -> Option<[u8; 4]> {
        self.entries.get(usize::from(index)).copied()
    }

    /// The RGB part of every entry as GIF colour-table triplets.
    pub fn to_color_table(&self) -> Vec<Rgb> {
        self.entries
            .iter()
            .map(|e| Rgb::new(e[0], e[1], e[2]))
            .collect()
    }

    /// Index of the first entry whose alpha is below
    /// [`crate::quantize::ALPHA_OPAQUE_THRESHOLD`] — what
    /// [`crate::encode`] writes as the §23.c.viii Transparency Index —
    /// or `None` when every entry is opaque enough.
    pub fn transparent_index(&self) -> Option<u8> {
        self.entries
            .iter()
            .position(|e| e[3] < crate::quantize::ALPHA_OPAQUE_THRESHOLD)
            .map(|i| i as u8)
    }

    /// `true` when any entry is not fully opaque.
    pub fn has_alpha(&self) -> bool {
        self.entries.iter().any(|e| e[3] != 255)
    }
}

/// Decoded GIF image in its native layout, as returned by
/// [`crate::decode`] and consumed by [`crate::encode`].
///
/// `planes` holds exactly one packed plane (every GIF layout is
/// packed); `color` is [`ColorInfo::gif_default`]; `metadata` is filled
/// from the ICC / Exif / XMP Application Extensions; `palette` is
/// `Some` for `Pal8`.
///
/// [`crate::decode`] returns the **first §20 image composed onto the
/// §18 Logical Screen**: pixels outside the image rectangle and pixels
/// at the §23.c.viii Transparency Index are transparent, exactly as
/// [`crate::compose()`] renders them. The layout is `Pal8` with the
/// frame's effective colour table as `palette` whenever one table can
/// describe that canvas (the frame covers the screen, or it has a
/// transparent index, or the table has a free slot for a synthetic
/// transparent entry); otherwise it is `Rgba`.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct GifImage {
    /// Image width in pixels (the §18 Logical Screen width).
    pub width: u32,
    /// Image height in pixels (the §18 Logical Screen height).
    pub height: u32,
    /// Native pixel layout.
    pub format: PixelFormat,
    /// Pixel planes — exactly one for GIF.
    pub planes: Vec<Plane>,
    /// Colour signalling (range + H.273 code points).
    pub color: ColorInfo,
    /// ICC / Exif / XMP.
    pub metadata: Metadata,
    /// Colour table for `Pal8`.
    pub palette: Option<Palette>,
}

impl GifImage {
    /// Assemble an image from its geometry, layout and planes (one for
    /// GIF), validating the geometry: exactly one plane, `stride ≥
    /// width × bytes_per_pixel`, `data.len() == stride × height`,
    /// and `width` / `height` at most 65 535 (the §18 / §20 wire
    /// fields are 16-bit). Colour is [`ColorInfo::gif_default`],
    /// metadata empty, no palette; the `with_*` builders fill those in.
    pub fn new(width: u32, height: u32, format: PixelFormat, planes: Vec<Plane>) -> Result<Self> {
        if width > u32::from(u16::MAX) || height > u32::from(u16::MAX) {
            return Err(GifError::unsupported(format!(
                "GIF: {width}x{height} exceeds the 65535x65535 wire limit"
            )));
        }
        let [plane] = planes.as_slice() else {
            return Err(GifError::invalid_input(format!(
                "GIF: expected exactly one packed plane, got {}",
                planes.len()
            )));
        };
        let row = (width as usize)
            .checked_mul(format.bytes_per_pixel())
            .ok_or_else(|| GifError::invalid_input("GIF: row size overflow"))?;
        if plane.stride < row {
            return Err(GifError::invalid_input(format!(
                "GIF: stride {} is smaller than the {row}-byte row",
                plane.stride
            )));
        }
        let need = plane
            .stride
            .checked_mul(height as usize)
            .ok_or_else(|| GifError::invalid_input("GIF: plane size overflow"))?;
        if plane.data.len() != need {
            return Err(GifError::invalid_input(format!(
                "GIF: plane holds {} bytes, stride {} x height {height} needs {need}",
                plane.data.len(),
                plane.stride
            )));
        }
        Ok(Self {
            width,
            height,
            format,
            planes,
            color: ColorInfo::gif_default(),
            metadata: Metadata::default(),
            palette: None,
        })
    }

    /// One packed plane with an explicit row stride (`stride ≥ width ×
    /// bytes_per_pixel`, `data.len() == stride × height`).
    pub fn packed(
        width: u32,
        height: u32,
        format: PixelFormat,
        stride: usize,
        data: Vec<u8>,
    ) -> Result<Self> {
        Self::new(width, height, format, vec![Plane::new(stride, data)])
    }

    /// Tightly packed `Rgb24` from `3 × width × height` bytes.
    pub fn from_rgb8(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        Self::packed(width, height, PixelFormat::Rgb24, width as usize * 3, data)
    }

    /// Tightly packed `Rgba` from `4 × width × height` bytes.
    pub fn from_rgba8(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        Self::packed(width, height, PixelFormat::Rgba, width as usize * 4, data)
    }

    /// Tightly packed `Pal8` from `width × height` index bytes and a
    /// palette. Indices past the palette are rejected.
    pub fn from_indexed(
        width: u32,
        height: u32,
        indices: Vec<u8>,
        palette: Palette,
    ) -> Result<Self> {
        if palette.is_empty() || palette.len() > 256 {
            return Err(GifError::invalid_input(format!(
                "GIF: palette must hold 1..=256 entries, got {}",
                palette.len()
            )));
        }
        if let Some(bad) = indices.iter().find(|&&i| usize::from(i) >= palette.len()) {
            return Err(GifError::invalid_input(format!(
                "GIF: index {bad} is past the {}-entry palette",
                palette.len()
            )));
        }
        let img = Self::packed(width, height, PixelFormat::Pal8, width as usize, indices)?;
        Ok(img.with_palette(palette))
    }

    /// Set the colour signalling.
    pub fn with_color(mut self, color: ColorInfo) -> Self {
        self.color = color;
        self
    }

    /// Set the metadata.
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Set (or clear) the palette.
    pub fn with_palette(mut self, palette: impl Into<Option<Palette>>) -> Self {
        self.palette = palette.into();
        self
    }

    /// Image width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Image height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Native pixel layout.
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// Number of bytes per pixel for [`Self::format`].
    pub fn bytes_per_pixel(&self) -> usize {
        self.format.bytes_per_pixel()
    }

    /// Row stride in bytes of the pixel plane (`0` if the image has no
    /// plane).
    pub fn stride(&self) -> usize {
        self.planes.first().map(|p| p.stride).unwrap_or(0)
    }

    /// The pixel bytes — `Some` for every GIF image that has its plane
    /// (GIF layouts are all packed), `None` only for an image built
    /// without planes.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        self.planes.first().map(|p| p.data.as_slice())
    }

    /// Consume the image and return its plane bytes (planes
    /// concatenated in order, strides as reported).
    pub fn into_raw(self) -> Vec<u8> {
        let mut planes = self.planes.into_iter();
        let mut out = planes.next().map(|p| p.data).unwrap_or_default();
        for p in planes {
            out.extend_from_slice(&p.data);
        }
        out
    }

    /// `true` when the decoded pixels can be transparent: an alpha
    /// layout or a non-opaque palette entry.
    pub fn has_alpha(&self) -> bool {
        self.format.has_alpha() || self.palette.as_ref().is_some_and(Palette::has_alpha)
    }

    /// Pixel bytes of the single plane (empty if none).
    pub(crate) fn data(&self) -> &[u8] {
        self.as_bytes().unwrap_or(&[])
    }

    /// Palette lookup table: 256 RGBA cells; entries the palette does
    /// not cover are black-transparent.
    fn palette_lut(&self) -> [[u8; 4]; 256] {
        let mut lut = [[0u8; 4]; 256];
        if let Some(p) = &self.palette {
            for (slot, e) in lut.iter_mut().zip(p.entries.iter()) {
                *slot = *e;
            }
        }
        lut
    }

    /// Tightly packed 8-bit RGBA, `4 × width` bytes per row, alpha
    /// `255` where the source has none.
    ///
    /// Exact kernels per layout — no colour management is applied:
    ///
    /// | Source  | RGBA |
    /// |---------|------|
    /// | `Pal8`  | palette lookup (RGBA entry); an index past the palette is black-transparent |
    /// | `Rgb24` | `(r, g, b, 255)` |
    /// | `Rgba`  | copy |
    pub fn to_rgba8(&self) -> Vec<u8> {
        let w = self.width as usize;
        let h = self.height as usize;
        let mut out = vec![0u8; w * h * 4];
        if w == 0 || h == 0 {
            return out;
        }
        let stride = self.stride();
        let src = self.data();
        match self.format {
            PixelFormat::Pal8 => {
                let lut = self.palette_lut();
                for y in 0..h {
                    let row = &src[y * stride..y * stride + w];
                    let dst = &mut out[y * w * 4..(y + 1) * w * 4];
                    for (d, &i) in dst.chunks_exact_mut(4).zip(row) {
                        d.copy_from_slice(&lut[usize::from(i)]);
                    }
                }
            }
            PixelFormat::Rgb24 => {
                for y in 0..h {
                    let row = &src[y * stride..y * stride + w * 3];
                    let dst = &mut out[y * w * 4..(y + 1) * w * 4];
                    for (d, s) in dst.chunks_exact_mut(4).zip(row.chunks_exact(3)) {
                        d[..3].copy_from_slice(s);
                        d[3] = 255;
                    }
                }
            }
            PixelFormat::Rgba => {
                for y in 0..h {
                    out[y * w * 4..(y + 1) * w * 4]
                        .copy_from_slice(&src[y * stride..y * stride + w * 4]);
                }
            }
        }
        out
    }

    /// Tightly packed 8-bit RGB, `3 × width` bytes per row; alpha is
    /// dropped (no compositing). Same kernels as [`Self::to_rgba8`]
    /// without the alpha byte.
    pub fn to_rgb8(&self) -> Vec<u8> {
        let w = self.width as usize;
        let h = self.height as usize;
        let mut out = vec![0u8; w * h * 3];
        if w == 0 || h == 0 {
            return out;
        }
        let stride = self.stride();
        let src = self.data();
        match self.format {
            PixelFormat::Pal8 => {
                let lut = self.palette_lut();
                for y in 0..h {
                    let row = &src[y * stride..y * stride + w];
                    let dst = &mut out[y * w * 3..(y + 1) * w * 3];
                    for (d, &i) in dst.chunks_exact_mut(3).zip(row) {
                        d.copy_from_slice(&lut[usize::from(i)][..3]);
                    }
                }
            }
            PixelFormat::Rgb24 => {
                for y in 0..h {
                    out[y * w * 3..(y + 1) * w * 3]
                        .copy_from_slice(&src[y * stride..y * stride + w * 3]);
                }
            }
            PixelFormat::Rgba => {
                for y in 0..h {
                    let row = &src[y * stride..y * stride + w * 4];
                    let dst = &mut out[y * w * 3..(y + 1) * w * 3];
                    for (d, s) in dst.chunks_exact_mut(3).zip(row.chunks_exact(4)) {
                        d.copy_from_slice(&s[..3]);
                    }
                }
            }
        }
        out
    }
}

/// Tightly packed 8-bit RGB image: `width × height × 3` bytes,
/// row-major, channel order `R, G, B`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RgbImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// `width × height × 3` bytes.
    pub data: Vec<u8>,
}

impl RgbImage {
    /// Wrap a tightly packed `width × height × 3` RGB buffer.
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        Self {
            width,
            height,
            data,
        }
    }

    /// The pixel bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Consume the image and return the pixel bytes.
    pub fn into_raw(self) -> Vec<u8> {
        self.data
    }

    /// Stride (bytes per row) — always `width × 3`.
    pub fn stride(&self) -> usize {
        self.width as usize * 3
    }
}

/// Tightly packed 8-bit RGBA image: `width × height × 4` bytes,
/// row-major, channel order `R, G, B, A`. Opaque source layouts are
/// promoted with `α = 255`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RgbaImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// `width × height × 4` bytes.
    pub data: Vec<u8>,
}

impl RgbaImage {
    /// Wrap a tightly packed `width × height × 4` RGBA buffer.
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        Self {
            width,
            height,
            data,
        }
    }

    /// The pixel bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Consume the image and return the pixel bytes.
    pub fn into_raw(self) -> Vec<u8> {
        self.data
    }

    /// Stride (bytes per row) — always `width × 4`.
    pub fn stride(&self) -> usize {
        self.width as usize * 4
    }
}

/// One image of a multi-image file ([`crate::decode_all`]): the fully
/// composited §18 Logical Screen after one graphic-rendering block
/// (§20 image or §25 Plain Text) has been rendered, per the §23
/// disposal-method state machine of [`crate::compose()`], plus that
/// block's §23 Graphic Control parameters.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Frame {
    /// The composited canvas (`Rgba`, the logical-screen size).
    pub image: GifImage,
    /// Display delay (§23.c.vii Delay Time × 10 ms); `None` when the
    /// block carried no Graphic Control Extension.
    pub delay: Option<Duration>,
    /// §23.c.iv Disposal Method applied after this frame
    /// ([`DisposalMethod::None`] without a GCE).
    pub disposal: DisposalMethod,
    /// §23.c.v User Input Flag (`false` without a GCE).
    pub user_input: bool,
}

impl Frame {
    /// Pair an image with its display delay; no disposal, no user
    /// input.
    pub fn new(image: GifImage, delay: Option<Duration>) -> Self {
        Self {
            image,
            delay,
            disposal: DisposalMethod::None,
            user_input: false,
        }
    }

    /// Set the disposal method.
    pub fn with_disposal(mut self, disposal: DisposalMethod) -> Self {
        self.disposal = disposal;
        self
    }

    /// Set the user-input flag.
    pub fn with_user_input(mut self, user_input: bool) -> Self {
        self.user_input = user_input;
        self
    }
}

/// What [`crate::info`] learns from the header walk without decoding
/// pixels (no LZW stream is expanded).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ImageInfo {
    /// Logical Screen width in pixels (§18.c.i).
    pub width: u32,
    /// Logical Screen height in pixels (§18.c.ii).
    pub height: u32,
    /// The layout [`crate::decode`] would return.
    pub format: PixelFormat,
    /// Number of graphic-rendering blocks (§20 images + §25 Plain Text
    /// blocks) — the length of [`crate::decode_all`].
    pub frames: u32,
    /// `true` when [`crate::decode`] can return transparent pixels:
    /// the first image has a §23.c.viii Transparency Index or does not
    /// cover the whole Logical Screen.
    pub has_alpha: bool,
    /// Colour signalling ([`ColorInfo::gif_default`]).
    pub color: ColorInfo,
    /// An `ICCRGBG1012` Application Extension is present.
    pub has_icc: bool,
    /// An `Exif` Application Extension is present.
    pub has_exif: bool,
    /// An `XMP DataXMP` Application Extension is present.
    pub has_xmp: bool,
    /// §17.c.ii version (`GIF87a` / `GIF89a`).
    pub version: Version,
    /// Number of §20 image blocks (excludes Plain Text).
    pub image_count: u32,
    /// `true` when a §19 Global Color Table is present.
    pub has_global_palette: bool,
    /// Entries in the first image's effective colour table (`0` when
    /// it has none).
    pub palette_entries: u32,
    /// `true` when the first image is stored interlaced (Appendix E).
    pub interlaced: bool,
    /// NETSCAPE2.0 / ANIMEXTS1.0 loop count: `None` when absent (play
    /// once), `Some(0)` = forever, `Some(n)` = `n` repeats.
    pub loop_count: Option<u16>,
    /// §18.c.viii Pixel Aspect Ratio raw byte (`0` = none given).
    pub pixel_aspect_ratio: u8,
}

impl ImageInfo {
    /// Build a header description; `frames` 1, no alpha / metadata
    /// flags, default colour — fill the rest with field assignment.
    pub fn new(width: u32, height: u32, format: PixelFormat) -> Self {
        Self {
            width,
            height,
            format,
            frames: 1,
            has_alpha: format.has_alpha(),
            color: ColorInfo::gif_default(),
            has_icc: false,
            has_exif: false,
            has_xmp: false,
            version: Version::Gif89a,
            image_count: 1,
            has_global_palette: false,
            palette_entries: 0,
            interlaced: false,
            loop_count: None,
            pixel_aspect_ratio: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_validates_geometry() {
        assert!(GifImage::new(2, 2, PixelFormat::Pal8, vec![Plane::new(2, vec![0; 4])]).is_ok());
        assert!(matches!(
            GifImage::new(2, 2, PixelFormat::Pal8, vec![Plane::new(1, vec![0; 4])]),
            Err(GifError::InvalidInput(_))
        ));
        assert!(matches!(
            GifImage::new(2, 2, PixelFormat::Rgb24, vec![Plane::new(6, vec![0; 11])]),
            Err(GifError::InvalidInput(_))
        ));
        assert!(matches!(
            GifImage::new(2, 2, PixelFormat::Rgba, vec![]),
            Err(GifError::InvalidInput(_))
        ));
        assert!(matches!(
            GifImage::from_rgb8(70_000, 1, vec![0; 210_000]),
            Err(GifError::Unsupported(_))
        ));
        // Padded stride is fine when the data length matches it.
        let img = GifImage::packed(2, 2, PixelFormat::Rgb24, 8, vec![0; 16]).unwrap();
        assert_eq!(img.stride(), 8);
        assert_eq!(img.to_rgb8().len(), 12);
    }

    #[test]
    fn palette_expansion_and_alpha() {
        let pal = Palette::from_color_table(
            &[
                Rgb::new(10, 20, 30),
                Rgb::new(40, 50, 60),
                Rgb::new(70, 80, 90),
            ],
            Some(1),
        );
        assert_eq!(pal.transparent_index(), Some(1));
        assert!(pal.has_alpha());
        let img = GifImage::from_indexed(3, 1, vec![0, 1, 2], pal).unwrap();
        assert!(img.has_alpha());
        assert_eq!(
            img.to_rgba8(),
            vec![10, 20, 30, 255, 40, 50, 60, 0, 70, 80, 90, 255]
        );
        assert_eq!(img.to_rgb8(), vec![10, 20, 30, 40, 50, 60, 70, 80, 90]);
        assert!(matches!(
            GifImage::from_indexed(1, 1, vec![9], Palette::new(vec![[0, 0, 0, 255]])),
            Err(GifError::InvalidInput(_))
        ));
    }

    #[test]
    fn rgb_and_rgba_kernels() {
        let rgb = GifImage::from_rgb8(2, 1, vec![1, 2, 3, 4, 5, 6]).unwrap();
        assert_eq!(rgb.to_rgba8(), vec![1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(rgb.as_bytes(), Some(&[1u8, 2, 3, 4, 5, 6][..]));
        let rgba = GifImage::from_rgba8(2, 1, vec![1, 2, 3, 9, 4, 5, 6, 8]).unwrap();
        assert_eq!(rgba.to_rgb8(), vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(rgba.clone().into_raw(), vec![1, 2, 3, 9, 4, 5, 6, 8]);
        assert!(rgba.has_alpha());
        assert!(!rgb.has_alpha());
    }
}
