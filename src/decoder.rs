//! GIF Data Stream parser per GIF89a §17–§27 / §B grammar.
//!
//! Reads bytes left-to-right, sequentially classifying blocks by
//! introducer (Image Separator `0x2C`, Extension Introducer `0x21`,
//! Trailer `0x3B`) and dispatching to per-block parsers. The result is
//! a [`crate::image::GifImage`] whose `blocks` field preserves the
//! on-disk Data block order so the encoder can write it back out.
//!
//! De-interlacing happens here (§E) — `GifFrame::indices` is always in
//! natural top-to-bottom order. The `interlaced` flag is preserved so
//! the encoder can re-emit the image interlaced if it chooses to.

use crate::error::{GifError, Result};
use crate::image::{
    ApplicationExtension, CommentExtension, DisposalMethod, GifBlock, GifFrame, GifImage,
    GifVersion, GraphicControl, PlainTextExtension,
};
use crate::lzw;

/// Codec id string used by the framework registry layer.
pub const CODEC_ID_STR: &str = "gif";

/// On-wire constants from the spec (§§ where each is defined are
/// in-line below).
const SIGNATURE: &[u8; 3] = b"GIF";
const VERSION_87A: &[u8; 3] = b"87a";
const VERSION_89A: &[u8; 3] = b"89a";

const IMAGE_SEPARATOR: u8 = 0x2C; // §20.c.i
const EXTENSION_INTRODUCER: u8 = 0x21; // §23.c.i / §24.c.i / §25.c.i / §26.c.i
const TRAILER: u8 = 0x3B; // §27.c.i

const LABEL_GRAPHIC_CONTROL: u8 = 0xF9; // §23.c.ii
const LABEL_COMMENT: u8 = 0xFE; // §24.c.ii
const LABEL_PLAIN_TEXT: u8 = 0x01; // §25.c.ii
const LABEL_APPLICATION: u8 = 0xFF; // §26.c.ii

/// Sequential cursor over a byte slice. Errors out on every short read,
/// keeping the decoder strictly defensive against truncated inputs.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn read_u8(&mut self) -> Result<u8> {
        if self.pos >= self.buf.len() {
            return Err(GifError::invalid("truncated: expected byte"));
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn read_u16_le(&mut self) -> Result<u16> {
        if self.pos + 2 > self.buf.len() {
            return Err(GifError::invalid("truncated: expected u16"));
        }
        let v = u16::from_le_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            return Err(GifError::invalid(format!(
                "truncated: expected {n} bytes, only {} remaining",
                self.remaining()
            )));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
}

/// Read a sequence of Data Sub-blocks (§15) terminated by a 0x00 Block
/// Terminator (§16). Returns the concatenated payload.
fn read_subblocks(c: &mut Cursor<'_>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let n = c.read_u8()? as usize;
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(c.read_bytes(n)?);
    }
}

/// Skip a sequence of Data Sub-blocks without copying — used to step
/// past extensions whose label we don't recognise (per §F decoder
/// guidance: "decoders must be able to recognize the existence of GIF
/// Extension Blocks and read past them if unable to process the
/// function code").
fn skip_subblocks(c: &mut Cursor<'_>) -> Result<()> {
    loop {
        let n = c.read_u8()? as usize;
        if n == 0 {
            return Ok(());
        }
        let _ = c.read_bytes(n)?;
    }
}

/// Top-level entry point: parse one full GIF Data Stream.
pub fn decode_gif(input: &[u8]) -> Result<GifImage> {
    let mut c = Cursor::new(input);

    // ---- Header (§17) ----
    let sig = c.read_bytes(3)?;
    if sig != SIGNATURE {
        return Err(GifError::invalid(format!(
            "bad signature: {sig:?} (expected b\"GIF\")"
        )));
    }
    let ver = c.read_bytes(3)?;
    let version = if ver == VERSION_87A {
        GifVersion::Gif87a
    } else if ver == VERSION_89A {
        GifVersion::Gif89a
    } else {
        return Err(GifError::invalid(format!(
            "unknown GIF version: {ver:?} (expected b\"87a\" or b\"89a\")"
        )));
    };

    // ---- Logical Screen Descriptor (§18) ----
    let lw = c.read_u16_le()?;
    let lh = c.read_u16_le()?;
    let packed = c.read_u8()?;
    let bg_index = c.read_u8()?;
    let par = c.read_u8()?;

    // §18.c packed-fields layout — bits, MSB to LSB:
    //   7  : Global Color Table Flag
    //   6,5,4: Color Resolution
    //   3  : Sort Flag
    //   2,1,0: Size of Global Color Table
    let gct_flag = (packed & 0x80) != 0;
    let color_res = (packed >> 4) & 0x07;
    let sort_flag = (packed & 0x08) != 0;
    let gct_size_bits = packed & 0x07;

    // ---- Global Color Table (§19) ----
    let global_palette = if gct_flag {
        // 3 * 2^(size+1) bytes per §19.
        let n_bytes = 3 * (1usize << (gct_size_bits as usize + 1));
        Some(c.read_bytes(n_bytes)?.to_vec())
    } else {
        None
    };

    let mut blocks: Vec<GifBlock> = Vec::new();
    // A pending GCE applies to the next graphic-rendering block (Image
    // Descriptor or Plain Text Extension) — §23.d.
    let mut pending_gce: Option<GraphicControl> = None;

    // ---- Block dispatch loop (§B grammar) ----
    loop {
        let intro = c.read_u8()?;
        match intro {
            TRAILER => {
                // Trailer (§27). End of Data Stream.
                if pending_gce.is_some() {
                    // Per spec §23.d, a GCE without a target is a
                    // grammar violation. Be lenient and discard.
                }
                break;
            }
            IMAGE_SEPARATOR => {
                let frame = parse_image_descriptor(&mut c, pending_gce.take())?;
                blocks.push(GifBlock::Frame(frame));
            }
            EXTENSION_INTRODUCER => {
                let label = c.read_u8()?;
                match label {
                    LABEL_GRAPHIC_CONTROL => {
                        // §23. Block Size = 4 (fixed).
                        let bs = c.read_u8()?;
                        if bs != 4 {
                            return Err(GifError::invalid(format!(
                                "Graphic Control Extension Block Size = {bs} (expected 4)"
                            )));
                        }
                        let gce_packed = c.read_u8()?;
                        let delay = c.read_u16_le()?;
                        let trans_idx = c.read_u8()?;
                        // GCE packed fields, bits MSB to LSB:
                        //   7,6,5: Reserved
                        //   4,3,2: Disposal Method
                        //   1    : User Input Flag
                        //   0    : Transparent Color Flag
                        let disposal = DisposalMethod::from_bits((gce_packed >> 2) & 0x07);
                        let user_input = (gce_packed & 0x02) != 0;
                        let trans_flag = (gce_packed & 0x01) != 0;
                        // Required Block Terminator after the 4 data bytes.
                        let term = c.read_u8()?;
                        if term != 0x00 {
                            return Err(GifError::invalid(format!(
                                "Graphic Control Extension Block Terminator = {term:#x} (expected 0x00)"
                            )));
                        }
                        // If there's already a pending GCE, the spec
                        // (§23.a) only allows one to precede a graphic.
                        // Be lenient: overwrite.
                        pending_gce = Some(GraphicControl {
                            disposal,
                            user_input,
                            delay_cs: delay,
                            transparent_index: if trans_flag { Some(trans_idx) } else { None },
                        });
                    }
                    LABEL_COMMENT => {
                        // §24.
                        let data = read_subblocks(&mut c)?;
                        blocks.push(GifBlock::Comment(CommentExtension { data }));
                    }
                    LABEL_PLAIN_TEXT => {
                        // §25. Block Size = 12 (fixed).
                        let bs = c.read_u8()?;
                        if bs != 12 {
                            return Err(GifError::invalid(format!(
                                "Plain Text Extension Block Size = {bs} (expected 12)"
                            )));
                        }
                        let grid_left = c.read_u16_le()?;
                        let grid_top = c.read_u16_le()?;
                        let grid_width = c.read_u16_le()?;
                        let grid_height = c.read_u16_le()?;
                        let cell_width = c.read_u8()?;
                        let cell_height = c.read_u8()?;
                        let fg = c.read_u8()?;
                        let bg = c.read_u8()?;
                        let data = read_subblocks(&mut c)?;
                        blocks.push(GifBlock::PlainText(PlainTextExtension {
                            grid_left,
                            grid_top,
                            grid_width,
                            grid_height,
                            cell_width,
                            cell_height,
                            fg_color_index: fg,
                            bg_color_index: bg,
                            data,
                            control: pending_gce.take(),
                        }));
                    }
                    LABEL_APPLICATION => {
                        // §26. Block Size = 11 (fixed).
                        let bs = c.read_u8()?;
                        if bs != 11 {
                            return Err(GifError::invalid(format!(
                                "Application Extension Block Size = {bs} (expected 11)"
                            )));
                        }
                        let header = c.read_bytes(11)?;
                        let mut identifier = [0u8; 8];
                        identifier.copy_from_slice(&header[0..8]);
                        let mut auth_code = [0u8; 3];
                        auth_code.copy_from_slice(&header[8..11]);
                        let data = read_subblocks(&mut c)?;
                        blocks.push(GifBlock::Application(ApplicationExtension {
                            identifier,
                            auth_code,
                            data,
                        }));
                    }
                    _ => {
                        // Unknown extension label. §F: read past it.
                        // The block is laid out as <label byte> <data
                        // sub-blocks ...> <terminator>. The label byte
                        // we already consumed; the remaining is just
                        // the sub-block sequence. (Some extensions
                        // have a fixed-size header before the
                        // sub-blocks, but generic skipping treats the
                        // header bytes as another sub-block, since it
                        // starts with a length byte. This works for
                        // all spec-compliant extensions.)
                        skip_subblocks(&mut c)?;
                    }
                }
            }
            other => {
                return Err(GifError::invalid(format!(
                    "unexpected block introducer {other:#x} at offset {}",
                    c.pos - 1
                )));
            }
        }
    }

    Ok(GifImage {
        version,
        width: lw,
        height: lh,
        color_resolution: color_res,
        global_palette_sorted: sort_flag,
        background_color_index: bg_index,
        pixel_aspect_ratio: par,
        global_palette,
        blocks,
    })
}

/// Parse one Image Descriptor + optional Local Color Table + Image
/// Data block (§20–§22). The Image Separator byte (0x2C) has already
/// been consumed.
fn parse_image_descriptor(c: &mut Cursor<'_>, control: Option<GraphicControl>) -> Result<GifFrame> {
    let left = c.read_u16_le()?;
    let top = c.read_u16_le()?;
    let width = c.read_u16_le()?;
    let height = c.read_u16_le()?;
    let packed = c.read_u8()?;

    // §20.c packed-fields layout:
    //   7  : Local Color Table Flag
    //   6  : Interlace Flag
    //   5  : Sort Flag
    //   4,3: Reserved
    //   2,1,0: Size of Local Color Table
    let lct_flag = (packed & 0x80) != 0;
    let interlace = (packed & 0x40) != 0;
    let sort = (packed & 0x20) != 0;
    let lct_size_bits = packed & 0x07;

    let local_palette = if lct_flag {
        let n_bytes = 3 * (1usize << (lct_size_bits as usize + 1));
        Some(c.read_bytes(n_bytes)?.to_vec())
    } else {
        None
    };

    // ---- Image Data (§22) ----
    let lzw_min_code_size = c.read_u8()?;
    let compressed = read_subblocks(c)?;
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| GifError::invalid("frame dimensions overflow usize"))?;
    let raw = lzw::decode(&compressed, lzw_min_code_size, pixel_count)?;
    if raw.len() < pixel_count {
        return Err(GifError::invalid(format!(
            "LZW decoded {} pixels, expected {pixel_count}",
            raw.len()
        )));
    }
    // The decoder may emit a trailing byte pair when the encoder's last
    // dictionary entry has length 1 — but in practice we trust the
    // declared dimensions and trim.
    let raw = if raw.len() > pixel_count {
        raw[..pixel_count].to_vec()
    } else {
        raw
    };

    let indices = if interlace {
        deinterlace(&raw, width as usize, height as usize)
    } else {
        raw
    };

    Ok(GifFrame {
        left,
        top,
        width,
        height,
        local_palette,
        local_palette_sorted: sort,
        interlaced: interlace,
        indices,
        control,
    })
}

/// Reorder pixel indices from the four-pass interlaced layout (§E)
/// into natural top-to-bottom row order.
///
/// On wire, an interlaced image's rows are concatenated in pass order:
/// * Pass 1: rows 0, 8, 16, …
/// * Pass 2: rows 4, 12, 20, …
/// * Pass 3: rows 2, 6, 10, …
/// * Pass 4: rows 1, 3, 5, …
pub(crate) fn deinterlace(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    let passes: &[(usize, usize)] = &[(0, 8), (4, 8), (2, 4), (1, 2)];
    let mut src_row = 0usize;
    for &(start, step) in passes {
        let mut r = start;
        while r < h {
            let dst_off = r * w;
            let src_off = src_row * w;
            out[dst_off..dst_off + w].copy_from_slice(&src[src_off..src_off + w]);
            r += step;
            src_row += 1;
        }
    }
    out
}

/// Cheap container probe — useful as a sniff helper. Returns `Some` if
/// the input begins with `GIF87a` or `GIF89a`.
pub fn probe(input: &[u8]) -> Option<GifVersion> {
    if input.len() < 6 || &input[..3] != SIGNATURE {
        return None;
    }
    match &input[3..6] {
        b"87a" => Some(GifVersion::Gif87a),
        b"89a" => Some(GifVersion::Gif89a),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_recognises_signatures() {
        assert_eq!(probe(b"GIF87a..."), Some(GifVersion::Gif87a));
        assert_eq!(probe(b"GIF89a..."), Some(GifVersion::Gif89a));
        assert_eq!(probe(b"GIF99a..."), None);
        assert_eq!(probe(b"PNG..."), None);
        assert_eq!(probe(b"G"), None);
    }

    #[test]
    fn deinterlace_reverses_interlace_order() {
        // 1x8 image — degenerate, but exercises the four passes.
        let interlaced = vec![10, 50, 30, 70, 20, 60, 40, 80];
        // Pass 1 rows 0,8,16: just row 0 -> 10
        // Pass 2 rows 4,12: just row 4 -> 50
        // Pass 3 rows 2,6: 30, 70
        // Pass 4 rows 1,3,5,7: 20,60,40,80
        // So natural order: 10, 20, 30, 60, 50, 40, 70, 80.
        let natural = deinterlace(&interlaced, 1, 8);
        assert_eq!(natural, vec![10, 20, 30, 60, 50, 40, 70, 80]);
    }
}
