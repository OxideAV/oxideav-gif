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
//!
//! ## Not implemented
//!
//! * Frame compositing semantics on top of the prior raster — the
//!   [`Frame::graphic_control`] disposal field is preserved verbatim,
//!   but reading a multi-frame stream into a final pixel buffer is
//!   left to a higher layer.
//! * Higher-level animation control extensions that ride on top of
//!   §26 Application Extensions (loop-count, etc.) — exposed as raw
//!   [`Application`] data.
//!
//! [`Frame::graphic_control`]: crate::Frame::graphic_control
//! [`Application`]: crate::Application

pub mod decoder;
pub mod encoder;
pub mod error;
pub mod image;
pub mod interlace;
pub mod lzw;

pub use decoder::decode;
pub use encoder::encode;
pub use error::{Error, Result};
pub use image::{
    Application, Block, DisposalMethod, Frame, GifImage, GraphicControl, PlainText, Rgb, Version,
};
