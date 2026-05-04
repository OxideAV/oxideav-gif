//! GIF frame encoder.
//!
//! The standalone API ([`encode_gif`]) takes a sequence of
//! [`GifFrame`]s and emits a complete GIF89a byte stream. The first
//! frame's palette becomes the global colour table; every frame can
//! still carry its own palette and the muxer-side path will emit a
//! local colour table when those palettes diverge.
//!
//! The [`crate::registry`]-gated [`Encoder`](oxideav_core::Encoder)
//! trait wrapper accepts `Pal8` `VideoFrame`s (plane 0 = indices,
//! plane 1 = packed RGBA palette) and emits container packets via the
//! same code path.
//!
//! ## Per-frame disposal and transparency
//!
//! The standalone `encode_gif` API takes per-frame `disposal` /
//! `transparent_index` values directly via [`GifEncoderFrame`]. The
//! framework `Encoder` trait only carries pixel data + pts, so the
//! [`GifEncoder`] type exposes
//! [`GifEncoder::set_next_disposal`] / [`GifEncoder::set_next_transparent_index`]
//! side-channel hooks consumed by the next `send_frame` call.

use crate::container::{encode_frame_payload, ParsedFrame};
use crate::error::{GifError as Error, Result};
use crate::image::GifFrame;
use crate::lzw::Lzw;

// Backward-compat re-export: existing callers reach for
// `oxideav_gif::encoder::make_encoder` to construct a framework-side
// encoder. Keep that path live by re-exporting the registry-side
// factory.
#[cfg(feature = "registry")]
pub use crate::registry::{make_encoder, GifEncoder};

/// Default frame delay when the caller doesn't specify one, in GIF time
/// units (1/100 s). 10 cs ≈ 10 fps — a sensible baseline that every
/// viewer handles.
pub const DEFAULT_DELAY_CS: u16 = 10;

/// Per-frame metadata accepted by [`encode_gif`]. Carries the index
/// plane + palette plus the GIF-specific knobs (disposal /
/// transparency / interlace) the standalone surface can't fold onto
/// the framework's `Frame` model.
#[derive(Clone, Debug)]
pub struct GifEncoderFrame {
    /// The frame to encode. `width` / `height` must match the canvas;
    /// `delay_cs` becomes the GCE delay.
    pub frame: GifFrame,
    /// Disposal method (`0` unspecified, `1` keep, `2` background,
    /// `3` previous). See the GIF89a spec §23.
    pub disposal: u8,
    /// Optional transparent palette index. `None` = fully opaque.
    pub transparent_index: Option<u8>,
    /// If `true`, the frame is encoded as a 4-pass interlace.
    /// Defaults to `false`.
    pub interlaced: bool,
}

impl From<GifFrame> for GifEncoderFrame {
    fn from(frame: GifFrame) -> Self {
        Self {
            frame,
            disposal: 0,
            transparent_index: None,
            interlaced: false,
        }
    }
}

/// Encode a sequence of [`GifEncoderFrame`]s as a complete GIF89a file.
/// Standalone (no `oxideav-core`) entry point.
///
/// All frames must share the same `width` / `height` (= the canvas
/// size). The first frame's palette becomes the global colour table;
/// `loop_count` controls the NETSCAPE2.0 extension (`Some(0)` = loop
/// forever, `None` = no loop extension at all).
pub fn encode_gif(frames: &[GifEncoderFrame], loop_count: Option<u16>) -> Result<Vec<u8>> {
    if frames.is_empty() {
        return Err(Error::invalid("GIF encoder: no frames"));
    }
    let canvas_w = frames[0].frame.width;
    let canvas_h = frames[0].frame.height;
    for f in &frames[1..] {
        if f.frame.width != canvas_w || f.frame.height != canvas_h {
            return Err(Error::invalid(
                "GIF encoder: every frame's width / height must match the first frame",
            ));
        }
    }

    let gct = frames[0].frame.palette.clone();
    let gct_present = !gct.is_empty();
    let gct_size_exp = if gct_present {
        size_exp_for(gct.len())
    } else {
        0
    };

    let mut buf: Vec<u8> = Vec::new();
    // Signature.
    buf.extend_from_slice(b"GIF89a");
    // Logical Screen Descriptor.
    buf.extend_from_slice(&(canvas_w as u16).to_le_bytes());
    buf.extend_from_slice(&(canvas_h as u16).to_le_bytes());
    let mut packed: u8 = 0;
    if gct_present {
        packed |= 0x80;
        packed |= 0x70; // color resolution = 7 (common default)
        packed |= (gct_size_exp as u8) & 0x07;
    }
    buf.push(packed);
    buf.push(0); // background color index
    buf.push(0); // pixel aspect ratio

    if gct_present {
        let padded_len = 1usize << (gct_size_exp + 1);
        write_palette(&mut buf, &gct, padded_len);
    }

    // NETSCAPE2.0 loop extension when requested.
    if let Some(lc) = loop_count {
        buf.push(0x21);
        buf.push(0xFF);
        buf.push(0x0B);
        buf.extend_from_slice(b"NETSCAPE2.0");
        buf.push(0x03);
        buf.push(0x01);
        buf.extend_from_slice(&lc.to_le_bytes());
        buf.push(0x00);
    }

    for f in frames {
        write_frame(&mut buf, f, &gct)?;
    }

    // Trailer.
    buf.push(0x3B);
    Ok(buf)
}

fn write_frame(buf: &mut Vec<u8>, ef: &GifEncoderFrame, global: &[[u8; 4]]) -> Result<()> {
    // Graphic Control Extension.
    buf.push(0x21);
    buf.push(0xF9);
    buf.push(0x04);
    let mut flags = 0u8;
    flags |= (ef.disposal & 0x07) << 2;
    if ef.transparent_index.is_some() {
        flags |= 0x01;
    }
    buf.push(flags);
    buf.extend_from_slice(&ef.frame.delay_cs.to_le_bytes());
    buf.push(ef.transparent_index.unwrap_or(0));
    buf.push(0); // block terminator

    // Image Descriptor.
    buf.push(0x2C);
    buf.extend_from_slice(&0u16.to_le_bytes()); // x
    buf.extend_from_slice(&0u16.to_le_bytes()); // y
    buf.extend_from_slice(&(ef.frame.width as u16).to_le_bytes());
    buf.extend_from_slice(&(ef.frame.height as u16).to_le_bytes());
    let has_local = ef.frame.palette != global;
    let mut packed: u8 = 0;
    let lct_exp = if has_local {
        size_exp_for(ef.frame.palette.len())
    } else {
        0
    };
    if has_local {
        packed |= 0x80;
        packed |= (lct_exp as u8) & 0x07;
    }
    if ef.interlaced {
        packed |= 0x40;
    }
    buf.push(packed);
    if has_local {
        let padded = 1usize << (lct_exp + 1);
        write_palette(buf, &ef.frame.palette, padded);
    } else if global.is_empty() {
        return Err(Error::invalid(
            "GIF muxer: frame has no local palette and no global palette",
        ));
    }

    // LZW min-code-size + compressed sub-block chain.
    let min_code_size = min_code_size_for(ef.frame.palette.len());
    buf.push(min_code_size);
    let mut enc = Lzw::encoder(min_code_size)?;
    let mut lzw_data = Vec::new();
    enc.write(&ef.frame.indices, &mut lzw_data);
    enc.finish(&mut lzw_data);
    write_sub_blocks(buf, &lzw_data);
    Ok(())
}

fn write_palette(buf: &mut Vec<u8>, pal: &[[u8; 4]], padded_len: usize) {
    for i in 0..padded_len {
        if i < pal.len() {
            buf.push(pal[i][0]);
            buf.push(pal[i][1]);
            buf.push(pal[i][2]);
        } else {
            buf.push(0);
            buf.push(0);
            buf.push(0);
        }
    }
}

fn write_sub_blocks(buf: &mut Vec<u8>, data: &[u8]) {
    let mut p = 0;
    while p < data.len() {
        let chunk = (data.len() - p).min(255);
        buf.push(chunk as u8);
        buf.extend_from_slice(&data[p..p + chunk]);
        p += chunk;
    }
    buf.push(0);
}

fn size_exp_for(n: usize) -> u32 {
    // GIF stores size-1 as `2^(size+1)` entries, so for N colours the
    // exponent is `ceil(log2(N)) - 1`, clamped to `[0, 7]`.
    if n <= 2 {
        0
    } else if n <= 4 {
        1
    } else if n <= 8 {
        2
    } else if n <= 16 {
        3
    } else if n <= 32 {
        4
    } else if n <= 64 {
        5
    } else if n <= 128 {
        6
    } else {
        7
    }
}

/// Compute `ceil(log2(max(2, palette_len)))`, clamped to `[2, 8]`. GIF
/// requires the LZW initial-code-width to fit the whole palette plus
/// the two reserved codes, and the minimum alphabet width is 2 bits.
pub(crate) fn min_code_size_for(palette_len: usize) -> u8 {
    let n = palette_len.max(2) as u32;
    let bits = 32 - (n - 1).leading_zeros();
    bits.clamp(2, 8) as u8
}

/// Compress one [`GifFrame`]'s indices into LZW + container payload
/// bytes ready for the framework `Muxer`. Used by the registry-side
/// `Encoder` trait impl.
pub(crate) fn frame_to_payload(
    frame: &GifFrame,
    canvas: (u32, u32),
    disposal: u8,
    transparent_index: Option<u8>,
) -> Vec<u8> {
    let min_code_size = min_code_size_for(frame.palette.len());
    let mut enc = Lzw::encoder(min_code_size).expect("min_code_size_for clamps to 2..=8");
    let mut lzw_data = Vec::new();
    enc.write(&frame.indices, &mut lzw_data);
    enc.finish(&mut lzw_data);

    let parsed = ParsedFrame {
        x: 0,
        y: 0,
        w: frame.width,
        h: frame.height,
        delay_cs: frame.delay_cs,
        disposal,
        transparent_index,
        interlaced: false,
        min_code_size,
        local_palette: frame.palette.clone(),
        lzw_data,
    };
    encode_frame_payload(&parsed, canvas)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_code_size_basics() {
        assert_eq!(min_code_size_for(2), 2);
        assert_eq!(min_code_size_for(4), 2);
        assert_eq!(min_code_size_for(5), 3);
        assert_eq!(min_code_size_for(8), 3);
        assert_eq!(min_code_size_for(16), 4);
        assert_eq!(min_code_size_for(256), 8);
    }
}
