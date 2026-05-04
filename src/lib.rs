//! Pure-Rust GIF codec + container.
//!
//! Handles both GIF87a and GIF89a. Decoding supports:
//!
//! * Logical Screen Descriptor with optional Global Color Table.
//! * Per-frame Image Descriptor with optional Local Color Table and the
//!   classic 4-pass interlace.
//! * Graphic Control Extension — delay time, disposal method, transparent
//!   colour index.
//! * Application Extension — NETSCAPE2.0 loop count is surfaced in
//!   container metadata; other application extensions are skipped.
//! * Comment + Plain Text extensions are silently skipped.
//! * LZW decode covering the whole 2..=12 bit code-width ladder, clear
//!   codes, and EOI.
//!
//! Encoding produces GIF89a output:
//!
//! * A Global Color Table sourced from the first frame's palette.
//! * A Graphic Control Extension per frame (delay + disposal).
//! * A NETSCAPE2.0 application extension when writing more than one
//!   frame (loop count = 0 = infinite).
//! * LZW-compressed image data with clear-on-full semantics (clear code
//!   emitted when the dictionary fills at 4096 entries).
//!
//! The encoder requires `Pal8` input. The DAG pipeline resolver will
//! auto-insert a pixfmt conversion when the upstream frame is RGBA.
//!
//! ## Standalone (no `oxideav-core`) mode
//!
//! `oxideav-core` is gated behind the default-on `registry` feature. With
//! the feature off, the crate exposes a free-standing
//! [`decode_gif`] / [`encode_gif`] API plus crate-local
//! [`GifImage`] / [`GifFrame`] / [`GifError`] types and never references
//! `oxideav-core`. Image-library consumers depend on this crate with
//! `default-features = false` to skip the framework dependency tree
//! entirely.

#![allow(clippy::needless_range_loop)]
// When built without the `registry` feature, the trait wrappers don't
// exist so a few helpers go unused. Suppress crate-wide rather than
// gating each individually.
#![cfg_attr(not(feature = "registry"), allow(dead_code))]

pub mod container;
pub mod decoder;
pub mod encoder;
pub mod error;
pub mod image;
pub mod lzw;
#[cfg(feature = "registry")]
pub mod registry;

/// Codec id for GIF image frames.
pub const GIF_CODEC_ID: &str = "gif";

// Public unconditional API — works whether or not `registry` is enabled.
pub use decoder::decode_gif;
pub use encoder::{encode_gif, GifEncoderFrame, DEFAULT_DELAY_CS};
pub use error::{GifError, Result};
pub use image::{GifFrame, GifImage};
pub use lzw::{Lzw, LzwDecoder, LzwEncoder};

// Public registry-gated API — keeps the framework integration surface
// (Decoder/Encoder/Demuxer/Muxer trait impls, `register*` helpers,
// `GifEncoder` trait wrapper) behind the default-on `registry` feature
// so image-library callers can build the crate without dragging in
// `oxideav-core`.
#[cfg(feature = "registry")]
pub use registry::{register, register_codecs, register_containers, GifEncoder};
