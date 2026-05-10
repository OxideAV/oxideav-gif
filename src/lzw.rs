//! Variable-length-code LZW compression and decompression per
//! GIF89a Appendix F.
//!
//! ## Bit packing convention (Appendix F "BUILD 8-BIT BYTES")
//!
//! Codes are packed least-significant-bit first into a stream of 8-bit
//! bytes. The example layout from the spec is, with bytes shown in
//! ascending output order on the left:
//!
//! ```text
//!     byte 0: bbbaaaaa     (5 LSBs of code 'a' in low bits, then code 'b' starts)
//!     byte 1: dcccccbb     (… 'b' continues, then 'c', …)
//! ```
//!
//! i.e. code `a`'s low bits land in the low bits of byte 0; the remaining
//! bits of code `a` occupy the high bits of byte 0; the next code `b`
//! begins right above where `a` ended. Implementations therefore use a
//! u32 bit buffer and shift in new codes at `bit_count`, pulling 8-bit
//! bytes off the bottom.
//!
//! ## Code-width bump rule (Appendix F "COMPRESSION" item 4)
//!
//! > "The output codes are of variable length, starting at <code size>+1
//! >  bits per code, up to 12 bits per code. … Whenever the LZW code
//! >  value would exceed the current code length, the code length is
//! >  increased by one. The packing/unpacking of these codes must then
//! >  be altered to reflect the new code length."
//!
//! There is a long-standing implementation asymmetry that the spec
//! text does NOT address but that every interoperable GIF
//! encoder/decoder pair has converged on. With `code_size = 2`
//! (Clear=4, EOI=5, first dictionary slot = 6, initial output width =
//! 3 bits):
//!
//! * The **decoder** bumps width when its post-install `next_code`
//!   value reaches `(1 << width)`. So after installing slot 7 it
//!   would bump from width 3 to width 4.
//! * The **encoder** must compensate for the fact that the decoder
//!   lags it by one install (the decoder's install in step k mirrors
//!   the encoder's install in step k-1, because the decoder needs
//!   `first(string(C_k))` to materialise the new entry — info that
//!   only becomes available at step k). The encoder therefore bumps
//!   width when its post-install `next_code` reaches `(1 << width) +
//!   1`, i.e. one slot LATER than the decoder, so that the *next
//!   emission* — which is the same emission the decoder is about to
//!   read at the new width — uses the new wider encoding.
//!
//! Concretely with code_size=2 again: encoder installs slot 6
//! (next_code → 7, no bump), slot 7 (next_code → 8, no bump), slot 8
//! (next_code → 9 == (1<<3)+1, BUMP). The very next code the encoder
//! emits is at width 4. The decoder installs slot 6 (next_code → 7,
//! no bump), slot 7 (next_code → 8 == (1<<3), BUMP) — at which point
//! its next read is at width 4, matching the encoder's next emission.
//!
//! This continues until the next-to-be-assigned slot would be 4096 —
//! at that point per the spec cover-sheet "DEFERRED CLEAR CODE" note
//! we *stop adding* dictionary entries but keep emitting at width=12,
//! until the encoder decides to send a Clear.
//!
//! ## Decoder symmetry
//!
//! The decoder maintains its own dictionary using exactly the same
//! string-table semantics. The width-bump check on the decoder fires
//! after every install: if `next_code == (1 << width)` and
//! `width < 12`, bump before reading the next code. A Clear code
//! resets width back to `code_size + 1` and discards the dictionary
//! (the literal alphabet + Clear/EOI placeholders survive).

use crate::error::{GifError, Result};

/// Maximum LZW code width per spec §F item 4.
const MAX_CODE_WIDTH: u8 = 12;
/// 2^12 — one past the maximum legal dict slot.
const MAX_TABLE_SIZE: u16 = 1 << MAX_CODE_WIDTH;

/// Pack codes into a byte buffer, LSB-first, per Appendix F "BUILD 8-BIT BYTES".
///
/// Writers are expected to drive this with the exact code stream the
/// LZW compressor produces. Codes greater than 12 bits are rejected as
/// `InvalidData` — the spec hard-caps at 12.
struct BitWriter {
    out: Vec<u8>,
    /// Lower-order bits hold the next bits to be flushed.
    buf: u32,
    /// How many valid bits are currently sitting in `buf`.
    nbits: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            buf: 0,
            nbits: 0,
        }
    }

    fn write(&mut self, code: u16, width: u8) -> Result<()> {
        if width == 0 || width > MAX_CODE_WIDTH {
            return Err(GifError::invalid(format!(
                "LZW: illegal code width {width}"
            )));
        }
        if (code as u32) >> width != 0 {
            return Err(GifError::invalid(format!(
                "LZW: code {code} doesn't fit in {width} bits"
            )));
        }
        self.buf |= (code as u32) << self.nbits;
        self.nbits += width;
        while self.nbits >= 8 {
            self.out.push((self.buf & 0xff) as u8);
            self.buf >>= 8;
            self.nbits -= 8;
        }
        Ok(())
    }

    fn finish(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            // Flush any partial last byte. Per Appendix F packing
            // example, the unused high bits are simply zero.
            self.out.push((self.buf & 0xff) as u8);
        }
        self.out
    }
}

/// Pull codes out of a byte buffer, LSB-first.
struct BitReader<'a> {
    inp: &'a [u8],
    pos: usize,
    buf: u32,
    nbits: u8,
}

impl<'a> BitReader<'a> {
    fn new(inp: &'a [u8]) -> Self {
        Self {
            inp,
            pos: 0,
            buf: 0,
            nbits: 0,
        }
    }

    /// Read one code of `width` bits. Returns `None` if the input is
    /// exhausted before a full code is available — the decoder treats
    /// that as an end-of-stream signal (the spec also defines an
    /// explicit EOI code; both are accepted).
    fn read(&mut self, width: u8) -> Option<u16> {
        while self.nbits < width {
            if self.pos >= self.inp.len() {
                return None;
            }
            self.buf |= (self.inp[self.pos] as u32) << self.nbits;
            self.pos += 1;
            self.nbits += 8;
        }
        let mask: u32 = (1u32 << width) - 1;
        let code = (self.buf & mask) as u16;
        self.buf >>= width;
        self.nbits -= width;
        Some(code)
    }
}

/// Compress a sequence of palette indices into the GIF LZW byte stream
/// (the post-"BUILD 8-BIT BYTES" form — i.e. *before* the data sub-block
/// chunking layer applied by `chunk_into_subblocks` on the encoder
/// side).
///
/// `code_size` is the LZW Minimum Code Size byte from §22.c.i. Per
/// Appendix F "ESTABLISH CODE SIZE", values < 2 are illegal (and
/// "Because of some algorithmic constraints however, black & white
/// images which have one color bit must be indicated as having a code
/// size of 2"). Values > 8 are also illegal because color tables are
/// indexed with at most 8 bits.
pub fn encode(indices: &[u8], code_size: u8) -> Result<Vec<u8>> {
    if !(2..=8).contains(&code_size) {
        return Err(GifError::invalid(format!(
            "LZW Minimum Code Size {code_size} not in 2..=8"
        )));
    }
    let clear: u16 = 1 << code_size;
    let eoi: u16 = clear + 1;

    let mut out = BitWriter::new();
    // Initial code width per Appendix F "ESTABLISH CODE SIZE":
    // "compression codes must start out one bit longer".
    let mut width: u8 = code_size + 1;

    // Dictionary: slot -> (parent_code, last_byte). The 0..clear slots
    // are the literal alphabet, but we store only the dynamic part
    // (slot >= eoi+1) explicitly. Lookup of "string + byte" uses a
    // HashMap-equivalent index keyed by (string_code, byte) pair —
    // implemented as a flat Vec sized 4096 * 256 of u16 with 0xffff
    // meaning "absent". That's 2 MB worst-case which is too big for
    // small images, so we instead use a per-prefix sparse map: a
    // Vec<Vec<(u8, u16)>> of length up to MAX_TABLE_SIZE. Each
    // (byte, child_code) entry says "appending byte to this prefix
    // yields child_code". Lookup is linear within a prefix's child
    // list, but real palette images have very small per-prefix fanout
    // so this stays fast.
    let mut children: Vec<Vec<(u8, u16)>> = vec![Vec::new(); MAX_TABLE_SIZE as usize];
    let mut next_code: u16 = eoi + 1;

    out.write(clear, width)?;

    if indices.is_empty() {
        out.write(eoi, width)?;
        return Ok(out.finish());
    }

    // Validate inputs against the code size up front. Indices must fit
    // in `code_size` bits because the literal alphabet is exactly
    // 0..(1<<code_size) — sending an index >= clear would be ambiguous
    // with the Clear code on the wire.
    for &b in indices {
        if (b as u16) >= clear {
            return Err(GifError::invalid(format!(
                "pixel index {b} doesn't fit in code_size={code_size} bits"
            )));
        }
    }

    // Standard LZW: maintain the "current string" implicitly as a code
    // that points into the dictionary, plus the next byte. When
    // (current_code, byte) is in the dict, advance current_code; else
    // emit current_code, install (current_code, byte) -> next_code,
    // and start over with current_code = byte.
    let mut current: u16 = indices[0] as u16;
    for &b in &indices[1..] {
        // Search the children of `current` for byte `b`.
        let found = children[current as usize]
            .iter()
            .find(|(by, _)| *by == b)
            .map(|(_, c)| *c);
        if let Some(c) = found {
            current = c;
        } else {
            out.write(current, width)?;
            // Install new dict entry — but only if there's room.
            // Per the cover-sheet "DEFERRED CLEAR CODE" note, encoders
            // are NOT required to emit a Clear when the table fills;
            // they may simply keep emitting at width=12 against the
            // existing table. Choose that path here because the spec's
            // own deferred-clear text recommends it.
            if next_code < MAX_TABLE_SIZE {
                children[current as usize].push((b, next_code));
                next_code += 1;
                // Width-bump rule (§F item 4): when the *next-to-be-
                // assigned* code value would exceed what the current
                // width can express, bump the width *before* emitting
                // any further code. With width=5 we can emit codes
                // 0..31; once next_code becomes 32 we need width=6 for
                // the next emission.
                //
                // The de-facto historical convention (and the only one
                // that interoperates with libgif/ImageMagick/browser
                // decoders) bumps the encoder one slot LATER than a
                // naive reading of the spec text suggests: at
                // `next_code == (1 << width) + 1` (= just after
                // installing slot `(1 << width)` itself), not at
                // `(1 << width)`. This pairs with a decoder that
                // bumps at `next_code == (1 << width)`, and absorbs
                // the one-install lag between encoder and decoder
                // automatically (decoder installs lag encoder by one
                // because the decoder's install in step k uses
                // information from the read in step k+1's predecessor).
                if next_code == (1u16 << width) + 1 && width < MAX_CODE_WIDTH {
                    width += 1;
                }
            }
            current = b as u16;
        }
    }
    out.write(current, width)?;
    out.write(eoi, width)?;
    Ok(out.finish())
}

/// Decompress an LZW byte stream (the post-"BUILD 8-BIT BYTES" form,
/// i.e. with the data sub-block framing already removed) into the
/// pixel-index sequence.
///
/// `expected_pixels` is used as a safety cap to bound the output size
/// against malicious inputs that might emit far more data than the
/// declared image dimensions justify. Pass `usize::MAX` to disable.
pub fn decode(data: &[u8], code_size: u8, expected_pixels: usize) -> Result<Vec<u8>> {
    if !(2..=8).contains(&code_size) {
        return Err(GifError::invalid(format!(
            "LZW Minimum Code Size {code_size} not in 2..=8"
        )));
    }
    let clear: u16 = 1 << code_size;
    let eoi: u16 = clear + 1;

    // Dictionary entries 0..clear are literal bytes. eoi has no string.
    // For dynamic entries we store (prefix_code, first_byte) and the
    // length, which is enough to materialise the string by following
    // prefix links back to a literal.
    //
    // We carry an explicit `first_byte` (the first byte of the entry's
    // expansion) because the standard "K = first(W)" step in the
    // canonical Welch decoder needs it, and walking back the prefix
    // chain to find it on every iteration is needlessly quadratic.
    #[derive(Clone, Copy)]
    struct Entry {
        prefix: u16,
        last: u8,
        first: u8,
        len: u16,
    }
    let mut dict: Vec<Entry> = Vec::with_capacity(MAX_TABLE_SIZE as usize);
    // Pre-fill literal slots so we can index uniformly. Reserve TWO
    // placeholder slots after the literals — one at index `clear` for
    // the Clear code, one at index `eoi` for the End-of-Information
    // code — neither of which carries content but both of which need
    // to occupy a numbered slot so dynamic dict slots start at
    // `eoi + 1` and the slot indexing matches code values everywhere.
    for i in 0..clear {
        dict.push(Entry {
            prefix: 0,
            last: i as u8,
            first: i as u8,
            len: 1,
        });
    }
    // Slot `clear` placeholder.
    dict.push(Entry {
        prefix: 0,
        last: 0,
        first: 0,
        len: 0,
    });
    // Slot `eoi` placeholder.
    dict.push(Entry {
        prefix: 0,
        last: 0,
        first: 0,
        len: 0,
    });

    let mut width: u8 = code_size + 1;
    let mut next_code: u16 = eoi + 1;
    let mut out: Vec<u8> = Vec::new();
    let mut reader = BitReader::new(data);
    // `prev` = the code we just decoded. Per the canonical Welch decoder,
    // when we read a new code that's *just* been added to the table by
    // the encoder (i.e. next_code itself), the matching string is
    // `string(prev) + first_byte_of(string(prev))`. We need to remember
    // `prev` across iterations to emit that "fall-through" case.
    let mut prev: Option<u16> = None;
    // Reusable stack for materialising a string by walking prefix links.
    let mut stack: Vec<u8> = Vec::with_capacity(MAX_TABLE_SIZE as usize);

    while let Some(code) = reader.read(width) {
        if code == eoi {
            break;
        }
        if code == clear {
            // Reset state per §F item 1.
            dict.truncate((eoi + 1) as usize);
            next_code = eoi + 1;
            width = code_size + 1;
            prev = None;
            continue;
        }
        // Validate the code.
        if (code as usize) > dict.len() {
            return Err(GifError::invalid(format!(
                "LZW: code {code} > dict size {}",
                dict.len()
            )));
        }
        // Materialise string(code). If code == next_code (the
        // "fall-through" case), the entry isn't in the dict yet — its
        // string is string(prev) + first(prev).
        let (entry_first, _entry_len): (u8, u16) = if (code as usize) < dict.len() {
            // Already in dict. Walk prefix chain to expand.
            let mut walk = code;
            stack.clear();
            loop {
                let e = dict[walk as usize];
                if e.len == 1 {
                    stack.push(e.last);
                    break;
                }
                stack.push(e.last);
                walk = e.prefix;
            }
            let len = stack.len() as u16;
            // Stack is in reverse order — push back to front into out.
            let first = *stack.last().unwrap();
            for &b in stack.iter().rev() {
                out.push(b);
            }
            (first, len)
        } else if code == next_code {
            // Fall-through: emit string(prev) + first(prev).
            let p = prev.ok_or_else(|| {
                GifError::invalid("LZW: fall-through code at start of stream (no preceding code)")
            })?;
            // Materialise string(prev), record its first byte.
            let mut walk = p;
            stack.clear();
            loop {
                let e = dict[walk as usize];
                if e.len == 1 {
                    stack.push(e.last);
                    break;
                }
                stack.push(e.last);
                walk = e.prefix;
            }
            let first = *stack.last().unwrap();
            for &b in stack.iter().rev() {
                out.push(b);
            }
            out.push(first);
            (first, stack.len() as u16 + 1)
        } else {
            return Err(GifError::invalid(format!(
                "LZW: code {code} > dict next slot {next_code}"
            )));
        };

        // Add new entry to dict — but only if there's room. Per the
        // deferred-clear note, decoders MUST NOT mutate the table once
        // it's full; they keep reading at width=12 until a Clear comes
        // in. Also: no entry is added on the very first code after
        // Clear (no prev to derive from).
        //
        // The width-bump check on the decoder side fires when
        // post-install `next_code == (1 << width)`. The encoder
        // compensates for the encoder/decoder install lag by bumping
        // one slot LATER (`(1 << width) + 1`); see the module-level
        // doc comment.
        if let Some(p) = prev {
            if next_code < MAX_TABLE_SIZE {
                let p_entry = dict[p as usize];
                dict.push(Entry {
                    prefix: p,
                    last: entry_first,
                    first: p_entry.first,
                    len: p_entry.len + 1,
                });
                next_code += 1;
                if next_code == (1u16 << width) && width < MAX_CODE_WIDTH {
                    width += 1;
                }
            }
        }
        prev = Some(code);

        // Safety cap.
        if out.len() > expected_pixels {
            return Err(GifError::invalid(format!(
                "LZW: decoded output {} > expected_pixels {}",
                out.len(),
                expected_pixels
            )));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_alternating_pattern_round_trips() {
        // Drives a width bump from 3 to 4 while exercising the
        // alternating-pattern case that historically tripped up
        // LZW implementations with off-by-one bump timing.
        let input: Vec<u8> = vec![0, 1, 0, 1, 0, 1, 0, 1, 2, 3, 2, 3, 2, 3, 2, 3, 0, 2, 0, 2];
        let enc = encode(&input, 2).unwrap();
        let dec = decode(&enc, 2, input.len()).unwrap();
        assert_eq!(input, dec);
    }

    #[test]
    fn empty_input_round_trip() {
        let enc = encode(&[], 4).unwrap();
        // Encoder always emits a Clear and an EOI; with width=5 that's
        // 10 bits = 2 bytes.
        assert_eq!(enc.len(), 2);
        let dec = decode(&enc, 4, 0).unwrap();
        assert!(dec.is_empty());
    }

    #[test]
    fn roundtrip_random_indices() {
        // Deterministic LCG to avoid pulling in `rand`.
        let mut state: u32 = 0x1234_5678;
        let mut next = || {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (state >> 16) as u8
        };
        for code_size in 2..=8u8 {
            let max = 1u16 << code_size;
            let mut input: Vec<u8> = Vec::new();
            for _ in 0..2000 {
                input.push((next() as u16 % max) as u8);
            }
            let enc = encode(&input, code_size).unwrap();
            let dec = decode(&enc, code_size, input.len()).unwrap();
            assert_eq!(input, dec, "round-trip failed at code_size={code_size}");
        }
    }

    #[test]
    fn long_repetitive_input_drives_width_bumps() {
        // 8KiB of repeating pattern forces the dictionary to grow into
        // the 11- and 12-bit width regions at least once.
        let mut input = Vec::with_capacity(8192);
        for i in 0..8192u32 {
            input.push((i % 16) as u8);
        }
        let enc = encode(&input, 4).unwrap();
        let dec = decode(&enc, 4, input.len()).unwrap();
        assert_eq!(input, dec);
    }

    #[test]
    fn width_bump_at_table_full_holds_at_12() {
        // Drive enough pseudo-random data to force next_code past
        // MAX_TABLE_SIZE - 1 and confirm we don't blow up.
        let mut input = Vec::with_capacity(60_000);
        let mut state: u32 = 0xdead_beef;
        for _ in 0..60_000 {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            input.push(((state >> 24) & 0x07) as u8);
        }
        let enc = encode(&input, 3).unwrap();
        let dec = decode(&enc, 3, input.len()).unwrap();
        assert_eq!(input, dec);
    }

    #[test]
    fn rejects_pixel_above_code_size() {
        // code_size=2 means max literal is 3.
        assert!(encode(&[0, 1, 2, 3, 4], 2).is_err());
    }

    #[test]
    fn rejects_bad_code_size() {
        assert!(encode(&[0, 1], 1).is_err());
        assert!(encode(&[0, 1], 9).is_err());
        assert!(decode(&[0, 0], 1, 100).is_err());
        assert!(decode(&[0, 0], 9, 100).is_err());
    }

    #[test]
    fn bit_writer_lsb_first_layout() {
        // Spec example layout: with 5-bit codes, byte 0 = bbbaaaaa.
        // Encode codes a=0b00000, b=0b00001 — should yield byte 0b001_00000.
        let mut w = BitWriter::new();
        w.write(0b00000, 5).unwrap();
        w.write(0b00001, 5).unwrap();
        // Need to flush — only the partial last byte goes out on
        // finish, so write a third code to cleanly close one byte
        // boundary.
        w.write(0b00000, 5).unwrap();
        w.write(0b00000, 5).unwrap();
        let out = w.finish();
        // 4 codes * 5 bits = 20 bits = 3 bytes (with the last byte
        // having 4 unused high bits = zero).
        assert_eq!(out.len(), 3);
        // byte 0: bits 0..4 = code a (0), bits 5..7 = low 3 bits of
        // code b (0b001) — so 0b001_00000 = 0x20.
        assert_eq!(out[0], 0x20);
    }
}
