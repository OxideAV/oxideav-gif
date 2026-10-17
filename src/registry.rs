//! `oxideav-core` integration layer for `oxideav-gif`.
//!
//! Gated behind the default-on `registry` Cargo feature so image-library
//! consumers can depend on `oxideav-gif` with `default-features = false`
//! and skip the `oxideav-core` dependency entirely.
//!
//! The module exposes:
//! * [`register`] / [`register_codecs`] / [`register_containers`] — the
//!   `RuntimeContext` / `CodecRegistry` / `ContainerRegistry` entry
//!   points the umbrella `oxideav` crate calls during framework
//!   initialisation.
//! * [`make_decoder`] / [`make_encoder`] — the codec factories, also
//!   installed in the registry.
//! * [`GifDecoder`] / [`GifEncoder`] — `Decoder` / `Encoder` trait
//!   wrappers that call the framework-free [`crate::decode_all`] and
//!   [`crate::encode`] (one implementation; the registry path is a thin
//!   adapter).
//! * `From<GifImage> for VideoFrame`, [`GifImage::from_video_frame`] and
//!   `TryFrom<(&VideoFrame, &CodecParameters)>` — the frame bridge, plus
//!   [`to_core_pixel_format`] / `TryFrom<PixelFormat>` for the
//!   pixel-format enums (1:1 by name) and [`to_color_signal`] /
//!   [`from_color_signal`].
//! * The `From<GifError> for oxideav_core::Error` conversion that lets
//!   trait impls bubble bitstream errors up through the framework error
//!   type.
//!
//! The `oxideav_core::register!` invocation at the bottom of the file
//! generates the `pub fn __oxideav_entry(ctx)` wrapper that
//! `oxideav-meta`'s `register_all` calls. `lib.rs` re-exports it at the
//! crate root (`oxideav_gif::__oxideav_entry`) which is the symbol meta
//! looks up.

use oxideav_core::{
    frame::VideoPlane, parse_options, BlobKind, CodecCapabilities, CodecId, CodecInfo,
    CodecOptionsStruct, CodecParameters, CodecRegistry, ColorPrimaries, ColorSignal,
    ContainerRegistry, Decoder, Encoder, Error as CoreError, Frame as CoreFrame,
    MatrixCoefficients, MetadataBlob, OptionField, OptionKind, OptionValue, Packet, PixelFormat,
    Result as CoreResult, RuntimeContext, TimeBase, TransferCharacteristics, VideoFrame,
};

use crate::app_ext::{
    EXIF_IDENTIFIER, ICC_AUTH_CODE, ICC_IDENTIFIER, XMP_AUTH_CODE, XMP_IDENTIFIER,
};
use crate::encoder::{EncodeOptions, LzwStrategy};
use crate::error::GifError;
use crate::image::{Application, Block, GifFile};
use crate::quantize::Dither;
use crate::types::{ColorInfo, ColorRange, GifImage, GifPixelFormat, Metadata, Palette};

/// Canonical codec id for GIF image frames.
pub const CODEC_ID_STR: &str = "gif";

impl From<GifError> for CoreError {
    fn from(e: GifError) -> Self {
        match e {
            GifError::InvalidData(s) => CoreError::InvalidData(s),
            GifError::Unsupported(s) => CoreError::Unsupported(s),
            GifError::LimitExceeded(s) => {
                CoreError::InvalidData(format!("gif: limit exceeded: {s}"))
            }
            GifError::Io(e) => CoreError::Io(e),
            GifError::UnexpectedEof => {
                CoreError::InvalidData("gif: unexpected end of stream".into())
            }
            GifError::InvalidInput(s) => CoreError::InvalidData(s),
        }
    }
}

// ---- Pixel-format / colour bridges ------------------------------------------

/// [`GifPixelFormat`] → the framework's [`PixelFormat`] (1:1 by name).
pub fn to_core_pixel_format(pf: GifPixelFormat) -> PixelFormat {
    match pf {
        GifPixelFormat::Pal8 => PixelFormat::Pal8,
        GifPixelFormat::Rgb24 => PixelFormat::Rgb24,
        GifPixelFormat::Rgba => PixelFormat::Rgba,
    }
}

/// The framework's native-layout names for the three GIF layouts
/// (the only [`PixelFormat`]s with a [`GifPixelFormat`] twin). Every
/// other declared input layout ([`ACCEPTED_PIXEL_FORMATS`]) is
/// normalised to one of these by [`GifImage::from_video_frame`].
fn from_core_pixel_format(pf: PixelFormat) -> crate::Result<GifPixelFormat> {
    Ok(match pf {
        PixelFormat::Pal8 => GifPixelFormat::Pal8,
        PixelFormat::Rgb24 => GifPixelFormat::Rgb24,
        PixelFormat::Rgba => GifPixelFormat::Rgba,
        other => return Err(unsupported_layout(other)),
    })
}

/// The typed refusal for an input layout the encoder does not declare.
fn unsupported_layout(pf: PixelFormat) -> GifError {
    GifError::unsupported(format!(
        "GIF encoder: pixel format {pf:?} is not an accepted input layout (accepted: {})",
        ACCEPTED_PIXEL_FORMATS
            .iter()
            .map(|f| format!("{f:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// The input layouts the registry encoder accepts
/// (`CodecCapabilities::accepted_pixel_formats` of the `gif` codec),
/// in the order the encoder prefers them: the native indexed layout,
/// then the 8-bit colour layouts with alpha, without alpha, grey,
/// 1-bit, then the deep layouts the encoder narrows to 8 bits. Every
/// one is normalised to `Pal8` / `Rgba` / `Rgb24` before Layer 1
/// [`crate::encode`] runs, so the colour-table reduction is the same
/// for all of them:
///
/// | Input | Normalised to | Conversion |
/// |---|---|---|
/// | `Pal8` | `Pal8` | none; palette + palette-alpha record as given |
/// | `Rgba`, `Bgra`, `Argb`, `Abgr`, `Gbrap8`, `Ya8` | `Rgba` | channel reorder / plane interleave / grey replicated to RGB; exact |
/// | `Rgb24`, `Bgr24`, `Gbrp8`, `Gray8` | `Rgb24` | channel reorder / plane interleave / grey replicated; exact |
/// | `MonoBlack`, `MonoWhite` | `Rgb24` | 1 bit per pixel, MSB first → `0` / `255` grey; exact |
/// | `Rgba64Le`, `Gbrap16Le`, `Ya16Le` | `Rgba` | the high byte of every 16-bit word (`v >> 8`), alpha included |
/// | `Rgb48Le`, `Gbrp16Le`, `Gray16Le` | `Rgb24` | the high byte of every 16-bit word (`v >> 8`) |
/// | `Gray12Le`, `Gray10Le` | `Rgb24` | the 12 / 10 significant bits narrowed to 8 (`v >> 4`, `v >> 2`) |
///
/// `Rgba` input then goes through the alpha threshold
/// ([`crate::quantize::ALPHA_OPAQUE_THRESHOLD`]) and `Rgba` / `Rgb24`
/// through the median-cut colour-table reduction when they hold more
/// than `max_colors` distinct colours; `Gray8` / mono / ≤ 256-colour
/// input is exact. Anything not listed — every YCbCr layout, CMYK,
/// float — is a typed `Unsupported` refusal from `send_frame`, and the
/// gateway's ladder converts to one of these first.
pub const ACCEPTED_PIXEL_FORMATS: &[PixelFormat] = &[
    PixelFormat::Pal8,
    PixelFormat::Rgba,
    PixelFormat::Bgra,
    PixelFormat::Argb,
    PixelFormat::Abgr,
    PixelFormat::Gbrap8,
    PixelFormat::Ya8,
    PixelFormat::Rgb24,
    PixelFormat::Bgr24,
    PixelFormat::Gbrp8,
    PixelFormat::Gray8,
    PixelFormat::MonoBlack,
    PixelFormat::MonoWhite,
    PixelFormat::Rgba64Le,
    PixelFormat::Gbrap16Le,
    PixelFormat::Ya16Le,
    PixelFormat::Rgb48Le,
    PixelFormat::Gbrp16Le,
    PixelFormat::Gray16Le,
    PixelFormat::Gray12Le,
    PixelFormat::Gray10Le,
];

/// The pixel plane(s) of `frame` as one tightly packed `Pal8` /
/// `Rgb24` / `Rgba` plane, per the [`ACCEPTED_PIXEL_FORMATS`] table.
/// Returns the GIF layout, the packed stride and the bytes.
fn normalise_planes(
    frame: &VideoFrame,
    pf: PixelFormat,
    width: u32,
    height: u32,
) -> crate::Result<(GifPixelFormat, usize, Vec<u8>)> {
    let (w, h) = (width as usize, height as usize);
    let planes = frame.image_planes();
    let count = pf.plane_count();
    if planes.len() < count {
        return Err(GifError::invalid(format!(
            "GIF encoder: {pf:?} needs {count} plane(s), the frame has {}",
            planes.len()
        )));
    }
    // Every declared layout stores full-resolution planes: validate
    // each against its own row length and height (a framework plane
    // may carry trailing bytes; it may not be short).
    let mut rows: Vec<&[u8]> = Vec::with_capacity(count);
    let mut strides = Vec::with_capacity(count);
    for (i, plane) in planes.iter().take(count).enumerate() {
        let row = pf
            .plane_row_bytes(i, width)
            .ok_or_else(|| GifError::invalid("GIF encoder: plane size overflow"))?;
        let need = plane
            .stride
            .checked_mul(h)
            .ok_or_else(|| GifError::invalid("GIF encoder: plane size overflow"))?;
        if plane.stride < row || plane.data.len() < need {
            return Err(GifError::invalid(format!(
                "GIF encoder: plane {i} holds {} bytes at stride {}, {pf:?} {width}x{height} \
                 needs rows of {row} bytes x {height}",
                plane.data.len(),
                plane.stride
            )));
        }
        rows.push(&plane.data[..need]);
        strides.push(plane.stride);
    }
    let p0 = rows[0];
    let s0 = strides[0];
    // Row `y` of plane 0.
    let row0 = |y: usize| &p0[y * s0..y * s0 + pf.plane_row_bytes(0, width).unwrap_or(0)];
    // Little-endian 16-bit word `i` of a row.
    let le16 = |r: &[u8], i: usize| u16::from_le_bytes([r[2 * i], r[2 * i + 1]]);

    Ok(match pf {
        PixelFormat::Pal8 => (GifPixelFormat::Pal8, s0, p0.to_vec()),
        PixelFormat::Rgb24 => (GifPixelFormat::Rgb24, s0, p0.to_vec()),
        PixelFormat::Rgba => (GifPixelFormat::Rgba, s0, p0.to_vec()),
        PixelFormat::Bgra | PixelFormat::Argb | PixelFormat::Abgr => {
            // Byte offsets of R, G, B, A within the 4-byte pixel.
            let [r, g, b, a] = match pf {
                PixelFormat::Bgra => [2, 1, 0, 3],
                PixelFormat::Argb => [1, 2, 3, 0],
                _ => [3, 2, 1, 0],
            };
            let mut out = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                for px in row0(y).chunks_exact(4) {
                    out.extend_from_slice(&[px[r], px[g], px[b], px[a]]);
                }
            }
            (GifPixelFormat::Rgba, w * 4, out)
        }
        PixelFormat::Bgr24 => {
            let mut out = Vec::with_capacity(w * h * 3);
            for y in 0..h {
                for px in row0(y).chunks_exact(3) {
                    out.extend_from_slice(&[px[2], px[1], px[0]]);
                }
            }
            (GifPixelFormat::Rgb24, w * 3, out)
        }
        PixelFormat::Gray8 => {
            let mut out = Vec::with_capacity(w * h * 3);
            for y in 0..h {
                for &v in row0(y) {
                    out.extend_from_slice(&[v, v, v]);
                }
            }
            (GifPixelFormat::Rgb24, w * 3, out)
        }
        PixelFormat::Ya8 => {
            let mut out = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                for px in row0(y).chunks_exact(2) {
                    out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
                }
            }
            (GifPixelFormat::Rgba, w * 4, out)
        }
        PixelFormat::MonoBlack | PixelFormat::MonoWhite => {
            // 1 bit per pixel, MSB first; `0` is black (MonoBlack) or
            // white (MonoWhite).
            let zero_is_black = pf == PixelFormat::MonoBlack;
            let mut out = Vec::with_capacity(w * h * 3);
            for y in 0..h {
                let r = row0(y);
                for x in 0..w {
                    let bit = (r[x / 8] >> (7 - (x % 8))) & 1;
                    let v = if (bit == 1) == zero_is_black { 255 } else { 0 };
                    out.extend_from_slice(&[v, v, v]);
                }
            }
            (GifPixelFormat::Rgb24, w * 3, out)
        }
        PixelFormat::Gbrp8 | PixelFormat::Gbrap8 => {
            let alpha = pf == PixelFormat::Gbrap8;
            let bpp = if alpha { 4 } else { 3 };
            let mut out = Vec::with_capacity(w * h * bpp);
            for y in 0..h {
                let at = |p: usize, x: usize| rows[p][y * strides[p] + x];
                for x in 0..w {
                    out.extend_from_slice(&[at(2, x), at(0, x), at(1, x)]);
                    if alpha {
                        out.push(at(3, x));
                    }
                }
            }
            (
                if alpha {
                    GifPixelFormat::Rgba
                } else {
                    GifPixelFormat::Rgb24
                },
                w * bpp,
                out,
            )
        }
        PixelFormat::Gbrp16Le | PixelFormat::Gbrap16Le => {
            let alpha = pf == PixelFormat::Gbrap16Le;
            let bpp = if alpha { 4 } else { 3 };
            let mut out = Vec::with_capacity(w * h * bpp);
            for y in 0..h {
                let at = |p: usize, x: usize| {
                    let r = &rows[p][y * strides[p]..];
                    (le16(r, x) >> 8) as u8
                };
                for x in 0..w {
                    out.extend_from_slice(&[at(2, x), at(0, x), at(1, x)]);
                    if alpha {
                        out.push(at(3, x));
                    }
                }
            }
            (
                if alpha {
                    GifPixelFormat::Rgba
                } else {
                    GifPixelFormat::Rgb24
                },
                w * bpp,
                out,
            )
        }
        PixelFormat::Rgb48Le | PixelFormat::Rgba64Le => {
            let ch = if pf == PixelFormat::Rgba64Le { 4 } else { 3 };
            let mut out = Vec::with_capacity(w * h * ch);
            for y in 0..h {
                let r = row0(y);
                for i in 0..w * ch {
                    out.push((le16(r, i) >> 8) as u8);
                }
            }
            (
                if ch == 4 {
                    GifPixelFormat::Rgba
                } else {
                    GifPixelFormat::Rgb24
                },
                w * ch,
                out,
            )
        }
        PixelFormat::Gray16Le | PixelFormat::Gray12Le | PixelFormat::Gray10Le => {
            let shift = match pf {
                PixelFormat::Gray16Le => 8,
                PixelFormat::Gray12Le => 4,
                _ => 2,
            };
            let mut out = Vec::with_capacity(w * h * 3);
            for y in 0..h {
                let r = row0(y);
                for x in 0..w {
                    let v = (le16(r, x) >> shift).min(255) as u8;
                    out.extend_from_slice(&[v, v, v]);
                }
            }
            (GifPixelFormat::Rgb24, w * 3, out)
        }
        PixelFormat::Ya16Le => {
            let mut out = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                let r = row0(y);
                for x in 0..w {
                    let v = (le16(r, 2 * x) >> 8) as u8;
                    let a = (le16(r, 2 * x + 1) >> 8) as u8;
                    out.extend_from_slice(&[v, v, v, a]);
                }
            }
            (GifPixelFormat::Rgba, w * 4, out)
        }
        other => return Err(unsupported_layout(other)),
    })
}

impl From<GifPixelFormat> for PixelFormat {
    fn from(pf: GifPixelFormat) -> Self {
        to_core_pixel_format(pf)
    }
}

impl TryFrom<PixelFormat> for GifPixelFormat {
    type Error = CoreError;
    fn try_from(pf: PixelFormat) -> CoreResult<Self> {
        Ok(from_core_pixel_format(pf)?)
    }
}

/// [`ColorInfo`] as the framework's [`ColorSignal`] (code points map
/// 1:1; `Unspecified` range stays unspecified).
pub fn to_color_signal(c: &ColorInfo) -> ColorSignal {
    let range = match c.range {
        ColorRange::Unspecified => oxideav_core::ColorRange::Unspecified,
        ColorRange::Limited => oxideav_core::ColorRange::Limited,
        ColorRange::Full => oxideav_core::ColorRange::Full,
    };
    ColorSignal::new(
        range,
        ColorPrimaries(c.primaries),
        TransferCharacteristics(c.transfer),
        MatrixCoefficients(c.matrix),
    )
}

/// The framework's [`ColorSignal`] as a [`ColorInfo`].
pub fn from_color_signal(s: &ColorSignal) -> ColorInfo {
    let range = match s.range {
        oxideav_core::ColorRange::Limited => ColorRange::Limited,
        oxideav_core::ColorRange::Full => ColorRange::Full,
        _ => ColorRange::Unspecified,
    };
    ColorInfo::new(range, s.primaries.0, s.transfer.0, s.matrix.0)
}

// ---- Frame bridge -----------------------------------------------------------

fn image_to_video_frame(image: &GifImage, pts: Option<i64>) -> VideoFrame {
    let mut frame = VideoFrame {
        pts,
        planes: vec![VideoPlane {
            stride: image.stride(),
            data: image.data().to_vec(),
        }],
    };
    stamp_frame_side_channels(&mut frame, image);
    frame
}

fn image_into_video_frame(mut image: GifImage, pts: Option<i64>) -> VideoFrame {
    let stride = image.stride();
    let data = if image.planes.is_empty() {
        Vec::new()
    } else {
        std::mem::take(&mut image.planes[0].data)
    };
    let mut frame = VideoFrame {
        pts,
        planes: vec![VideoPlane { stride, data }],
    };
    stamp_frame_side_channels(&mut frame, &image);
    frame
}

/// The palette side-channel for `Pal8` (RGB triplets) plus, when any
/// entry is not fully opaque, the palette-alpha record (one byte per
/// entry, in entry order: `0` at the §23.c.viii Transparency Index,
/// `255` everywhere else — see [`palette_alpha_record`]); and the
/// colour-signal side-channel whenever the image signals more than
/// GIF's default.
fn stamp_frame_side_channels(frame: &mut VideoFrame, image: &GifImage) {
    if let (GifPixelFormat::Pal8, Some(p)) = (image.format, &image.palette) {
        frame.set_palette(p.entries.iter().flat_map(|e| [e[0], e[1], e[2]]).collect());
        if let Some(alpha) = palette_alpha_record(p) {
            frame.set_palette_alpha(alpha);
        }
    }
    let c = image.color;
    if c.primaries != ColorInfo::UNSPECIFIED
        || c.transfer != ColorInfo::UNSPECIFIED
        || c.range == ColorRange::Limited
    {
        frame.set_color_signal(to_color_signal(&c));
    }
}

/// The palette-alpha side-channel record for `palette`: one byte per
/// entry (`entries[i][3]`), covering the whole table, or `None` when
/// every entry is opaque (an opaque palette attaches no record — the
/// framework reads a missing record as "all opaque").
///
/// This is the carriage the image-crate contract rules for indexed
/// layouts: a still with a Transparency Index stays `Pal8` and its
/// transparency rides this record (`0` at that index, `255` elsewhere)
/// instead of being dropped or forcing an `Rgba` expansion.
pub fn palette_alpha_record(palette: &Palette) -> Option<Vec<u8>> {
    palette
        .has_alpha()
        .then(|| palette.entries.iter().map(|e| e[3]).collect())
}

/// The [`Palette`] a `Pal8` [`VideoFrame`] carries: the palette
/// side-channel's RGB triplets joined with the palette-alpha record
/// (an entry the record does not cover — or every entry when no
/// record is attached — is opaque). `None` without a palette record.
pub fn palette_from_frame(frame: &VideoFrame) -> Option<Palette> {
    let rgb = frame.palette()?;
    let alpha = frame.palette_alpha().unwrap_or(&[]);
    Some(Palette::new(
        rgb.chunks_exact(3)
            .enumerate()
            .map(|(i, c)| [c[0], c[1], c[2], alpha.get(i).copied().unwrap_or(255)])
            .collect(),
    ))
}

impl From<GifImage> for VideoFrame {
    /// The pixel plane (`pts` `None`), plus the palette side-channel
    /// (and the palette-alpha record for a non-opaque table) for
    /// `Pal8` and the colour-signal side-channel when the image
    /// signals a colour space.
    fn from(image: GifImage) -> Self {
        image_into_video_frame(image, None)
    }
}

impl From<&GifImage> for VideoFrame {
    fn from(image: &GifImage) -> Self {
        image_to_video_frame(image, None)
    }
}

impl GifImage {
    /// Build a [`GifImage`] from a framework [`VideoFrame`]: dimensions
    /// and pixel format come from `params` (`Rgba` when unset); the
    /// frame's image planes are normalised to the GIF layout the
    /// [`ACCEPTED_PIXEL_FORMATS`] table gives for that format (`Pal8`,
    /// `Rgb24`, `Rgba` are taken as they are; the others are reordered,
    /// interleaved or narrowed to 8 bits as documented there — any
    /// other format is a typed `Unsupported` error); for `Pal8` the
    /// frame's palette side-channel joined with its palette-alpha
    /// record (see [`palette_from_frame`]; no record = every entry
    /// opaque) becomes [`GifImage::palette`]; the colour-signal
    /// side-channel, when attached, becomes [`GifImage::color`].
    pub fn from_video_frame(frame: &VideoFrame, params: &CodecParameters) -> crate::Result<Self> {
        let width = params
            .width
            .ok_or_else(|| GifError::invalid("GIF: missing width"))?;
        let height = params
            .height
            .ok_or_else(|| GifError::invalid("GIF: missing height"))?;
        let pf = params.pixel_format.unwrap_or(PixelFormat::Rgba);
        if !ACCEPTED_PIXEL_FORMATS.contains(&pf) {
            return Err(unsupported_layout(pf));
        }
        if frame.image_planes().is_empty() {
            return Err(GifError::invalid("GIF: frame has no planes"));
        }
        let (pix, stride, data) = normalise_planes(frame, pf, width, height)?;
        let mut img = GifImage::packed(width, height, pix, stride, data)?;
        if pix == GifPixelFormat::Pal8 {
            img.palette = palette_from_frame(frame);
        }
        if let Some(sig) = frame.color_signal() {
            img.color = from_color_signal(&sig);
        }
        Ok(img)
    }
}

impl TryFrom<(&VideoFrame, &CodecParameters)> for GifImage {
    type Error = GifError;
    fn try_from((frame, params): (&VideoFrame, &CodecParameters)) -> crate::Result<Self> {
        GifImage::from_video_frame(frame, params)
    }
}

// ---- Metadata blobs -----------------------------------------------------------

/// The custom [`BlobKind`] name for a §24 Comment Extension: one blob
/// per Comment Extension, holding its sub-block payload verbatim (raw
/// bytes — §24 says 7-bit ASCII, producers write anything). The
/// framework's well-known kinds cover the ICC / Exif / XMP Application
/// Extensions; comments have no well-known kind, so this crate names
/// the native structure as the contract asks (`"gif-comment"`).
pub const COMMENT_BLOB_KIND: &str = "gif-comment";

/// The [`BlobKind`] of a §24 Comment Extension blob
/// ([`COMMENT_BLOB_KIND`]).
pub fn comment_blob_kind() -> BlobKind {
    BlobKind::from_static(COMMENT_BLOB_KIND)
}

/// A `gif-comment` blob holding `comment` verbatim.
pub fn comment_blob(comment: impl Into<Vec<u8>>) -> MetadataBlob {
    MetadataBlob::new(comment_blob_kind(), comment)
}

/// The JPEG `APP1` framing some producers keep in front of the TIFF
/// structure inside a GIF `Exif` Application Extension. The framework's
/// `exif` payload convention starts at the byte-order mark, so the
/// prefix is removed on the way in and never written on the way out.
const EXIF_APP1_PREFIX: &[u8] = b"Exif\0\0";

/// The metadata blobs a parsed GIF carries, in source order of its
/// §26 Application Extensions: [`BlobKind::ICC`] from `ICCRGBG1012`
/// (the profile bytes verbatim), [`BlobKind::EXIF`] from `Exif` (the
/// TIFF structure; a leading `Exif\0\0` is stripped so the payload
/// starts at the byte-order mark), [`BlobKind::XMP`] from `XMP DataXMP`
/// (the packet bytes as the sub-blocks carry them), then one
/// [`comment_blob`] per §24 Comment Extension in source order.
///
/// Every other Application Extension (NETSCAPE2.0 / ANIMEXTS1.0 loop
/// control, vendor blocks) is structure, not metadata, and stays in
/// [`GifFile::blocks`]. The demuxer puts these on
/// [`CodecParameters::blobs`] (GIF metadata is file-level, so it is
/// stream-level in the framework; decoded frames carry no blob
/// record).
pub fn metadata_blobs(file: &GifFile) -> Vec<MetadataBlob> {
    let mut out = application_blobs(file);
    out.extend(file.comments().map(comment_blob));
    out
}

/// The ICC / Exif / XMP blobs of `file`'s Application Extensions, in
/// source order (see [`metadata_blobs`]).
pub(crate) fn application_blobs(file: &GifFile) -> Vec<MetadataBlob> {
    file.application_extensions()
        .filter_map(application_blob)
        .collect()
}

/// One Application Extension as a well-known blob, or `None` when it is
/// not one of the three metadata extensions.
fn application_blob(app: &Application) -> Option<MetadataBlob> {
    if &app.identifier == ICC_IDENTIFIER && &app.auth_code == ICC_AUTH_CODE {
        Some(MetadataBlob::new(BlobKind::ICC, app.data.clone()))
    } else if &app.identifier == XMP_IDENTIFIER && &app.auth_code == XMP_AUTH_CODE {
        Some(MetadataBlob::new(BlobKind::XMP, app.data.clone()))
    } else if &app.identifier == EXIF_IDENTIFIER {
        let payload = app.data.strip_prefix(EXIF_APP1_PREFIX).unwrap_or(&app.data);
        Some(MetadataBlob::new(BlobKind::EXIF, payload.to_vec()))
    } else {
        None
    }
}

/// `true` when `app` is one of the three metadata Application
/// Extensions ([`metadata_blobs`] lifts).
pub(crate) fn is_metadata_application(app: &Application) -> bool {
    application_blob(app).is_some()
}

/// What the registry encoder writes for a blob list: the
/// [`Metadata`] (first ICC / Exif / XMP blob of each kind — GIF carries
/// one extension of each) and one §24 Comment block per
/// `gif-comment` blob, in blob order. Kinds GIF has no carriage for
/// (IPTC, cover art, other custom kinds) are dropped.
pub fn blobs_to_gif(blobs: &[MetadataBlob]) -> (Metadata, Vec<Block>) {
    let mut meta = Metadata::new();
    let mut comments = Vec::new();
    for b in blobs {
        if b.kind == BlobKind::ICC {
            meta.icc.get_or_insert_with(|| b.data.clone());
        } else if b.kind == BlobKind::EXIF {
            meta.exif.get_or_insert_with(|| {
                b.data
                    .strip_prefix(EXIF_APP1_PREFIX)
                    .unwrap_or(&b.data)
                    .to_vec()
            });
        } else if b.kind == BlobKind::XMP {
            meta.xmp.get_or_insert_with(|| b.data.clone());
        } else if b.kind == COMMENT_BLOB_KIND {
            comments.push(Block::Comment(b.data.clone()));
        }
    }
    (meta, comments)
}

/// The blob list a picture is written with: `stream` blobs, except
/// that every kind present in `frame` replaces the stream blobs of
/// that kind (a frame blob overrides a stream blob of the same kind —
/// for `gif-comment`, the frame's comment set replaces the stream's).
pub fn effective_blobs(stream: &[MetadataBlob], frame: &[MetadataBlob]) -> Vec<MetadataBlob> {
    let mut out: Vec<MetadataBlob> = stream
        .iter()
        .filter(|s| !frame.iter().any(|f| f.kind == s.kind))
        .cloned()
        .collect();
    out.extend(frame.iter().cloned());
    out
}

// ---- CodecOptionsStruct (registry-only schema for EncodeOptions) ----------

impl CodecOptionsStruct for EncodeOptions {
    const SCHEMA: &'static [OptionField] = &[
        OptionField {
            name: "lzw_strategy",
            kind: OptionKind::Enum(&["deferred_clear", "clear_on_full"]),
            default: OptionValue::String(String::new()),
            help: "Appendix-F table-full strategy: `deferred_clear` (default; \
                   keep coding against the frozen 4096-entry table) or \
                   `clear_on_full` (emit a Clear code and rebuild).",
        },
        OptionField {
            name: "interlace",
            kind: OptionKind::Bool,
            default: OptionValue::Bool(false),
            help: "Store rows in the four-pass Appendix E interlace order \
                   (§20.c.vii Interlace Flag).",
        },
        OptionField {
            name: "max_colors",
            kind: OptionKind::U32,
            default: OptionValue::U32(256),
            help: "Colour budget for truecolour input (1..=256); the \
                   median-cut quantiser reduces to at most this many \
                   colour-table entries (one is reserved for transparency \
                   when the frame has transparent pixels).",
        },
        OptionField {
            name: "dither",
            kind: OptionKind::Enum(&[
                "none",
                "floyd_steinberg",
                "jarvis_judice_ninke",
                "stucki",
                "burkes",
                "sierra",
                "atkinson",
                "ordered_bayer8x8",
            ]),
            default: OptionValue::String(String::new()),
            help: "Index-plane assignment for truecolour input: `none` \
                   (default, nearest entry), an error-diffusion kernel, or \
                   `ordered_bayer8x8`.",
        },
        OptionField {
            name: "loop_count",
            kind: OptionKind::I32,
            default: OptionValue::I32(0),
            help: "NETSCAPE2.0 loop count for encode_animation: 0 loops \
                   forever, n repeats n times, -1 plays once (no extension).",
        },
        OptionField {
            name: "embed_metadata",
            kind: OptionKind::Bool,
            default: OptionValue::Bool(true),
            help: "Write the ICC / Exif / XMP Application Extensions and \
                   the §24 Comment blocks from the stream's and the frame's \
                   metadata blobs (`icc` / `exif` / `xmp` / `gif-comment`).",
        },
    ];
    fn apply(&mut self, key: &str, v: &OptionValue) -> CoreResult<()> {
        match key {
            "lzw_strategy" => {
                self.lzw_strategy = match v.as_str()? {
                    "" | "deferred_clear" => LzwStrategy::DeferredClear,
                    "clear_on_full" => LzwStrategy::ClearOnFull,
                    _ => unreachable!("guarded by SCHEMA"),
                }
            }
            "interlace" => self.interlace = v.as_bool()?,
            "max_colors" => {
                let n = v.as_u32()?;
                if !(1..=256).contains(&n) {
                    return Err(CoreError::invalid(format!(
                        "GIF encoder: option `max_colors` got {n}; expected 1..=256"
                    )));
                }
                self.quantize.max_colors = n as usize;
            }
            "dither" => {
                self.quantize.dither = match v.as_str()? {
                    "" | "none" => Dither::None,
                    "floyd_steinberg" => Dither::FloydSteinberg,
                    "jarvis_judice_ninke" => Dither::JarvisJudiceNinke,
                    "stucki" => Dither::Stucki,
                    "burkes" => Dither::Burkes,
                    "sierra" => Dither::Sierra,
                    "atkinson" => Dither::Atkinson,
                    "ordered_bayer8x8" => Dither::OrderedBayer8x8,
                    _ => unreachable!("guarded by SCHEMA"),
                }
            }
            "loop_count" => {
                let n = v.as_i32()?;
                self.loop_count = match n {
                    n if n < 0 => None,
                    n => Some(u16::try_from(n).map_err(|_| {
                        CoreError::invalid(format!(
                            "GIF encoder: option `loop_count` got {n}; expected -1..=65535"
                        ))
                    })?),
                };
            }
            "embed_metadata" => self.embed_metadata = v.as_bool()?,
            _ => unreachable!("guarded by SCHEMA"),
        }
        Ok(())
    }
}

// ---- Registration -----------------------------------------------------------

/// Register the GIF codec into the supplied [`CodecRegistry`].
pub fn register_codecs(reg: &mut CodecRegistry) {
    // Lossless for indexed / ≤ `max_colors`-colour input, lossy (the
    // median-cut reduction, the alpha threshold, the 16 → 8-bit
    // narrowing) for everything else — both flags are truthful.
    let caps = CodecCapabilities::video("gif_sw")
        .with_lossy(true)
        .with_lossless(true)
        .with_intra_only(true)
        .with_max_size(65535, 65535)
        .with_pixel_formats(ACCEPTED_PIXEL_FORMATS.to_vec());
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_STR))
            .capabilities(caps)
            .decoder(make_decoder)
            .encoder(make_encoder),
    );
}

/// Register the GIF container — demuxer, muxer, content probe and the
/// `.gif` extension — so the framework can open and write GIF files
/// through the registry (see [`crate::container`] for the stream layout
/// and packetisation).
pub fn register_containers(reg: &mut ContainerRegistry) {
    crate::container::register(reg);
}

/// Unified registration entry point — installs the GIF codec into the
/// codec sub-registry and the GIF container (demuxer / muxer / probe /
/// `.gif` extension) into the container sub-registry of the supplied
/// [`RuntimeContext`].
///
/// Wired into `oxideav_meta::register_all` via the
/// [`oxideav_core::register!`] macro below.
pub fn register(ctx: &mut RuntimeContext) {
    register_codecs(&mut ctx.codecs);
    register_containers(&mut ctx.containers);
}

oxideav_core::register!("gif", register);

/// Factory for the `Decoder` trait impl — registered in the codec
/// registry and called by the framework when a `gif` packet stream
/// needs decoding. `params.extradata` selects the packetisation (see
/// [`GifDecoder::from_params`]); `params.limits` tighten the decode
/// limits.
pub fn make_decoder(params: &CodecParameters) -> CoreResult<Box<dyn Decoder>> {
    Ok(Box::new(GifDecoder::from_params(params)))
}

/// Factory for the `Encoder` trait impl. `params.width` / `height`
/// are required at `send_frame`; `params.pixel_format` selects the
/// input layout (`Rgba` default; any of [`ACCEPTED_PIXEL_FORMATS`],
/// `Pal8` with the frame's palette side-channel); `params.options` is
/// parsed into [`EncodeOptions`] (see its `CodecOptionsStruct` schema).
/// The encoder's `output_params` carry the coded label: `pixel_format
/// = Pal8` (what the `gif` decoder emits for every packet this encoder
/// writes), not the input layout.
pub fn make_encoder(params: &CodecParameters) -> CoreResult<Box<dyn Encoder>> {
    let opts = parse_options::<EncodeOptions>(&params.options)?;
    Ok(Box::new(GifEncoder::with_options(params, opts)))
}

/// The stream `params` describe, relabelled with what the coded
/// packets decode to: `Pal8` (every packet the encoder writes is a
/// single full-canvas image with a ≤ 256-entry table, which
/// [`crate::decode`] returns indexed — with the palette-alpha record
/// when it has a Transparency Index).
fn coded_params(params: &CodecParameters) -> CodecParameters {
    let mut out = params.clone();
    out.pixel_format = Some(PixelFormat::Pal8);
    out
}

// ---- Decoder --------------------------------------------------------------

/// How the packets of a `gif` stream are cut — chosen from
/// `CodecParameters::extradata` by [`GifDecoder::from_params`].
enum Packetisation {
    /// Every packet is a complete `<GIF Data Stream>` decoded on its
    /// own: a still comes out native, an animation as every composited
    /// frame. The default (no [`crate::container`] record).
    WholeStream,
    /// The [`crate::container`] demuxer's animation layout: one
    /// single-frame stream per packet, composited across packets on a
    /// persistent canvas (`Rgba` out).
    AnimationPackets {
        compositor: Option<crate::compose::Compositor>,
    },
}

/// `Decoder` trait wrapper around the framework-free [`crate::decode`]
/// / [`crate::decode_all`] and the §23 compositor of
/// [`crate::compose()`].
///
/// Every input [`Packet`] is a complete `<GIF Data Stream>` (Header →
/// Logical Screen Descriptor → blocks → Trailer).
///
/// * A still image (exactly one graphic-rendering block) is emitted in
///   its native layout, exactly as [`crate::decode`] returns it: `Pal8`
///   with the palette side-channel (RGB) and — when the file has a
///   §23.c.viii Transparency Index, or the image leaves canvas
///   uncovered and the table has a free slot for a synthetic
///   transparent entry — the palette-alpha record (`0` at that entry,
///   `255` elsewhere; see [`palette_alpha_record`]); `Rgba` only when
///   the file's 256-entry opaque table leaves no room for the
///   transparent entry an uncovered canvas needs.
/// * An animation is emitted as [`crate::decode_all`] does: every
///   graphic-rendering block becomes one composited `Rgba`
///   [`VideoFrame`] (the §23 disposal-method state machine), because
///   disposal across frames with distinct colour tables has no indexed
///   representation. When the stream's `extradata` carries the
///   [`crate::container`] animation record, each packet holds one frame
///   and the canvas persists across packets; otherwise the whole
///   animation is one packet.
///
/// `pts` is the packet's `pts` when it has one (the demuxer's
/// cumulative delay), else the frame ordinal within the packet.
///
/// Drained, `receive_frame` returns [`CoreError::NeedMore`] until
/// [`Decoder::flush`] has been called, then [`CoreError::Eof`].
pub struct GifDecoder {
    codec_id: CodecId,
    queued: std::collections::VecDeque<CoreFrame>,
    opts: crate::DecodeOptions,
    mode: Packetisation,
    eof: bool,
}

impl GifDecoder {
    /// A whole-stream decoder with default [`crate::DecodeOptions`]
    /// (every packet a complete file).
    pub fn new() -> Self {
        Self {
            codec_id: CodecId::new(CODEC_ID_STR),
            queued: std::collections::VecDeque::new(),
            opts: crate::DecodeOptions::default(),
            mode: Packetisation::WholeStream,
            eof: false,
        }
    }

    /// A decoder for the stream `params` describe: the
    /// [`crate::container`] `extradata` record selects whole-stream or
    /// per-frame animation packets, and `params.limits`
    /// (`max_pixels_per_frame`, `max_alloc_bytes_per_frame`) tighten the
    /// [`crate::DecodeOptions`] limits (never loosen them).
    pub fn from_params(params: &CodecParameters) -> Self {
        let mut dec = Self::new();
        let limits = &params.limits;
        dec.opts.max_pixels = Some(
            dec.opts
                .max_pixels
                .map_or(limits.max_pixels_per_frame, |m| {
                    m.min(limits.max_pixels_per_frame)
                }),
        );
        dec.opts.max_bytes = Some(
            dec.opts
                .max_bytes
                .map_or(limits.max_alloc_bytes_per_frame, |m| {
                    m.min(limits.max_alloc_bytes_per_frame)
                }),
        );
        if crate::container::is_animation_stream(params) {
            dec.mode = Packetisation::AnimationPackets { compositor: None };
        }
        dec
    }

    /// Whole-stream path: one packet = one complete file.
    fn decode_whole_stream(&mut self, packet: &Packet) -> CoreResult<()> {
        let info = crate::info(&packet.data)?;
        if info.frames == 1 && info.image_count == 1 {
            // Still image: the native layout (`Pal8` + palette), as
            // `decode` returns it.
            let image = crate::decode_with(&packet.data, &self.opts)?;
            self.queued
                .push_back(CoreFrame::Video(image_into_video_frame(
                    image,
                    packet.pts.or(Some(0)),
                )));
            return Ok(());
        }
        let frames = crate::decode_all_with(&packet.data, &self.opts)?;
        for (idx, frame) in frames.into_iter().enumerate() {
            let pts = match packet.pts {
                Some(p) if idx == 0 => Some(p),
                Some(_) => None,
                None => Some(idx as i64),
            };
            self.queued
                .push_back(CoreFrame::Video(image_into_video_frame(frame.image, pts)));
        }
        Ok(())
    }

    /// Animation-packets path: composite this packet's graphic block(s)
    /// onto the persistent canvas.
    fn decode_animation_packet(&mut self, packet: &Packet) -> CoreResult<()> {
        let file = crate::parse_with(&packet.data, &self.opts)?;
        let (w, h) = (file.screen_width, file.screen_height);
        let Packetisation::AnimationPackets { compositor } = &mut self.mode else {
            unreachable!("decode_animation_packet is only called in animation mode");
        };
        if compositor.is_none() {
            let bytes = u64::from(w) * u64::from(h) * 4;
            self.opts.check_screen(u32::from(w), u32::from(h), bytes)?;
            *compositor = Some(crate::compose::Compositor::new(w, h));
        }
        let comp = compositor.as_mut().expect("compositor was just created");
        if (comp.width(), comp.height()) != (w, h) {
            return Err(CoreError::invalid(format!(
                "gif: packet is a {w}×{h} logical screen, the stream's canvas is {}×{}",
                comp.width(),
                comp.height()
            )));
        }
        for block in &file.blocks {
            if let Some(composed) = comp.step(&file, block)? {
                let frame = VideoFrame {
                    pts: packet.pts,
                    planes: vec![VideoPlane {
                        stride: usize::from(w) * 4,
                        data: composed.canvas.pixels,
                    }],
                };
                self.queued.push_back(CoreFrame::Video(frame));
            }
        }
        Ok(())
    }
}

impl Default for GifDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for GifDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> CoreResult<()> {
        match self.mode {
            Packetisation::WholeStream => self.decode_whole_stream(packet),
            Packetisation::AnimationPackets { .. } => self.decode_animation_packet(packet),
        }
    }

    fn receive_frame(&mut self) -> CoreResult<CoreFrame> {
        match self.queued.pop_front() {
            Some(f) => Ok(f),
            None if self.eof => Err(CoreError::Eof),
            None => Err(CoreError::NeedMore),
        }
    }

    fn flush(&mut self) -> CoreResult<()> {
        self.eof = true;
        Ok(())
    }

    fn reset(&mut self) -> CoreResult<()> {
        self.queued.clear();
        self.eof = false;
        if let Packetisation::AnimationPackets { compositor } = &mut self.mode {
            *compositor = None;
        }
        Ok(())
    }
}

// ---- Encoder --------------------------------------------------------------

/// `Encoder` trait wrapper around the framework-free [`crate::encode`]:
/// accepts one [`VideoFrame`] per `send_frame` (layout per
/// `params.pixel_format`) and emits a single-image GIF byte stream per
/// `receive_packet` (`pts` / `dts` = the frame's `pts`). Drained,
/// `receive_packet` returns [`CoreError::NeedMore`] until
/// [`Encoder::flush`] has been called, then [`CoreError::Eof`]. The
/// [`crate::container`] muxer merges several such packets into one
/// animated file.
///
/// Truecolour frames are reduced to a colour table by the deterministic
/// median-cut quantiser ([`crate::quantize`]); an opaque frame using
/// ≤ 256 distinct colours keeps its exact colours. GIF has no per-pixel
/// alpha — pixels with alpha below
/// [`crate::quantize::ALPHA_OPAQUE_THRESHOLD`] route to the one
/// §23.c.viii Transparency Index. `Pal8` frames are written with their
/// palette side-channel as the Global Color Table; their palette-alpha
/// record, when attached, selects the Transparency Index by the same
/// rule — the first entry whose alpha is below the threshold becomes
/// the Transparency Index and every pixel at another such entry is
/// folded onto it (GIF has one transparent index per image); entries
/// at or above the threshold are written opaque. No record = every
/// entry opaque, no Graphic Control Extension.
pub struct GifEncoder {
    /// The stream as the caller described it: the input layout the
    /// frames arrive in, the options, the stream-level blobs.
    input_params: CodecParameters,
    /// `input_params` with the coded label (`Pal8`).
    output_params: CodecParameters,
    opts: EncodeOptions,
    pending: std::collections::VecDeque<Packet>,
    flushed: bool,
}

impl GifEncoder {
    /// Build an encoder from codec parameters (options parsed per the
    /// [`EncodeOptions`] schema; invalid options fall back to defaults —
    /// use [`make_encoder`] to surface them as errors).
    pub fn new_from_params(params: &CodecParameters) -> Self {
        Self::with_options(
            params,
            parse_options::<EncodeOptions>(&params.options).unwrap_or_default(),
        )
    }

    fn with_options(params: &CodecParameters, opts: EncodeOptions) -> Self {
        Self {
            input_params: params.clone(),
            output_params: coded_params(params),
            opts,
            pending: std::collections::VecDeque::new(),
            flushed: false,
        }
    }
}

impl Encoder for GifEncoder {
    fn codec_id(&self) -> &CodecId {
        &self.output_params.codec_id
    }

    fn output_params(&self) -> &CodecParameters {
        &self.output_params
    }

    fn send_frame(&mut self, frame: &CoreFrame) -> CoreResult<()> {
        let video = match frame {
            CoreFrame::Video(v) => v,
            _ => return Err(CoreError::invalid("gif encoder: expected Frame::Video")),
        };
        let mut image = GifImage::from_video_frame(video, &self.input_params)?;
        if image.width == 0 || image.height == 0 {
            return Err(CoreError::invalid(
                "gif encoder: width/height must be in 1..=65535",
            ));
        }
        // Metadata: the stream's blobs, overridden per kind by the
        // frame's record, as the ICC / Exif / XMP Application
        // Extensions and §24 Comment blocks of this packet (every
        // packet is a complete file, so every packet carries them; the
        // muxer keeps one copy when it merges).
        let comments = if self.opts.embed_metadata {
            let blobs = effective_blobs(&self.input_params.blobs, &video.blobs());
            let (meta, comments) = blobs_to_gif(&blobs);
            image.metadata = meta;
            comments
        } else {
            Vec::new()
        };
        let file = crate::api::file_from_image_with_blocks(&image, &self.opts, comments)?;
        let bytes = crate::encode_file_with(&file, &self.opts)?;
        let mut pkt = Packet::new(0u32, TimeBase::new(1, 1), bytes);
        pkt.pts = video.pts;
        pkt.dts = video.pts;
        pkt.flags.keyframe = true;
        self.pending.push_back(pkt);
        Ok(())
    }

    fn receive_packet(&mut self) -> CoreResult<Packet> {
        match self.pending.pop_front() {
            Some(p) => Ok(p),
            None if self.flushed => Err(CoreError::Eof),
            None => Err(CoreError::NeedMore),
        }
    }

    fn flush(&mut self) -> CoreResult<()> {
        self.flushed = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_rgba(w: usize, h: usize, r: u8, g: u8, b: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity(w * h * 4);
        for _ in 0..(w * h) {
            v.extend_from_slice(&[r, g, b, 255]);
        }
        v
    }

    #[test]
    fn register_via_runtime_context_installs_codec_factory() {
        let mut ctx = RuntimeContext::new();
        register(&mut ctx);
        let id = CodecId::new(CODEC_ID_STR);
        assert!(
            ctx.codecs.has_decoder(&id),
            "GIF decoder factory not installed via RuntimeContext"
        );
        assert!(
            ctx.codecs.has_encoder(&id),
            "GIF encoder factory not installed via RuntimeContext"
        );
        assert_eq!(
            ctx.containers.container_for_extension("gif"),
            Some("gif"),
            "GIF container extension not installed via RuntimeContext"
        );
    }

    #[test]
    fn entry_point_dispatches_into_register() {
        // The register! macro should expose __oxideav_entry on the
        // module path; meta's register_all calls the crate-root
        // re-export. Confirm both paths work.
        let mut ctx = RuntimeContext::new();
        super::__oxideav_entry(&mut ctx);
        let id = CodecId::new(CODEC_ID_STR);
        assert!(ctx.codecs.has_decoder(&id));
    }

    #[test]
    fn encode_then_decode_roundtrip_solid_frame() {
        let w = 8usize;
        let h = 4usize;
        let rgba = solid_rgba(w, h, 0x10, 0x80, 0xC0);
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(w as u32);
        params.height = Some(h as u32);
        params.pixel_format = Some(PixelFormat::Rgba);

        let mut enc = make_encoder(&params).unwrap();
        let frame_in = CoreFrame::Video(VideoFrame {
            pts: Some(0),
            planes: vec![VideoPlane {
                stride: w * 4,
                data: rgba.clone(),
            }],
        });
        enc.send_frame(&frame_in).unwrap();
        let pkt = enc.receive_packet().unwrap();
        assert!(pkt.data.starts_with(b"GIF"));

        let mut dec = make_decoder(&params).unwrap();
        dec.send_packet(&pkt).unwrap();
        let frame_out = dec.receive_frame().unwrap();
        let CoreFrame::Video(v) = frame_out else {
            panic!("decoder returned non-video frame");
        };
        // A still GIF comes back in its native layout: one `Pal8`
        // plane plus the palette side-channel (RGB), never a
        // pre-expanded `Rgba` canvas.
        assert_eq!(v.image_plane_count(), 1);
        assert_eq!(v.planes[0].stride, w);
        assert_eq!(v.planes[0].data.len(), w * h);
        let pal = v.palette().expect("Pal8 frame carries its palette");
        assert_eq!(pal.len() % 3, 0);
        // Solid colour input → every index points at the one RGB
        // triplet (opaque input, no transparent index requested).
        for &i in &v.planes[0].data {
            let i = usize::from(i) * 3;
            assert_eq!(&pal[i..i + 3], [0x10, 0x80, 0xC0]);
        }
        // Expanding through the standalone type gives the input back.
        let img = crate::GifImage::from_video_frame(&v, &params_for(&v, w, h)).unwrap();
        assert_eq!(img.format, crate::PixelFormat::Pal8);
        assert_eq!(img.to_rgba8(), rgba);
    }

    fn params_for(v: &VideoFrame, w: usize, h: usize) -> CodecParameters {
        let mut p = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        p.width = Some(w as u32);
        p.height = Some(h as u32);
        p.pixel_format = Some(if v.planes[0].stride == w {
            PixelFormat::Pal8
        } else {
            PixelFormat::Rgba
        });
        p
    }

    #[test]
    fn decoder_emits_native_pal8_for_stills_and_composited_rgba_for_animations() {
        use crate::{decode_all, encode_all, EncodeOptions, Frame, GifImage};
        // Still: a 2-colour 4×2 Pal8 image.
        let still = GifImage::from_rgb8(
            4,
            2,
            vec![
                1, 2, 3, 1, 2, 3, 9, 8, 7, 9, 8, 7, 9, 8, 7, 1, 2, 3, 1, 2, 3, 9, 8, 7,
            ],
        )
        .unwrap();
        let bytes = crate::encode(&still, &EncodeOptions::default()).unwrap();
        let params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        let mut dec = make_decoder(&params).unwrap();
        dec.send_packet(&Packet::new(0u32, TimeBase::new(1, 1), bytes))
            .unwrap();
        let CoreFrame::Video(v) = dec.receive_frame().unwrap() else {
            panic!("non-video frame");
        };
        assert_eq!(v.planes[0].stride, 4, "still GIF is emitted as Pal8");
        assert_eq!(v.planes[0].data.len(), 8);
        assert!(v.palette().is_some(), "palette side-channel is stamped");
        assert!(dec.receive_frame().is_err(), "exactly one frame");

        // Animation: two full-canvas frames → two composited Rgba frames.
        let a = GifImage::from_rgba8(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        let b = GifImage::from_rgba8(2, 1, vec![0, 255, 0, 255, 0, 0, 255, 255]).unwrap();
        let frames = [Frame::new(a, None), Frame::new(b, None)];
        let anim = encode_all(&frames, &EncodeOptions::default()).unwrap();
        let expect = decode_all(&anim).unwrap();
        let mut dec = make_decoder(&params).unwrap();
        dec.send_packet(&Packet::new(0u32, TimeBase::new(1, 1), anim))
            .unwrap();
        for (idx, want) in expect.iter().enumerate() {
            let CoreFrame::Video(v) = dec.receive_frame().unwrap() else {
                panic!("non-video frame");
            };
            assert_eq!(v.pts, Some(idx as i64));
            assert_eq!(v.planes[0].stride, 2 * 4, "animation frames are Rgba");
            assert_eq!(v.planes[0].data, want.image.as_bytes().unwrap());
            assert!(v.palette().is_none());
        }
        assert!(dec.receive_frame().is_err());
    }

    #[test]
    fn encoder_quantises_more_than_256_colours() {
        // 17×16 = 272 pixels, every one a unique RGB triplet → > 256
        // colours. The encoder used to reject this; it now reduces it
        // to a conformant ≤256-entry §19 palette via median cut and
        // produces a decodable stream.
        let w = 17usize;
        let h = 16usize;
        let mut rgba = Vec::with_capacity(w * h * 4);
        for i in 0..(w * h) {
            rgba.extend_from_slice(&[
                (i & 0xFF) as u8,
                ((i >> 4) & 0xFF) as u8,
                ((i >> 8) & 0xFF) as u8,
                255,
            ]);
        }
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(w as u32);
        params.height = Some(h as u32);
        params.pixel_format = Some(PixelFormat::Rgba);
        let mut enc = make_encoder(&params).unwrap();
        let frame = CoreFrame::Video(VideoFrame {
            pts: Some(0),
            planes: vec![VideoPlane {
                stride: w * 4,
                data: rgba,
            }],
        });
        enc.send_frame(&frame).unwrap();
        let pkt = enc.receive_packet().unwrap();
        assert!(pkt.data.starts_with(b"GIF"));
        // The produced stream decodes, and its §19 palette is within
        // the 256-entry limit.
        let decoded = crate::parse(&pkt.data).unwrap();
        let pal = decoded.global_palette.as_ref().unwrap();
        assert!(pal.len() <= 256);
        assert_eq!(decoded.screen_width as usize, w);
        assert_eq!(decoded.screen_height as usize, h);
    }

    #[test]
    fn encoder_marks_transparency_with_gce() {
        // 2×1: one opaque red pixel + one fully-transparent pixel.
        let w = 2usize;
        let h = 1usize;
        let rgba = vec![255, 0, 0, 255, 9, 9, 9, 0];
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(w as u32);
        params.height = Some(h as u32);
        params.pixel_format = Some(PixelFormat::Rgba);
        let mut enc = make_encoder(&params).unwrap();
        let frame = CoreFrame::Video(VideoFrame {
            pts: Some(0),
            planes: vec![VideoPlane {
                stride: w * 4,
                data: rgba,
            }],
        });
        enc.send_frame(&frame).unwrap();
        let pkt = enc.receive_packet().unwrap();
        let decoded = crate::parse(&pkt.data).unwrap();
        // The frame carries a §23 GCE with a §23.c.viii Transparency
        // Index, and the transparent pixel decodes to alpha 0 through
        // the compositor.
        assert!(decoded.has_transparency());
        let composed = crate::compose(&decoded).unwrap();
        let canvas = &composed[0].canvas;
        // Pixel 1 (the transparent one) composites to fully transparent.
        assert_eq!(canvas.pixels[7], 0, "transparent pixel alpha");
        // Pixel 0 (opaque red) stays opaque.
        assert_eq!(canvas.pixels[3], 0xFF, "opaque pixel alpha");
    }

    #[test]
    fn frame_bridge_round_trips_pal8_and_rgba() {
        let pal = Palette::new(vec![[9, 8, 7, 255], [1, 2, 3, 255]]);
        let img = GifImage::from_indexed(2, 1, vec![0, 1], pal).unwrap();
        let frame: VideoFrame = (&img).into();
        assert_eq!(frame.image_planes().len(), 1);
        assert_eq!(frame.palette(), Some(&[9u8, 8, 7, 1, 2, 3][..]));
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(2);
        params.height = Some(1);
        params.pixel_format = Some(PixelFormat::Pal8);
        let back = GifImage::try_from((&frame, &params)).unwrap();
        assert_eq!(back, img);

        let rgba = GifImage::from_rgba8(1, 1, vec![1, 2, 3, 4]).unwrap();
        let frame = VideoFrame::from(rgba.clone());
        params.width = Some(1);
        params.pixel_format = Some(PixelFormat::Rgba);
        assert_eq!(GifImage::from_video_frame(&frame, &params).unwrap(), rgba);
        params.pixel_format = Some(PixelFormat::Yuv420P);
        assert!(GifImage::from_video_frame(&frame, &params).is_err());
        assert_eq!(
            to_core_pixel_format(GifPixelFormat::Pal8),
            PixelFormat::Pal8
        );
        assert!(GifPixelFormat::try_from(PixelFormat::Gray8).is_err());
    }

    #[test]
    fn encoder_accepts_pal8_frames_with_palette_side_channel() {
        let pal = Palette::new(vec![[9, 8, 7, 255], [1, 2, 3, 255]]);
        let img = GifImage::from_indexed(2, 1, vec![0, 1], pal).unwrap();
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(2);
        params.height = Some(1);
        params.pixel_format = Some(PixelFormat::Pal8);
        let mut enc = make_encoder(&params).unwrap();
        enc.send_frame(&CoreFrame::Video(VideoFrame::from(&img)))
            .unwrap();
        let pkt = enc.receive_packet().unwrap();
        let back = crate::decode(&pkt.data).unwrap();
        assert_eq!(back, img);
    }

    #[test]
    fn encoder_options_schema_applies() {
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(1);
        params.height = Some(1);
        params.options.insert("interlace", "true");
        params.options.insert("max_colors", "16");
        params.options.insert("dither", "floyd_steinberg");
        params.options.insert("lzw_strategy", "clear_on_full");
        params.options.insert("loop_count", "-1");
        params.options.insert("embed_metadata", "false");
        let opts = parse_options::<EncodeOptions>(&params.options).unwrap();
        assert!(opts.interlace);
        assert_eq!(opts.quantize.max_colors, 16);
        assert_eq!(opts.quantize.dither, Dither::FloydSteinberg);
        assert_eq!(opts.lzw_strategy, LzwStrategy::ClearOnFull);
        assert_eq!(opts.loop_count, None);
        assert!(!opts.embed_metadata);
        params.options.insert("max_colors", "0");
        assert!(make_encoder(&params).is_err());
        params.options.insert("max_colors", "2");
        params.options.insert("dither", "bogus");
        assert!(make_encoder(&params).is_err());
    }
}
