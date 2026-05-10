//! Pure-Rust GIF (87a / 89a) decoder + encoder.
//!
//! Implements the on-disk format defined in CompuServe's
//! *Graphics Interchange Format Version 89a* (July 1990) and the
//! original *Graphics Interchange Format* (June 1987) specs. Spec
//! references throughout the source cite the section numbers from
//! `docs/image/gif/gif89a-spec.txt` (e.g. "§17 Header", "§22 Table
//! Based Image Data", "§F LZW Compression").
//!
//! ## What's implemented
//!
//! * File header (§17), Logical Screen Descriptor (§18),
//!   Global / Local Color Tables (§19, §21).
//! * Image Descriptor (§20) + Table-Based Image Data (§22) —
//!   including the four-pass interlace order (§E).
//! * Variable-Length-Code LZW compression and decompression
//!   (Appendix F) with the deferred-clear-code rule (cover sheet).
//! * Graphic Control (§23), Comment (§24), Plain Text (§25), and
//!   Application (§26) extensions, with proper GCE → graphic-rendering
//!   block scoping per §23.d.
//! * Trailer (§27).
//! * Multi-image / animation: all `<Table-Based Image>` blocks in the
//!   data stream are surfaced through [`GifImage::frames`]. A naive
//!   per-frame compositor lives at [`GifImage::composite_frame_rgba`]
//!   and respects transparency + Disposal Method.
//!
//! ## Not implemented (in this round)
//!
//! * Animation playback machinery: the crate exposes the per-frame
//!   data and the `composite_frame_rgba` helper, but does *not* drive
//!   a real-time render loop or NETSCAPE2.0 loop parsing — the
//!   `NETSCAPE2.0` Application Extension is preserved verbatim through
//!   `GifBlock::Application` for callers that want to interpret it.
//! * On-line Capabilities Dialogue (§G) — not part of any GIF file.
//!
//! ## Standalone vs registry-integrated
//!
//! `oxideav-core` is gated behind the default-on `registry` feature.
//! Image-library consumers can depend on the crate with
//! `default-features = false` to get a `decode_gif` / `encode_gif`
//! API plus crate-local [`GifImage`] / [`GifError`] types and never
//! reference `oxideav-core`.

#![cfg_attr(not(feature = "registry"), allow(dead_code))]

pub mod decoder;
pub mod encoder;
pub mod error;
pub mod image;
pub mod lzw;

#[cfg(feature = "registry")]
pub mod registry;

pub use decoder::{decode_gif, probe, CODEC_ID_STR};
pub use encoder::encode_gif;
pub use error::{GifError, Result};
pub use image::{
    ApplicationExtension, CommentExtension, DisposalMethod, GifBlock, GifFrame, GifImage,
    GifVersion, GraphicControl, PlainTextExtension,
};

#[cfg(feature = "registry")]
pub use registry::{__oxideav_entry, register, GifDecoder, GifEncoder};
