//! GIF87a / GIF89a top-level decoder.
//!
//! The implementation walks the byte stream against the grammar in
//! Appendix B and produces the in-memory tree defined in
//! [`crate::image`].

use crate::error::{Error, Result};
use crate::image::{
    Application, Block, DisposalMethod, Frame, GifImage, GraphicControl, PlainText, Rgb, Version,
};
use crate::interlace::interlace_row_order;
use crate::lzw;

/// Byte values referenced by the spec.
mod label {
    /// §17 Header byte 0..=2 — `b"GIF"`.
    pub const SIGNATURE: &[u8; 3] = b"GIF";
    /// §20.c.i — Image Descriptor introducer.
    pub const IMAGE_SEPARATOR: u8 = 0x2C;
    /// §27 — Trailer byte.
    pub const TRAILER: u8 = 0x3B;
    /// §23.c.i (and friends) — extension introducer.
    pub const EXTENSION_INTRODUCER: u8 = 0x21;
    /// §23.c.ii — Graphic Control label.
    pub const GRAPHIC_CONTROL: u8 = 0xF9;
    /// §24.c.ii — Comment label.
    pub const COMMENT: u8 = 0xFE;
    /// §25.c.ii — Plain Text label.
    pub const PLAIN_TEXT: u8 = 0x01;
    /// §26.c.ii — Application label.
    pub const APPLICATION: u8 = 0xFF;
}

/// Read a GIF Data Stream from `bytes` and return it as a [`GifImage`].
pub fn decode(bytes: &[u8]) -> Result<GifImage> {
    let mut p = Parser::new(bytes);
    let version = p.read_header()?;
    let (screen_width, screen_height, packed, background_index, pixel_aspect_ratio) =
        p.read_logical_screen_descriptor()?;
    // §18.c "<Packed Fields>" — bit layout:
    //  bit 7      : Global Color Table Flag
    //  bits 6..4  : Color Resolution
    //  bit 3      : Sort Flag
    //  bits 2..0  : Size of Global Color Table
    let global_table_flag = (packed & 0b1000_0000) != 0;
    let color_resolution = (packed >> 4) & 0b0000_0111;
    let global_palette_sorted = (packed & 0b0000_1000) != 0;
    let global_table_size_bits = packed & 0b0000_0111;
    let global_palette = if global_table_flag {
        Some(p.read_color_table(global_table_size_bits)?)
    } else {
        None
    };

    let mut blocks = Vec::new();
    // The Graphic Control Extension's scope is "the first graphic
    // rendering block to follow" (§23.d). Buffer the most recent GCE
    // here and attach it to the next image / plain text we see.
    let mut pending_gce: Option<GraphicControl> = None;

    loop {
        let intro = p.peek_byte()?;
        match intro {
            label::TRAILER => {
                p.advance(1);
                break;
            }
            label::IMAGE_SEPARATOR => {
                let frame = p.read_image_descriptor_and_data(pending_gce.take())?;
                blocks.push(Block::Image(frame));
            }
            label::EXTENSION_INTRODUCER => {
                p.advance(1);
                let label = p.read_byte()?;
                match label {
                    label::GRAPHIC_CONTROL => {
                        let gce = p.read_graphic_control_extension()?;
                        // §23 — at most one GCE may precede a graphic
                        // rendering block. A second consecutive GCE is
                        // invalid; drop the previous one with a warning
                        // by silently overwriting (decoders must be
                        // robust against malformed streams).
                        pending_gce = Some(gce);
                    }
                    label::COMMENT => {
                        let data = p.read_data_sub_blocks()?;
                        blocks.push(Block::Comment(data));
                    }
                    label::PLAIN_TEXT => {
                        let params = p.read_plain_text_extension()?;
                        blocks.push(Block::PlainText {
                            params,
                            graphic_control: pending_gce.take(),
                        });
                    }
                    label::APPLICATION => {
                        let app = p.read_application_extension()?;
                        blocks.push(Block::Application(app));
                    }
                    other => {
                        return Err(Error::Unsupported(format!(
                            "unknown extension label 0x{other:02X}"
                        )));
                    }
                }
            }
            other => {
                return Err(Error::InvalidData(format!(
                    "expected block introducer, got byte 0x{other:02X}"
                )));
            }
        }
    }

    Ok(GifImage {
        version,
        screen_width,
        screen_height,
        color_resolution,
        global_palette_sorted,
        background_index,
        pixel_aspect_ratio,
        global_palette,
        blocks,
    })
}

// ---------------------------------------------------------------------
// Parser internals.
// ---------------------------------------------------------------------

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a [u8]) -> Self {
        Self { src, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.src.len() - self.pos
    }

    fn need(&self, n: usize) -> Result<()> {
        if self.remaining() < n {
            Err(Error::UnexpectedEof)
        } else {
            Ok(())
        }
    }

    fn advance(&mut self, n: usize) {
        self.pos += n;
    }

    fn peek_byte(&self) -> Result<u8> {
        self.need(1)?;
        Ok(self.src[self.pos])
    }

    fn read_byte(&mut self) -> Result<u8> {
        self.need(1)?;
        let b = self.src[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn read_u16_le(&mut self) -> Result<u16> {
        // Appendix D: "multi-byte numeric fields are ordered with the
        // Least Significant Byte first."
        self.need(2)?;
        let lo = self.src[self.pos] as u16;
        let hi = self.src[self.pos + 1] as u16;
        self.pos += 2;
        Ok(lo | (hi << 8))
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.need(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(&self.src[self.pos..self.pos + N]);
        self.pos += N;
        Ok(out)
    }

    fn read_slice(&mut self, n: usize) -> Result<&'a [u8]> {
        self.need(n)?;
        let s = &self.src[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn read_header(&mut self) -> Result<Version> {
        // §17.c: 3 signature bytes + 3 version bytes.
        let signature: [u8; 3] = self.read_array()?;
        if &signature != label::SIGNATURE {
            return Err(Error::InvalidData(format!(
                "header signature is not 'GIF': {signature:?}"
            )));
        }
        let version_bytes: [u8; 3] = self.read_array()?;
        match &version_bytes {
            b"87a" => Ok(Version::Gif87a),
            b"89a" => Ok(Version::Gif89a),
            other => Err(Error::Unsupported(format!(
                "unsupported version field {other:?}"
            ))),
        }
    }

    fn read_logical_screen_descriptor(&mut self) -> Result<(u16, u16, u8, u8, u8)> {
        // §18.c: width, height, packed, bg_index, aspect.
        let w = self.read_u16_le()?;
        let h = self.read_u16_le()?;
        let packed = self.read_byte()?;
        let bg = self.read_byte()?;
        let aspect = self.read_byte()?;
        Ok((w, h, packed, bg, aspect))
    }

    fn read_color_table(&mut self, size_bits: u8) -> Result<Vec<Rgb>> {
        // §19 / §21: number of bytes = 3 * 2^(size_bits + 1).
        let count = 1usize << (size_bits as u32 + 1);
        let mut table = Vec::with_capacity(count);
        for _ in 0..count {
            let r = self.read_byte()?;
            let g = self.read_byte()?;
            let b = self.read_byte()?;
            table.push(Rgb::new(r, g, b));
        }
        Ok(table)
    }

    fn read_data_sub_blocks(&mut self) -> Result<Vec<u8>> {
        // §15: each sub-block is `len_byte | data*len`. Sequence is
        // terminated by the §16 zero-length Block Terminator.
        let mut out = Vec::new();
        loop {
            let n = self.read_byte()?;
            if n == 0 {
                return Ok(out);
            }
            let chunk = self.read_slice(n as usize)?;
            out.extend_from_slice(chunk);
        }
    }

    fn read_image_descriptor_and_data(
        &mut self,
        graphic_control: Option<GraphicControl>,
    ) -> Result<Frame> {
        // Consume the image separator.
        let sep = self.read_byte()?;
        debug_assert_eq!(sep, label::IMAGE_SEPARATOR);

        // §20.c: left, top, width, height, packed.
        let left = self.read_u16_le()?;
        let top = self.read_u16_le()?;
        let width = self.read_u16_le()?;
        let height = self.read_u16_le()?;
        let packed = self.read_byte()?;

        // §20.c "<Packed Fields>" — bit layout:
        //  bit 7      : Local Color Table Flag
        //  bit 6      : Interlace Flag
        //  bit 5      : Sort Flag
        //  bits 4..3  : Reserved
        //  bits 2..0  : Size of Local Color Table
        let local_flag = (packed & 0b1000_0000) != 0;
        let interlaced = (packed & 0b0100_0000) != 0;
        let palette_sorted = (packed & 0b0010_0000) != 0;
        let local_size_bits = packed & 0b0000_0111;

        let local_palette = if local_flag {
            Some(self.read_color_table(local_size_bits)?)
        } else {
            None
        };

        // §22 — LZW Minimum Code Size byte, then sub-block sequence.
        let min_code_size = self.read_byte()?;
        let raw = self.read_data_sub_blocks()?;

        let pixels_expected = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| Error::InvalidData("image width × height overflows".into()))?;

        let raw_indices = lzw::decode(min_code_size, &raw, pixels_expected)?;
        if raw_indices.len() != pixels_expected {
            // Spec gives no recovery contract: pad with the background
            // (index 0) is risky because index 0 may not exist in the
            // active palette. Refuse instead.
            return Err(Error::InvalidData(format!(
                "LZW produced {} pixels, expected {}",
                raw_indices.len(),
                pixels_expected
            )));
        }

        let indices = if interlaced {
            // Appendix E — re-shuffle decoded rows into top-to-bottom
            // order before exposing them.
            let mut out = vec![0u8; pixels_expected];
            let row_order = interlace_row_order(height);
            let row_bytes = width as usize;
            for (storage_row, &target_row) in row_order.iter().enumerate() {
                let src_off = storage_row * row_bytes;
                let dst_off = (target_row as usize) * row_bytes;
                out[dst_off..dst_off + row_bytes]
                    .copy_from_slice(&raw_indices[src_off..src_off + row_bytes]);
            }
            out
        } else {
            raw_indices
        };

        Ok(Frame {
            left,
            top,
            width,
            height,
            local_palette,
            palette_sorted,
            interlaced,
            indices,
            graphic_control,
        })
    }

    fn read_graphic_control_extension(&mut self) -> Result<GraphicControl> {
        // §23.c: block_size byte (must be 4) + packed + delay (LE u16)
        // + transparent index + zero block terminator.
        let block_size = self.read_byte()?;
        if block_size != 4 {
            return Err(Error::InvalidData(format!(
                "Graphic Control Extension block size {block_size} != 4"
            )));
        }
        let packed = self.read_byte()?;
        let delay = self.read_u16_le()?;
        let transparent_index = self.read_byte()?;
        let term = self.read_byte()?;
        if term != 0 {
            return Err(Error::InvalidData(format!(
                "Graphic Control Extension missing block terminator (got 0x{term:02X})"
            )));
        }
        // §23.c "<Packed Fields>" — bit layout:
        //  bits 7..5 : Reserved
        //  bits 4..2 : Disposal Method
        //  bit 1     : User Input Flag
        //  bit 0     : Transparency Flag
        let disposal_bits = (packed >> 2) & 0b0000_0111;
        let user_input = (packed & 0b0000_0010) != 0;
        let transparency_flag = (packed & 0b0000_0001) != 0;
        Ok(GraphicControl {
            disposal: DisposalMethod::from_bits(disposal_bits),
            user_input,
            transparent_index: if transparency_flag {
                Some(transparent_index)
            } else {
                None
            },
            delay_centis: delay,
        })
    }

    fn read_plain_text_extension(&mut self) -> Result<PlainText> {
        // §25.c: block_size (12), then 12 bytes of fixed parameters,
        // then sub-block payload, then terminator.
        let block_size = self.read_byte()?;
        if block_size != 12 {
            return Err(Error::InvalidData(format!(
                "Plain Text Extension block size {block_size} != 12"
            )));
        }
        let left = self.read_u16_le()?;
        let top = self.read_u16_le()?;
        let width = self.read_u16_le()?;
        let height = self.read_u16_le()?;
        let cell_w = self.read_byte()?;
        let cell_h = self.read_byte()?;
        let fg = self.read_byte()?;
        let bg = self.read_byte()?;
        let text = self.read_data_sub_blocks()?;
        Ok(PlainText {
            left,
            top,
            width,
            height,
            cell_width: cell_w,
            cell_height: cell_h,
            fg_color_index: fg,
            bg_color_index: bg,
            text,
        })
    }

    fn read_application_extension(&mut self) -> Result<Application> {
        // §26.c: block_size (11), 8-byte identifier, 3-byte auth code,
        // sub-block payload, terminator.
        let block_size = self.read_byte()?;
        if block_size != 11 {
            return Err(Error::InvalidData(format!(
                "Application Extension block size {block_size} != 11"
            )));
        }
        let identifier: [u8; 8] = self.read_array()?;
        let auth_code: [u8; 3] = self.read_array()?;
        let data = self.read_data_sub_blocks()?;
        Ok(Application {
            identifier,
            auth_code,
            data,
        })
    }
}
