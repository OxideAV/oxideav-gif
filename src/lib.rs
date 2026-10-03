//! Pure-Rust GIF87a / GIF89a decoder and encoder.
//!
//! # Standalone use
//!
//! The crate follows the OxideAV image-crate contract
//! (`IMAGE_CRATE_API`): a small root vocabulary that works with
//! `default-features = false` and returns pixels as plain `Vec<u8>`.
//!
//! ```no_run
//! # fn main() -> Result<(), oxideav_gif::Error> {
//! let bytes = std::fs::read("in.gif")?;
//! if oxideav_gif::probe(&bytes) {
//!     let info = oxideav_gif::info(&bytes)?;   // header only: width, height, format, frames
//!     let img = oxideav_gif::decode(&bytes)?;  // GifImage: first frame on the logical screen, Pal8
//!     let rgba: Vec<u8> = img.to_rgba8();      // tightly packed RGBA, 4 * width bytes per row
//!     let (w, h) = (img.width(), img.height());
//!     assert_eq!(info.width, w);
//!
//!     let opts = oxideav_gif::EncodeOptions::default().with_max_colors(64);
//!     let out: Vec<u8> = oxideav_gif::encode_rgba8(w, h, &rgba, &opts)?;
//!     std::fs::write("out.gif", out)?;
//! }
//! # Ok(()) }
//! ```
//!
//! * [`probe`] / [`info`] — header sniff; structural walk without
//!   expanding any LZW raster ([`ImageInfo`]: screen size, layout,
//!   frame count, alpha, metadata presence, version, loop count).
//! * [`decode`] / [`decode_with`] — the first §20 image composed onto
//!   the §18 Logical Screen in its native layout ([`GifImage`]:
//!   `width`, `height`, [`PixelFormat`], one [`Plane`], [`ColorInfo`],
//!   [`Metadata`], [`Palette`]), with [`DecodeOptions`] for limits,
//!   strictness and recovery.
//! * [`decode_rgb8`] / [`decode_rgba8`] — the one-call raw paths
//!   ([`RgbImage`] / [`RgbaImage`]); [`GifImage::to_rgb8`] /
//!   [`GifImage::to_rgba8`] do the same from a decoded image.
//! * [`decode_all`] — every graphic-rendering block composited per the
//!   §23 disposal rules ([`Frame`]: `Rgba` canvas, delay, disposal);
//!   [`decode_from`] reads a `Read` to its end.
//! * [`encode`] / [`encode_rgb8`] / [`encode_rgba8`] / [`encode_to`] /
//!   [`encode_animation`] with [`EncodeOptions`] (LZW strategy,
//!   interlace, quantiser, loop count, metadata).
//! * [`GifError`] (alias [`Error`]): `InvalidData`, `Unsupported`,
//!   `LimitExceeded`, `Io`, `UnexpectedEof`, `InvalidInput`.
//!
//! # Framework use
//!
//! With the default-on `registry` feature, [`register`] installs the
//! `gif` codec and the `.gif` extension hint into an
//! `oxideav_core::RuntimeContext`; [`make_decoder`] / [`make_encoder`]
//! are the factories, and `From<GifImage> for VideoFrame` /
//! [`GifImage::from_video_frame`] convert between the two worlds. The
//! trait-side [`GifDecoder`] / [`GifEncoder`] call the standalone
//! functions ([`decode_all`] and [`encode`]).
//!
//! # Supported layouts
//!
//! Decode:
//!
//! | Source | Layout |
//! |---|---|
//! | first §20 image, composed onto the Logical Screen, when one colour table describes the canvas | `Pal8` + [`Palette`] (RGBA entries; the §23.c.viii Transparency Index at alpha 0; a synthetic `[0,0,0,0]` entry appended when the frame leaves screen pixels uncovered and has no transparent index) |
//! | first image whose table has 256 opaque entries and still leaves transparent pixels | `Rgba` |
//! | every [`decode_all`] frame | `Rgba` (the composited canvas) |
//!
//! Every §20 image: Global or Local Color Table (2–256 entries),
//! Appendix E interlace, Appendix F LZW (both table-full strategies),
//! §23 Graphic Control (disposal 0–3, transparency, delay, user
//! input), §25 Plain Text rendered with the crate-local 8×8 font.
//!
//! Encode: `Pal8` as given (one Transparency Index, see [`encode`]);
//! `Rgb24` / `Rgba` reduced to ≤ 256 colours by the deterministic
//! median-cut quantiser of [`quantize`] (alpha below
//! [`quantize::ALPHA_OPAQUE_THRESHOLD`] → transparent), as
//! [`encode_rgb8`] / [`encode_rgba8`] do — GIF has no truecolour
//! layout, so this conversion is documented rather than refused.
//! [`Error::Unsupported`] for dimensions above 65 535.
//!
//! # Options
//!
//! [`DecodeOptions`]: `max_width` / `max_height` / `max_pixels` /
//! `max_bytes` (checked against the Logical Screen Descriptor and every
//! Image Descriptor before any raster is expanded; default 1 GiB of
//! decoded data), `strict` (the §7–§26 conformance walk of
//! [`GifFile::validate_strict`]) and `lenient` (the recovery parser of
//! [`parse_lenient`]). [`EncodeOptions`]: `lzw_strategy`
//! ([`LzwStrategy`]), `interlace`, `quantize`
//! ([`quantize::QuantizeOptions`]: colour budget, dither, box priority,
//! Lloyd refinement), `loop_count`, `embed_metadata`.
//!
//! # Metadata and colour
//!
//! [`GifImage::metadata`] carries the ICC profile (`ICCRGBG1012`), Exif
//! (`Exif`) and XMP (`XMP DataXMP`) §26 Application Extensions; GIF has
//! no gamma field. GIF carries no colour signalling, so
//! [`GifImage::color`] is always [`ColorInfo::gif_default`] (full-range
//! RGB, unspecified primaries / transfer); an ICC profile, when present,
//! governs. [`encode`] writes the three extensions back
//! ([`EncodeOptions::embed_metadata`]). `decode(encode(img)) == img` for
//! every `Pal8` image the decoder produces (planes, palette, metadata).
//!
//! # Limits
//!
//! Every function returns [`GifError`] on hostile input, never panics
//! (the fuzz targets cover `probe` / `info` / `decode` / `decode_all`
//! and the depth API). Beyond [`DecodeOptions`], the LZW decoder is
//! bounded by the raster size the Image Descriptor implies, and
//! [`decode_all`] bounds `frames × canvas` by `max_bytes`.
//!
//! # GIF specifics
//!
//! The contract is a floor. The full GIF Data Stream model is
//! [`GifFile`] — every block in source order ([`Block`]: §20 images as
//! [`GifFrameData`] with their raw §22 index rasters, §24 comments,
//! §25 Plain Text, §26 Application Extensions), both colour tables,
//! the §18 Logical Screen fields — obtained with [`parse`] /
//! [`parse_with`] / [`parse_first_frame`] / [`parse_lenient`] and
//! serialised with [`encode_file`] / [`encode_file_with`] (byte-stable
//! round trip). On top of it:
//!
//! * [`compose()`] / [`Playback`] — the §23 disposal-method state machine
//!   as eager canvases or a lazy, NETSCAPE2.0-loop-aware iterator;
//!   [`compose_frame_at_global`] seeks by wall-clock offset.
//! * [`AnimationBuilder`], [`GifFile::from_rgba_frame`],
//!   [`GifFile::from_rgba_frames`],
//!   [`GifFile::from_rgba_frames_shared_palette`] — authoring from
//!   truecolour frames; [`GifFile::optimize_color_tables`] /
//!   [`GifFile::optimize_frame_rects`] — stream optimisation.
//! * [`quantize`] — median cut with extent / population box priority,
//!   Floyd–Steinberg / Jarvis–Judice–Ninke / Stucki / Atkinson /
//!   Sierra / ordered-Bayer dithering, Lloyd refinement, fixed-palette
//!   remap, shared animation palettes.
//! * [`app_ext`] — typed NETSCAPE2.0 / ANIMEXTS1.0 looping, XMP, ICC,
//!   Exif views over the raw [`Application`] blocks.
//! * [`GifFile::conformance_report`] / [`GifFile::validate_strict`] —
//!   the non-fatal §7–§26 conformance walk ([`ConformanceIssue`]).
//! * [`lzw`] — the Appendix F codec on its own.
//!
//! ## §7 Required Version enforcement on encode
//!
//! Per-block "Required Version" entries are 87a for §20 / §21 / §22 and
//! 89a for §23–§26. [`encode_file`] refuses a [`GifFile`] declared
//! [`Version::Gif87a`] that contains an 89a-only block with
//! [`Error::InvalidInput`]; [`GifFile::required_version`] and
//! [`GifFile::upgrade_version_if_needed`] are the recovery helpers.
//!
//! ## Plain Text rendering
//!
//! §25.e leaves the font to the decoder; [`compose()`] / [`Playback`] /
//! [`decode_all`] render Plain Text against the crate-local clean-room
//! 8×8 monospace bitmap font ([`font`]), honouring the §23.c.viii
//! transparent index for the background colour.
//!
//! [`Application`]: crate::Application

mod api;
pub mod app_ext;
pub mod builder;
pub mod compose;
pub mod conformance;
pub mod decoder;
pub mod encoder;
pub mod error;
pub mod font;
pub mod image;
pub mod interlace;
pub mod lzw;
pub mod options;
pub mod playback;
pub mod quantize;
#[cfg(feature = "registry")]
pub mod registry;
pub mod types;

// ---- The image-crate contract (IMAGE_CRATE_API) ---------------------------
// Root vocabulary, identical across every oxideav image crate; works
// with `default-features = false`.
pub use api::{
    decode, decode_all, decode_all_with, decode_from, decode_rgb8, decode_rgba8, decode_with,
    encode, encode_animation, encode_rgb8, encode_rgba8, encode_to, info, probe,
};
pub use error::{Error, GifError, Result};
pub use options::DecodeOptions;
pub use types::{
    ColorInfo, ColorRange, Frame, GifImage, GifPixelFormat, ImageInfo, Metadata, Palette,
    PixelFormat, Plane, RgbImage, RgbaImage,
};

// ---- GIF-specific depth (the contract is a floor, not a ceiling) ----------
pub use app_ext::ApplicationKind;
pub use builder::AnimationBuilder;
pub use compose::{compose, compose_frame_at_global, ComposedFrame, RgbaCanvas, SeekResult};
pub use conformance::{ConformanceIssue, ConformanceReport, ConformanceRule, ConformanceSeverity};
#[allow(deprecated)]
pub use decoder::{decode_first_frame, decode_lenient};
pub use decoder::{parse, parse_first_frame, parse_first_frame_with, parse_lenient, parse_with};
#[allow(deprecated)]
pub use encoder::encode_with_options;
pub use encoder::{encode_file, encode_file_with, EncodeOptions, LzwStrategy};
pub use image::{
    Application, Block, BlockClass, DisposalMethod, FramePresentation, GifFile, GifFrameData,
    GraphicControl, PlainText, Rgb, Version,
};
pub use playback::{FrameIter, LoopingFrameIter, Playback, PlaybackFrame};
pub use quantize::{
    quantize_frames_shared, quantize_rgb, quantize_rgb_with_options, quantize_rgba,
    quantize_rgba_with_options, remap_rgb_to_palette, remap_rgba_to_palette, BoxPriority, Dither,
    QuantizeOptions, Quantized, SharedQuantized,
};

// Registry-gated public surface. The `__oxideav_entry` re-export is
// load-bearing: `oxideav-meta`'s build-script-generated `register_all`
// looks up `oxideav_gif::__oxideav_entry`, which only exists at the
// crate root via this re-export.
#[cfg(feature = "registry")]
#[doc(hidden)]
pub use registry::__oxideav_entry;
#[cfg(feature = "registry")]
pub use registry::{
    from_color_signal, make_decoder, make_encoder, register, register_codecs, register_containers,
    to_color_signal, to_core_pixel_format, GifDecoder, GifEncoder, CODEC_ID_STR,
};
