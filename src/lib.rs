//! Pure-Rust GIF87a / GIF89a codec.
//!
//! Decode a byte stream with [`decode`] and produce a [`GifImage`].
//! Use [`decode_first_frame`] when you only need the cover frame —
//! it short-circuits at the first image-bearing block and skips the
//! per-block dispatch overhead for everything that follows.
//! Construct or modify a [`GifImage`] and serialise it with [`encode`].
//!
//! ## Implemented (per CompuServe specifications)
//!
//! * Header, Logical Screen Descriptor, Global / Local Color Tables
//!   (§17–§19, §21).
//! * Image Descriptor + Table-Based Image Data, including the LZW
//!   variable-length-code compression scheme of Appendix F (§20, §22).
//! * Graphic Control Extension — disposal method, user-input flag,
//!   transparent colour index, delay time (§23).
//! * Comment, Plain Text, and Application Extensions (§24, §25, §26).
//! * Trailer (§27).
//! * Four-pass interlace transform (Appendix E) on both decode and
//!   encode.
//! * Multi-frame compositing onto the §18 Logical Screen using the
//!   §23 Disposal Method state machine — see [`compose`] for the
//!   eager `Vec<ComposedFrame>` form, [`playback::Playback`] for the
//!   lazy iterator form (one canvas at a time, plus a NETSCAPE2.0
//!   loop-count-aware [`playback::LoopingFrameIter`]).
//!
//! ## Application-extension structured views
//!
//! The CompuServe spec defines no concrete §26 Application
//! Extensions. The [`app_ext`] module layers typed parsers on top of
//! the raw [`Application`] block for the five ecosystem-defined
//! shapes that achieved cross-decoder de-facto interoperability:
//!
//! * NETSCAPE2.0 looping + buffering sub-blocks
//!   ([`app_ext::LoopControl`]) — see also [`GifImage::loop_count`]
//!   and [`GifImage::netscape_buffer_hint`].
//! * ANIMEXTS1.0 looping ([`app_ext::AnimextsLoopControl`]) — older
//!   Aldus-era variant that predates NETSCAPE2.0 and reuses the same
//!   *Looping* sub-block layout under a different identifier+auth.
//!   [`GifImage::loop_count`] falls back to it when NETSCAPE2.0 is
//!   absent.
//! * XMP packet ([`app_ext::XmpPacket`]) — see also
//!   [`GifImage::xmp_packet`].
//! * ICC colour profile ([`app_ext::IccProfile`]) — see also
//!   [`GifImage::icc_profile`].
//! * EXIF metadata ([`app_ext::ExifMetadata`]) — see also
//!   [`GifImage::exif`]. Carries a TIFF EXIF blob per Exif 2.3
//!   §4.7.2; the spec defines this for JPEG/TIFF and the GIF binding
//!   is an ecosystem convention.
//!
//! These accessors layer on top of the raw block list — the
//! [`Application`] block stays in [`GifImage::blocks`] regardless,
//! preserving byte-stable round-trip.
//!
//! ## §7 Required Version enforcement on encode
//!
//! The CompuServe spec's §7 ("Version Numbers") requires encoders to
//! label a stream with the earliest version that covers every block in
//! it. Per-block "Required Version" entries are 87a for §20 Image
//! Descriptor / §21 Local Color Table / §22 Table-Based Image Data and
//! 89a for §23 Graphic Control / §24 Comment / §25 Plain Text / §26
//! Application Extensions.
//!
//! [`encode`] enforces this on the encoder side: a [`GifImage`] declared
//! [`Version::Gif87a`] that contains any 89a-only block is rejected with
//! [`Error::InvalidInput`] before any bytes go to the wire — the
//! alternative ("a `GIF87a` header followed by 89a-only block payloads")
//! would be structurally invalid.
//!
//! Two recovery helpers ride on top: [`GifImage::required_version`]
//! returns the minimum version that covers the current block list, and
//! [`GifImage::upgrade_version_if_needed`] bumps the declared version up
//! to that minimum in one call. The upgrade helper never *down*grades:
//! a caller's explicit choice of [`Version::Gif89a`] for an
//! 87a-compatible payload is preserved.
//!
//! ## §24 Comment Extension accessors
//!
//! The CompuServe spec (§24.a) allows any number of Comment
//! Extensions in the Data Stream and recommends (§24.e.i) that they
//! contain 7-bit ASCII text and (§24.e.ii) that they appear at the
//! beginning or end of the stream. [`GifImage::comments`] iterates the
//! payloads in source order; [`GifImage::concatenated_comment`]
//! returns every payload joined with a single LF (or `None` when no
//! Comment Extension is present). [`GifImage::comments_are_7bit_ascii`]
//! and [`GifImage::comments_in_recommended_position`] surface the
//! §24.e *recommendations* as boolean queries so a caller that wants
//! to honour them strictly can gate on them; the encoder itself never
//! enforces a recommendation.
//!
//! ## Plain Text rendering
//!
//! The §25 Plain Text Extension leaves font choice to the decoder
//! (§25.e). [`compose`] / [`playback`] render Plain Text blocks
//! against a crate-local clean-room 8×8 monospace bitmap font (see
//! [`font`]) using the §23.c.viii transparent-index handling for the
//! background colour. Out-of-range characters fall back to space per
//! §25.e.
//!
//! [`Application`]: crate::Application
//!
//! ## Conformance reporting
//!
//! [`encode`] fatally rejects the small set of departures that make a
//! stream un-encodable (§7 version mismatch, §19/§21 colour-table size,
//! §20/§22 indices length). [`GifImage::conformance_report`] is the
//! complementary *non-fatal* diagnostic surface: it walks an in-memory
//! image against the Appendix-B grammar and the §7–§26 field rules and
//! returns a [`ConformanceReport`] of every departure — a *superset* of
//! the encoder's checks that additionally surfaces the placement /
//! range / recommendation issues the encoder tolerates (§20.a images
//! escaping the Logical Screen, §18.c.vii / §22 / §23.c.viii / §25
//! index references past the active palette, the §23.e.ii
//! User-Input-without-Delay recommendation). Each [`ConformanceIssue`]
//! carries a [`ConformanceRule`], a [`ConformanceSeverity`]
//! (`Error` for requirements, `Recommendation` for recommendations), an
//! offending `block_index`, and a spec-cited `detail`.
//! [`GifImage::validate_strict`] is the hard-gate convenience that turns
//! the error-level issues into a single [`Error::InvalidInput`] while
//! tolerating recommendations.
//!
//! ## Standalone vs registry-integrated
//!
//! With the default `registry` Cargo feature on, the crate exposes
//! [`oxideav_core::Decoder`] / [`oxideav_core::Encoder`] trait impls
//! plus a [`registry::register`] entry point against `oxideav-core`.
//! With the feature off the crate ships only the standalone
//! [`decode`] / [`encode`] / [`compose`] API plus the local
//! [`GifImage`] / [`Error`] types, with no `oxideav-core` dep in the
//! tree. Image-library consumers should depend on `oxideav-gif` with
//! `default-features = false`.

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
pub mod playback;
pub mod quantize;
#[cfg(feature = "registry")]
pub mod registry;

pub use app_ext::ApplicationKind;
pub use builder::AnimationBuilder;
pub use compose::{compose, ComposedFrame, RgbaCanvas};
pub use conformance::{ConformanceIssue, ConformanceReport, ConformanceRule, ConformanceSeverity};
pub use decoder::{decode, decode_first_frame, decode_lenient};
pub use encoder::{encode, encode_with_options, EncodeOptions, LzwStrategy};
pub use error::{Error, Result};
pub use image::{
    Application, Block, BlockClass, DisposalMethod, Frame, GifImage, GraphicControl, PlainText,
    Rgb, Version,
};
pub use playback::{FrameIter, LoopingFrameIter, Playback, PlaybackFrame};
pub use quantize::{
    quantize_rgb, quantize_rgb_with_options, quantize_rgba, quantize_rgba_with_options, Dither,
    QuantizeOptions, Quantized,
};

// Registry-gated public surface. The `__oxideav_entry` re-export is
// load-bearing: `oxideav-meta`'s build-script-generated `register_all`
// looks up `oxideav_gif::__oxideav_entry`, which only exists at the
// crate root via this re-export.
#[cfg(feature = "registry")]
pub use registry::{
    __oxideav_entry, register, register_codecs, register_containers, GifDecoder, GifEncoder,
    CODEC_ID_STR,
};
