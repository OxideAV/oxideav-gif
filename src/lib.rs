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
//! ## Not implemented
//!
//! * Higher-level animation control extensions that ride on top of
//!   §26 Application Extensions (loop-count, etc.) — exposed as raw
//!   [`Application`] data.
//! * Plain Text Extension glyph rendering — the spec leaves font
//!   choice to the decoder; [`compose`] treats Plain Text blocks as
//!   no-ops on the canvas.
//!
//! [`Application`]: crate::Application

pub mod compose;
pub mod decoder;
pub mod encoder;
pub mod error;
pub mod image;
pub mod interlace;
pub mod lzw;

pub use compose::{compose, ComposedFrame, RgbaCanvas};
pub use decoder::decode;
pub use encoder::encode;
pub use error::{Error, Result};
pub use image::{
    Application, Block, DisposalMethod, Frame, GifImage, GraphicControl, PlainText, Rgb, Version,
};
