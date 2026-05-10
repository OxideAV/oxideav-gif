//! GIF87a / GIF89a top-level encoder.

use crate::error::{Error, Result};
use crate::image::{Application, Block, Frame, GifImage, GraphicControl, PlainText, Rgb, Version};
use crate::interlace::interlace_row_order;
use crate::lzw;

/// Serialise a [`GifImage`] into a byte stream conforming to the
/// `<GIF Data Stream>` grammar in Appendix B.
pub fn encode(image: &GifImage) -> Result<Vec<u8>> {
    validate(image)?;

    let mut out = Vec::new();
    write_header(&mut out, image.version);
    write_logical_screen_descriptor(&mut out, image)?;
    if let Some(palette) = &image.global_palette {
        write_color_table(&mut out, palette)?;
    }

    for block in &image.blocks {
        match block {
            Block::Image(frame) => {
                if let Some(gce) = &frame.graphic_control {
                    write_graphic_control_extension(&mut out, gce);
                }
                write_image_block(&mut out, frame, image.global_palette.as_deref())?;
            }
            Block::PlainText {
                params,
                graphic_control,
            } => {
                if let Some(gce) = graphic_control {
                    write_graphic_control_extension(&mut out, gce);
                }
                write_plain_text_extension(&mut out, params)?;
            }
            Block::Comment(data) => {
                write_comment_extension(&mut out, data)?;
            }
            Block::Application(app) => {
                write_application_extension(&mut out, app)?;
            }
        }
    }

    // §27 — Trailer.
    out.push(0x3B);
    Ok(out)
}

// ---------------------------------------------------------------------
// Validation.
// ---------------------------------------------------------------------

fn validate(image: &GifImage) -> Result<()> {
    if image.color_resolution > 7 {
        return Err(Error::InvalidInput(format!(
            "color_resolution {} exceeds 3-bit field range",
            image.color_resolution
        )));
    }
    if let Some(palette) = &image.global_palette {
        validate_palette(palette, "global")?;
    }
    for block in &image.blocks {
        if let Block::Image(frame) = block {
            if let Some(palette) = &frame.local_palette {
                validate_palette(palette, "local")?;
            }
            let pixels_expected = (frame.width as usize)
                .checked_mul(frame.height as usize)
                .ok_or_else(|| Error::InvalidInput("frame width × height overflows".into()))?;
            if frame.indices.len() != pixels_expected {
                return Err(Error::InvalidInput(format!(
                    "frame indices length {} != width*height {}",
                    frame.indices.len(),
                    pixels_expected
                )));
            }
        }
    }
    Ok(())
}

fn validate_palette(palette: &[Rgb], scope: &str) -> Result<()> {
    if palette.is_empty() {
        return Err(Error::InvalidInput(format!(
            "{scope} colour table must contain at least one entry"
        )));
    }
    if palette.len() > 256 {
        return Err(Error::InvalidInput(format!(
            "{scope} colour table has {} entries; max is 256",
            palette.len()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Block writers.
// ---------------------------------------------------------------------

fn write_header(out: &mut Vec<u8>, version: Version) {
    out.extend_from_slice(b"GIF");
    out.extend_from_slice(&version.ascii());
}

fn write_logical_screen_descriptor(out: &mut Vec<u8>, image: &GifImage) -> Result<()> {
    write_u16_le(out, image.screen_width);
    write_u16_le(out, image.screen_height);

    let mut packed: u8 = 0;
    if let Some(palette) = &image.global_palette {
        packed |= 0b1000_0000;
        let bits = size_bits_for_palette(palette.len())?;
        packed |= bits;
    }
    packed |= (image.color_resolution & 0b0000_0111) << 4;
    if image.global_palette_sorted {
        packed |= 0b0000_1000;
    }
    out.push(packed);

    out.push(image.background_index);
    out.push(image.pixel_aspect_ratio);
    Ok(())
}

fn write_color_table(out: &mut Vec<u8>, palette: &[Rgb]) -> Result<()> {
    let bits = size_bits_for_palette(palette.len())?;
    let on_disk_count = 1usize << (bits as u32 + 1);
    for entry in palette {
        out.push(entry.r);
        out.push(entry.g);
        out.push(entry.b);
    }
    // §19 / §21: the on-disk size is the next power of two ≥ entry
    // count (range 2..=256). Pad unused slots with black.
    for _ in palette.len()..on_disk_count {
        out.push(0);
        out.push(0);
        out.push(0);
    }
    Ok(())
}

fn write_image_block(
    out: &mut Vec<u8>,
    frame: &Frame,
    global_palette: Option<&[Rgb]>,
) -> Result<()> {
    out.push(0x2C); // Image Separator (§20.c.i)
    write_u16_le(out, frame.left);
    write_u16_le(out, frame.top);
    write_u16_le(out, frame.width);
    write_u16_le(out, frame.height);

    let mut packed: u8 = 0;
    if let Some(palette) = &frame.local_palette {
        packed |= 0b1000_0000;
        let bits = size_bits_for_palette(palette.len())?;
        packed |= bits;
    }
    if frame.interlaced {
        packed |= 0b0100_0000;
    }
    if frame.palette_sorted {
        packed |= 0b0010_0000;
    }
    out.push(packed);

    if let Some(palette) = &frame.local_palette {
        write_color_table(out, palette)?;
    }

    // §22.c.i — LZW Minimum Code Size derived from active palette
    // size, with a floor of 2 ("ESTABLISH CODE SIZE" black-and-white
    // clause).
    let active_palette_len = frame
        .local_palette
        .as_deref()
        .or(global_palette)
        .map(|p| p.len())
        .ok_or_else(|| {
            Error::InvalidInput(
                "frame has no local palette and no global palette is present".into(),
            )
        })?;
    let raw_bits = bits_required_for(active_palette_len);
    let min_code_size = raw_bits.max(2);

    // Validate every index fits in the active palette.
    for &idx in &frame.indices {
        if (idx as usize) >= active_palette_len {
            return Err(Error::InvalidInput(format!(
                "pixel index {idx} >= active palette size {active_palette_len}"
            )));
        }
    }

    // Re-shuffle into interlaced storage order if requested (Appendix E).
    let pixels_for_lzw: Vec<u8> = if frame.interlaced {
        let row_order = interlace_row_order(frame.height);
        let row_bytes = frame.width as usize;
        let mut buf = Vec::with_capacity(frame.indices.len());
        for &target_row in &row_order {
            let dst_off = (target_row as usize) * row_bytes;
            buf.extend_from_slice(&frame.indices[dst_off..dst_off + row_bytes]);
        }
        buf
    } else {
        frame.indices.clone()
    };

    let compressed = lzw::encode(min_code_size, &pixels_for_lzw)?;
    out.push(min_code_size);
    write_data_sub_blocks(out, &compressed);
    Ok(())
}

fn write_graphic_control_extension(out: &mut Vec<u8>, gce: &GraphicControl) {
    out.push(0x21); // Extension Introducer
    out.push(0xF9); // Graphic Control Label
    out.push(4); // Block Size — fixed 4 (§23.c.iii)
    let mut packed: u8 = 0;
    packed |= (gce.disposal.as_bits() & 0b0000_0111) << 2;
    if gce.user_input {
        packed |= 0b0000_0010;
    }
    if gce.transparent_index.is_some() {
        packed |= 0b0000_0001;
    }
    out.push(packed);
    write_u16_le(out, gce.delay_centis);
    out.push(gce.transparent_index.unwrap_or(0));
    out.push(0); // Block Terminator (§23.c.ix)
}

fn write_plain_text_extension(out: &mut Vec<u8>, params: &PlainText) -> Result<()> {
    out.push(0x21);
    out.push(0x01);
    out.push(12); // Block Size — fixed 12 (§25.c.iii)
    write_u16_le(out, params.left);
    write_u16_le(out, params.top);
    write_u16_le(out, params.width);
    write_u16_le(out, params.height);
    out.push(params.cell_width);
    out.push(params.cell_height);
    out.push(params.fg_color_index);
    out.push(params.bg_color_index);
    write_data_sub_blocks(out, &params.text);
    Ok(())
}

fn write_comment_extension(out: &mut Vec<u8>, data: &[u8]) -> Result<()> {
    out.push(0x21);
    out.push(0xFE);
    write_data_sub_blocks(out, data);
    Ok(())
}

fn write_application_extension(out: &mut Vec<u8>, app: &Application) -> Result<()> {
    out.push(0x21);
    out.push(0xFF);
    out.push(11); // Block Size — fixed 11 (§26.c.iii)
    out.extend_from_slice(&app.identifier);
    out.extend_from_slice(&app.auth_code);
    write_data_sub_blocks(out, &app.data);
    Ok(())
}

// ---------------------------------------------------------------------
// Sub-block packaging — §15 (Data Sub-blocks) and §16 (Block
// Terminator). A zero-length sub-block ends the sequence even when no
// data was written, so an empty payload still emits a single 0x00.
// ---------------------------------------------------------------------

fn write_data_sub_blocks(out: &mut Vec<u8>, data: &[u8]) {
    let mut i = 0;
    while i < data.len() {
        let chunk_len = (data.len() - i).min(255);
        out.push(chunk_len as u8);
        out.extend_from_slice(&data[i..i + chunk_len]);
        i += chunk_len;
    }
    out.push(0); // Block Terminator
}

// ---------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------

fn write_u16_le(out: &mut Vec<u8>, v: u16) {
    out.push((v & 0xFF) as u8);
    out.push(((v >> 8) & 0xFF) as u8);
}

/// Map an entry count (1..=256) to the 3-bit "Size" field used by
/// §18.c.vi and §20.c.ix: actual entries = `2^(field + 1)`.
fn size_bits_for_palette(n: usize) -> Result<u8> {
    if n == 0 || n > 256 {
        return Err(Error::InvalidInput(format!(
            "palette size {n} outside 1..=256"
        )));
    }
    // Smallest k in 0..=7 with 2^(k+1) >= n.
    for k in 0u8..=7 {
        if (1usize << (k as u32 + 1)) >= n {
            return Ok(k);
        }
    }
    unreachable!()
}

/// Bits needed to address `n` distinct values. 0 → 0 bits; 1 → 0 bits;
/// 2 → 1 bit; 3..=4 → 2 bits; ...
fn bits_required_for(n: usize) -> u8 {
    if n <= 1 {
        0
    } else {
        // Minimum k such that 2^k >= n.
        let mut k = 0u8;
        while (1usize << k as u32) < n {
            k += 1;
        }
        k
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_bits_table() {
        // 2^(k+1) entries → k.
        assert_eq!(size_bits_for_palette(2).unwrap(), 0);
        assert_eq!(size_bits_for_palette(3).unwrap(), 1);
        assert_eq!(size_bits_for_palette(4).unwrap(), 1);
        assert_eq!(size_bits_for_palette(5).unwrap(), 2);
        assert_eq!(size_bits_for_palette(8).unwrap(), 2);
        assert_eq!(size_bits_for_palette(16).unwrap(), 3);
        assert_eq!(size_bits_for_palette(256).unwrap(), 7);
        assert!(size_bits_for_palette(0).is_err());
        assert!(size_bits_for_palette(257).is_err());
    }

    #[test]
    fn bits_required_table() {
        assert_eq!(bits_required_for(0), 0);
        assert_eq!(bits_required_for(1), 0);
        assert_eq!(bits_required_for(2), 1);
        assert_eq!(bits_required_for(3), 2);
        assert_eq!(bits_required_for(4), 2);
        assert_eq!(bits_required_for(5), 3);
        assert_eq!(bits_required_for(16), 4);
        assert_eq!(bits_required_for(256), 8);
    }
}
