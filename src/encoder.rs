//! GIF Data Stream encoder per GIF89a §17–§27 / §B grammar.
//!
//! Encodes a [`crate::image::GifImage`] back to bytes. Symmetric with
//! the decoder: round-tripping a parsed file is deliberately stable
//! because the on-disk Data block order is preserved.
//!
//! Choices the encoder makes:
//! * Always writes one byte of LZW Minimum Code Size derived from the
//!   active palette — `max(2, ceil(log2(palette_entries)))`. Two is the
//!   floor per Appendix F "ESTABLISH CODE SIZE".
//! * Picks the version: `GIF89a` if any 89a-only block (Graphic Control
//!   Extension, Comment, Plain Text, Application) is present in the
//!   data stream; otherwise `GIF87a` per spec §6 ("encoder should use
//!   the earliest possible version number").
//! * For interlaced output: the [`crate::image::GifFrame::interlaced`]
//!   flag is honoured — the encoder re-interlaces the natural-order
//!   pixel buffer before LZW-compressing it.
//! * Color tables are written verbatim in their raw `R,G,B,…` layout
//!   per §19. Palette length is rounded up to the nearest power-of-two
//!   between 2 and 256 by zero-padding (the on-wire `Size of Color
//!   Table` field can only express those values).

use crate::error::{GifError, Result};
use crate::image::{
    ApplicationExtension, CommentExtension, GifBlock, GifFrame, GifImage, GifVersion,
    GraphicControl, PlainTextExtension,
};
use crate::lzw;

/// Encode one [`GifImage`] into a complete GIF Data Stream.
pub fn encode_gif(img: &GifImage) -> Result<Vec<u8>> {
    let mut out = Vec::new();

    // ---- Header (§17) ----
    out.extend_from_slice(b"GIF");
    let version = pick_version(img);
    out.extend_from_slice(version.as_bytes());

    // ---- Logical Screen Descriptor (§18) ----
    out.extend_from_slice(&img.width.to_le_bytes());
    out.extend_from_slice(&img.height.to_le_bytes());

    let (gct_flag, gct_size_bits, padded_global) = match &img.global_palette {
        Some(p) => {
            let (size_bits, padded) = pad_palette(p)?;
            (true, size_bits, Some(padded))
        }
        None => (false, 0u8, None),
    };
    let mut packed: u8 = 0;
    if gct_flag {
        packed |= 0x80;
    }
    packed |= (img.color_resolution & 0x07) << 4;
    if img.global_palette_sorted {
        packed |= 0x08;
    }
    packed |= gct_size_bits & 0x07;
    out.push(packed);
    out.push(img.background_color_index);
    out.push(img.pixel_aspect_ratio);

    if let Some(p) = padded_global {
        out.extend_from_slice(&p);
    }

    // ---- Data block sequence (§B) ----
    for block in &img.blocks {
        match block {
            GifBlock::Frame(f) => {
                if let Some(gce) = &f.control {
                    write_graphic_control(&mut out, gce);
                }
                write_image_descriptor_and_data(&mut out, f, img.global_palette.as_deref())?;
            }
            GifBlock::PlainText(pt) => {
                if let Some(gce) = &pt.control {
                    write_graphic_control(&mut out, gce);
                }
                write_plain_text(&mut out, pt);
            }
            GifBlock::Comment(c) => write_comment(&mut out, c),
            GifBlock::Application(a) => write_application(&mut out, a),
        }
    }

    // ---- Trailer (§27) ----
    out.push(0x3B);

    Ok(out)
}

/// Decide the version label. §6: "the earliest possible version number
/// that includes all the blocks used in the Data Stream". 87a covers
/// header + LSD + GCT + image descriptors + LCTs + image data + trailer
/// only; any extension implies 89a.
fn pick_version(img: &GifImage) -> GifVersion {
    let needs_89 = img.blocks.iter().any(|b| match b {
        GifBlock::Frame(f) => f.control.is_some(),
        GifBlock::PlainText(_) | GifBlock::Comment(_) | GifBlock::Application(_) => true,
    });
    if needs_89 {
        GifVersion::Gif89a
    } else {
        // If the caller explicitly asked for 89a, honour it; otherwise
        // 87a is the minimal label.
        img.version
    }
}

/// Pad a palette to the nearest power-of-two number of entries between
/// 2 and 256, returning `(size_bits, padded_bytes)`.  `size_bits` is
/// the on-wire 3-bit value: actual_entries = 2^(size_bits + 1).
fn pad_palette(p: &[u8]) -> Result<(u8, Vec<u8>)> {
    if p.len() % 3 != 0 {
        return Err(GifError::invalid(format!(
            "palette length {} is not a multiple of 3 (R,G,B triplets)",
            p.len()
        )));
    }
    let entries = p.len() / 3;
    if entries == 0 {
        return Err(GifError::invalid("palette has 0 entries"));
    }
    if entries > 256 {
        return Err(GifError::invalid(format!(
            "palette has {entries} entries, max is 256"
        )));
    }
    // Smallest 2^(k+1) >= entries, k in 0..=7. So k = ceil(log2(entries)) - 1.
    let mut size_bits: u8 = 0;
    while (1usize << (size_bits as usize + 1)) < entries {
        size_bits += 1;
    }
    let padded_entries = 1usize << (size_bits as usize + 1);
    let mut padded = p.to_vec();
    padded.resize(padded_entries * 3, 0);
    Ok((size_bits, padded))
}

fn write_graphic_control(out: &mut Vec<u8>, gce: &GraphicControl) {
    out.push(0x21); // Extension Introducer (§23.c.i)
    out.push(0xF9); // Graphic Control Label (§23.c.ii)
    out.push(0x04); // Block Size (§23.c.iii) — fixed 4
    let mut packed: u8 = 0;
    // bits 4..2: Disposal Method
    packed |= (gce.disposal.to_bits() & 0x07) << 2;
    if gce.user_input {
        packed |= 0x02;
    }
    if gce.transparent_index.is_some() {
        packed |= 0x01;
    }
    out.push(packed);
    out.extend_from_slice(&gce.delay_cs.to_le_bytes());
    out.push(gce.transparent_index.unwrap_or(0));
    out.push(0x00); // Block Terminator (§23.c.ix)
}

fn write_image_descriptor_and_data(
    out: &mut Vec<u8>,
    f: &GifFrame,
    global_palette: Option<&[u8]>,
) -> Result<()> {
    out.push(0x2C); // Image Separator (§20.c.i)
    out.extend_from_slice(&f.left.to_le_bytes());
    out.extend_from_slice(&f.top.to_le_bytes());
    out.extend_from_slice(&f.width.to_le_bytes());
    out.extend_from_slice(&f.height.to_le_bytes());

    let (lct_flag, lct_size_bits, padded_local) = match &f.local_palette {
        Some(p) => {
            let (sb, padded) = pad_palette(p)?;
            (true, sb, Some(padded))
        }
        None => (false, 0u8, None),
    };
    let mut packed: u8 = 0;
    if lct_flag {
        packed |= 0x80;
    }
    if f.interlaced {
        packed |= 0x40;
    }
    if f.local_palette_sorted {
        packed |= 0x20;
    }
    packed |= lct_size_bits & 0x07;
    out.push(packed);

    if let Some(p) = padded_local {
        out.extend_from_slice(&p);
    }

    // Determine LZW Minimum Code Size from the active palette. Per
    // Appendix F "ESTABLISH CODE SIZE" the value is the bit-width of
    // the source palette, with a floor of 2.
    let active_palette_entries = if let Some(p) = &f.local_palette {
        p.len() / 3
    } else if let Some(p) = global_palette {
        p.len() / 3
    } else {
        return Err(GifError::invalid(
            "frame has no local palette and image has no global palette",
        ));
    };
    let active_padded = next_pow2_at_least_2(active_palette_entries);
    let mut code_size: u8 = 1;
    while (1usize << code_size) < active_padded {
        code_size += 1;
    }
    if code_size < 2 {
        code_size = 2;
    }
    out.push(code_size);

    // Re-interlace the natural-order index buffer if the frame was
    // marked interlaced.
    let pixels: Vec<u8> = if f.interlaced {
        interlace(&f.indices, f.width as usize, f.height as usize)
    } else {
        f.indices.clone()
    };
    let lzw_bytes = lzw::encode(&pixels, code_size)?;
    write_subblocks(out, &lzw_bytes);
    Ok(())
}

fn write_plain_text(out: &mut Vec<u8>, pt: &PlainTextExtension) {
    out.push(0x21); // Extension Introducer (§25.c.i)
    out.push(0x01); // Plain Text Label (§25.c.ii)
    out.push(0x0C); // Block Size (§25.c.iii) — fixed 12
    out.extend_from_slice(&pt.grid_left.to_le_bytes());
    out.extend_from_slice(&pt.grid_top.to_le_bytes());
    out.extend_from_slice(&pt.grid_width.to_le_bytes());
    out.extend_from_slice(&pt.grid_height.to_le_bytes());
    out.push(pt.cell_width);
    out.push(pt.cell_height);
    out.push(pt.fg_color_index);
    out.push(pt.bg_color_index);
    write_subblocks(out, &pt.data);
}

fn write_comment(out: &mut Vec<u8>, c: &CommentExtension) {
    out.push(0x21); // Extension Introducer (§24.c.i)
    out.push(0xFE); // Comment Label (§24.c.ii)
    write_subblocks(out, &c.data);
}

fn write_application(out: &mut Vec<u8>, a: &ApplicationExtension) {
    out.push(0x21); // Extension Introducer (§26.c.i)
    out.push(0xFF); // Application Extension Label (§26.c.ii)
    out.push(0x0B); // Block Size (§26.c.iii) — fixed 11
    out.extend_from_slice(&a.identifier);
    out.extend_from_slice(&a.auth_code);
    write_subblocks(out, &a.data);
}

/// Chunk a byte buffer into Data Sub-blocks (§15) and append the
/// Block Terminator (§16).
fn write_subblocks(out: &mut Vec<u8>, mut data: &[u8]) {
    while !data.is_empty() {
        let n = data.len().min(255);
        out.push(n as u8);
        out.extend_from_slice(&data[..n]);
        data = &data[n..];
    }
    out.push(0x00);
}

/// Interlace a natural-order pixel buffer back into the four-pass
/// order described in §E.
fn interlace(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    let passes: &[(usize, usize)] = &[(0, 8), (4, 8), (2, 4), (1, 2)];
    for &(start, step) in passes {
        let mut r = start;
        while r < h {
            let off = r * w;
            out.extend_from_slice(&src[off..off + w]);
            r += step;
        }
    }
    out
}

fn next_pow2_at_least_2(n: usize) -> usize {
    let mut k = 2usize;
    while k < n {
        k <<= 1;
    }
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_palette_sizes() {
        // 1 entry -> rounds up to 2 (size_bits = 0).
        let p = vec![0xff, 0x00, 0x00];
        let (sb, padded) = pad_palette(&p).unwrap();
        assert_eq!(sb, 0);
        assert_eq!(padded.len(), 6);
        // 5 entries -> rounds up to 8 (size_bits = 2).
        let p = vec![0u8; 15];
        let (sb, padded) = pad_palette(&p).unwrap();
        assert_eq!(sb, 2);
        assert_eq!(padded.len(), 24);
        // 256 entries -> rounds to 256 (size_bits = 7).
        let p = vec![0u8; 768];
        let (sb, padded) = pad_palette(&p).unwrap();
        assert_eq!(sb, 7);
        assert_eq!(padded.len(), 768);
    }

    #[test]
    fn interlace_round_trip_via_deinterlace() {
        let mut src: Vec<u8> = Vec::new();
        for i in 0..(10 * 16) {
            src.push((i % 251) as u8);
        }
        let inter = interlace(&src, 10, 16);
        let back = crate::decoder::deinterlace(&inter, 10, 16);
        assert_eq!(src, back);
    }
}
