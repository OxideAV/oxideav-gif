//! Pure-Rust GIF87a / GIF89a codec.
//!
//! Decode a byte stream with [`decode`] and produce a [`GifImage`].
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
//!   §23 Disposal Method state machine — see [`compose`].
//!
//! ## Application-extension structured views
//!
//! The CompuServe spec defines no concrete §26 Application
//! Extensions. The [`app_ext`] module layers typed parsers on top of
//! the raw [`Application`] block for the three ecosystem-defined
//! shapes that achieved cross-decoder de-facto interoperability:
//!
//! * NETSCAPE2.0 looping + buffering sub-blocks
//!   ([`app_ext::LoopControl`]) — see also [`GifImage::loop_count`]
//!   and [`GifImage::netscape_buffer_hint`].
//! * XMP packet ([`app_ext::XmpPacket`]) — see also
//!   [`GifImage::xmp_packet`].
//! * ICC colour profile ([`app_ext::IccProfile`]) — see also
//!   [`GifImage::icc_profile`].
//!
//! These accessors layer on top of the raw block list — the
//! [`Application`] block stays in [`GifImage::blocks`] regardless,
//! preserving byte-stable round-trip.
//!
//! ## Not implemented
//!
//! * Plain Text Extension glyph rendering — the spec leaves font
//!   choice to the decoder; [`compose`] treats Plain Text blocks as
//!   no-ops on the canvas.
//!
//! [`Application`]: crate::Application
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
pub mod compose;
pub mod decoder;
pub mod encoder;
pub mod error;
pub mod image;
pub mod interlace;
pub mod lzw;
#[cfg(feature = "registry")]
pub mod registry;

pub use compose::{compose, ComposedFrame, RgbaCanvas};
pub use decoder::decode;
pub use encoder::encode;
pub use error::{Error, Result};
pub use image::{
    Application, Block, DisposalMethod, Frame, GifImage, GraphicControl, PlainText, Rgb, Version,
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
