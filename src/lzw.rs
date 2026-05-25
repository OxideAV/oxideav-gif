//! Variable-Length-Code LZW per Appendix F of the GIF89a specification.
//!
//! # State machine derivation
//!
//! All transitions trace to specific lines of Appendix F:
//!
//! * "ESTABLISH CODE SIZE" — `min_code_size` is the initial code size
//!   byte stored in the LZW Minimum Code Size field (§22.c.i). The
//!   actual bit width used by the variable-length codes starts at
//!   `min_code_size + 1` ("compression codes must start out one bit
//!   longer"). The smallest legal value is 2 ("black & white images
//!   which have one color bit must be indicated as having a code size
//!   of 2").
//! * §F.1 — Clear code value = `2**min_code_size`. Encoders should
//!   output a Clear code as the first code of each image data stream;
//!   this implementation always does.
//! * §F.2 — End-of-Information value = Clear + 1. It is the last code
//!   the encoder emits.
//! * §F.3 — First available compression code = Clear + 2.
//! * §F.4 — Output codes start at `min_code_size + 1` bits and grow up
//!   to a maximum of 12 bits, defining a maximum code value of 4095.
//! * "BUILD 8-BIT BYTES" — codes are packed right-to-left and
//!   subsequently picked off 8 bits at a time. Equivalently, within
//!   each byte the lowest-numbered code's least-significant bits
//!   occupy the byte's least-significant positions.
//! * Cover Sheet ("DEFERRED CLEAR CODE IN LZW COMPRESSION") — once the
//!   table is full at the maximum code size, encoders may keep emitting
//!   12-bit codes against the existing table until they choose to send
//!   a Clear; decoders must keep using the existing table until a Clear
//!   arrives.
//!
//! # When the bit-width grows
//!
//! Appendix F.4: "Whenever the LZW code value would exceed the current
//! code length, the code length is increased by one." Reading
//! `would exceed` strictly: a value `v` exceeds a width of `w` bits the
//! moment `v >= 2^w`. So the width transition `w → w + 1` happens at the
//! point at which a code of value `2^w` first becomes possible to emit.
//!
//! Encoder side: codes 0..=(2^w − 1) fit in `w` bits. Once an entry is
//! assigned the index 2^w − 1, the encoder's next assignable index
//! becomes 2^w which would not fit. So the encoder bumps the width the
//! instant it has assigned an index equal to (2^w − 1).
//!
//! Decoder side: the decoder is intrinsically one entry behind the
//! encoder during in-loop emission — the decoder's first non-Clear
//! code adds no entry (there is no prior `prev_code`), so after K
//! received codes the decoder has assigned K − 1 entries while the
//! encoder has assigned K. The encoder's bump after assigning entry
//! 2^w − 1 therefore lines up with the decoder's bump after assigning
//! entry 2^w − 2. Equivalently the decoder bumps when its post-add
//! `next_code` reaches 2^w − 1, while the encoder bumps when its
//! post-add `next_code` reaches 2^w.
//!
//! The encoder mirrors the decoder's "one extra entry" at end-of-input
//! by performing one phantom dictionary extension during its final
//! flush (see [`encode`] — the post-loop block adds a `(prev, prev's
//! own first byte)` entry just like the decoder would on receipt of
//! `prev`). Without that phantom add the two sides would desync at the
//! exact moment the encoder's penultimate in-loop assignment lands on
//! `2^w − 2` (decoder bumps; encoder doesn't), and the encoder's EOI
//! would be written at the old width while the decoder reads at the
//! new one.
//!
//! At `w == 12` no further widening occurs; behaviour from then on
//! follows the deferred-clear rule on the cover sheet.

use crate::error::{Error, Result};

/// Smallest legal `min_code_size` value: §F "ESTABLISH CODE SIZE"
/// black & white clause.
pub const MIN_CODE_SIZE_FLOOR: u8 = 2;
/// Largest legal `min_code_size` value: a single pixel may not exceed
/// 8 bits in the format (palette indices fit in a byte, §22.a "Pixel
/// indices are in order of left to right and from top to bottom. Each
/// index must be within the range of the size of the active color
/// table, starting at 0").
pub const MIN_CODE_SIZE_CEIL: u8 = 8;
/// Maximum code bit width — §F.4 "up to 12 bits per code".
const MAX_CODE_WIDTH: u8 = 12;
/// Maximum number of entries in the dictionary, derived from
/// `MAX_CODE_WIDTH`: 2^12 = 4096.
const MAX_TABLE_SIZE: usize = 1 << MAX_CODE_WIDTH;

// ---------------------------------------------------------------------
// Bit-level I/O.
//
// §F "BUILD 8-BIT BYTES" — codes are packed right-to-left within a
// stream of bytes; equivalently, within a single byte, the bits of
// the next code occupy the least-significant positions.
// ---------------------------------------------------------------------

/// Right-to-left bit writer. Holds an accumulator and emits whole bytes
/// to its destination as they fill.
struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    nbits: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            acc: 0,
            nbits: 0,
        }
    }

    fn write(&mut self, code: u16, width: u8) {
        debug_assert!(width <= MAX_CODE_WIDTH);
        // Place the new code's bits ABOVE any partial bits already in
        // the accumulator: the in-byte ordering specified in
        // "BUILD 8-BIT BYTES" puts the previous code's tail in the LSBs.
        self.acc |= (code as u32) << self.nbits;
        self.nbits += width;
        while self.nbits >= 8 {
            self.out.push((self.acc & 0xFF) as u8);
            self.acc >>= 8;
            self.nbits -= 8;
        }
    }

    /// Flush the trailing partial byte (if any) and return the encoded
    /// bytes.
    fn finish(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            self.out.push((self.acc & 0xFF) as u8);
        }
        self.out
    }
}

/// Right-to-left bit reader. Pulls bytes lazily on demand.
struct BitReader<'a> {
    src: &'a [u8],
    pos: usize,
    acc: u32,
    nbits: u8,
}

impl<'a> BitReader<'a> {
    fn new(src: &'a [u8]) -> Self {
        Self {
            src,
            pos: 0,
            acc: 0,
            nbits: 0,
        }
    }

    /// Read the next `width` bits as a code. Returns `None` when the
    /// underlying byte stream is exhausted before `width` bits could be
    /// assembled — the caller treats that as truncation.
    fn read(&mut self, width: u8) -> Option<u16> {
        debug_assert!(width <= MAX_CODE_WIDTH);
        while self.nbits < width {
            if self.pos >= self.src.len() {
                return None;
            }
            self.acc |= (self.src[self.pos] as u32) << self.nbits;
            self.pos += 1;
            self.nbits += 8;
        }
        let mask = (1u32 << width) - 1;
        let code = (self.acc & mask) as u16;
        self.acc >>= width;
        self.nbits -= width;
        Some(code)
    }
}

// ---------------------------------------------------------------------
// Encoder.
// ---------------------------------------------------------------------

/// Encode `pixels` (palette indices) under the variable-length-code
/// LZW scheme of Appendix F. Returns the raw compressed byte stream
/// excluding the leading LZW Minimum Code Size byte and excluding the
/// surrounding sub-block framing (§22.c.i and §15 are applied by the
/// caller).
pub fn encode(min_code_size: u8, pixels: &[u8]) -> Result<Vec<u8>> {
    if !(MIN_CODE_SIZE_FLOOR..=MIN_CODE_SIZE_CEIL).contains(&min_code_size) {
        return Err(Error::InvalidInput(format!(
            "LZW minimum code size {min_code_size} outside [2, 8]"
        )));
    }

    let clear_code: u16 = 1 << min_code_size;
    let eoi_code: u16 = clear_code + 1;
    let first_entry: u16 = clear_code + 2;
    let initial_width: u8 = min_code_size + 1;

    let mut writer = BitWriter::new();

    // §F.1 recommendation — output Clear as the first code.
    writer.write(clear_code, initial_width);

    // The dictionary is keyed (prefix_code, next_byte) → entry_code.
    // Single-byte strings 0..clear_code are implicit identity entries
    // and never stored explicitly.
    //
    // A flat Vec indexed by `(prefix as usize) * 256 + (byte as usize)`
    // keeps the inner loop branch-free and cache-friendly. 0 means
    // "absent"; entry codes are stored as `code + 1` so we can
    // distinguish absent from entry-zero.
    let mut table: Vec<u16> = vec![0u16; MAX_TABLE_SIZE * 256];
    let mut next_code: u16 = first_entry;
    let mut width: u8 = initial_width;

    let mut iter = pixels.iter().copied();
    let mut prev: u16 = match iter.next() {
        Some(b) => {
            if (b as u16) >= clear_code {
                return Err(Error::InvalidInput(format!(
                    "pixel index {b} >= 2^min_code_size ({clear_code})"
                )));
            }
            b as u16
        }
        None => {
            // Empty raster: emit only EOI after the Clear we already wrote.
            writer.write(eoi_code, width);
            return Ok(writer.finish());
        }
    };

    for byte in iter {
        if (byte as u16) >= clear_code {
            return Err(Error::InvalidInput(format!(
                "pixel index {byte} >= 2^min_code_size ({clear_code})"
            )));
        }
        let key = (prev as usize) * 256 + (byte as usize);
        let slot = table[key];
        if slot != 0 {
            // The string `prev + byte` already has a code — extend.
            prev = slot - 1;
        } else {
            // Emit the code for `prev` and learn the new pattern.
            writer.write(prev, width);

            if next_code < MAX_TABLE_SIZE as u16 {
                table[key] = next_code + 1;
                // Width-bump rule (Appendix F.4) — see the module-level
                // doc-comment for the derivation. Compare against
                // `1 << width` after the increment so that the next
                // emitted code is read by the decoder at the new width.
                let assigned = next_code;
                next_code += 1;
                if assigned == (1u16 << width) - 1 && width < MAX_CODE_WIDTH {
                    width += 1;
                }
                // Cover-sheet "deferred clear": at next_code ==
                // MAX_TABLE_SIZE we stop assigning new codes but keep
                // emitting at MAX_CODE_WIDTH against the existing
                // table. The encoder is free to send a Clear at any
                // time; this implementation simply keeps using the
                // table.
            }

            prev = byte as u16;
        }
    }

    // Final pending prefix. The decoder, on receiving this code, will
    // add a dictionary entry (prev_code + first_byte_of_this) and may
    // bump its width when that assignment lands on `2^W − 2`. Mirror
    // that here so the encoder's `width` advances in lock-step;
    // otherwise the EOI emission below would go out at the old width
    // while the decoder reads at the new one. We do not actually need
    // the dictionary entry's contents (no further compression
    // happens), only the bump-trigger side-effect.
    writer.write(prev, width);
    if next_code < MAX_TABLE_SIZE as u16
        && next_code == (1u16 << width) - 1
        && width < MAX_CODE_WIDTH
    {
        width += 1;
    }

    // §F.2 — EOI must be the last code output for an image.
    writer.write(eoi_code, width);

    Ok(writer.finish())
}

// ---------------------------------------------------------------------
// Decoder.
// ---------------------------------------------------------------------

/// Decode the raw LZW byte stream back into palette indices. The input
/// is the compressed payload after the LZW Minimum Code Size byte and
/// after the sub-block framing has been stripped.
///
/// `expected_pixels` caps the output buffer; decoding stops at the
/// first EOI even if fewer pixels were produced (the spec leaves
/// truncation handling to the application).
///
/// The capacity hint passed to the output `Vec` is clamped against a
/// conservative upper bound derived from the compressed input length
/// (every input byte can plausibly yield at most `MAX_TABLE_SIZE`
/// output bytes in pathological cases). Without the clamp a hostile
/// `width × height` field can trick the decoder into pre-reserving
/// gigabytes of memory before any code is read — a trivial DoS
/// regardless of how much actual LZW payload is present.
pub fn decode(min_code_size: u8, src: &[u8], expected_pixels: usize) -> Result<Vec<u8>> {
    if !(MIN_CODE_SIZE_FLOOR..=MIN_CODE_SIZE_CEIL).contains(&min_code_size) {
        return Err(Error::InvalidData(format!(
            "LZW minimum code size {min_code_size} outside [2, 8]"
        )));
    }

    let clear_code: u16 = 1 << min_code_size;
    let eoi_code: u16 = clear_code + 1;
    let first_entry: u16 = clear_code + 2;
    let initial_width: u8 = min_code_size + 1;

    // Each table entry stores its decoded byte sequence as
    // (parent_code, last_byte). Single-byte entries 0..clear_code use
    // parent == NONE.
    const NONE: u16 = u16::MAX;
    let mut parent: Vec<u16> = vec![NONE; MAX_TABLE_SIZE];
    let mut suffix: Vec<u8> = vec![0u8; MAX_TABLE_SIZE];
    for i in 0..clear_code {
        suffix[i as usize] = i as u8;
    }
    let reset_next_code = first_entry;

    let mut next_code: u16 = reset_next_code;
    let mut width: u8 = initial_width;
    let mut reader = BitReader::new(src);

    // Cap the up-front reservation: see the function-level doc for the
    // DoS rationale. `MAX_TABLE_SIZE` (4096) is the largest substring an
    // LZW dictionary entry can hold, so `src.len() * MAX_TABLE_SIZE` is
    // a generous-but-bounded ceiling on legitimate output.
    let cap_ceiling = src.len().saturating_mul(MAX_TABLE_SIZE);
    let initial_cap = expected_pixels.min(cap_ceiling);
    let mut output: Vec<u8> = Vec::with_capacity(initial_cap);
    // Scratch buffer used to materialise an entry into bytes (entries
    // are stored as a parent-pointer chain that we walk in reverse).
    let mut scratch: Vec<u8> = Vec::with_capacity(MAX_TABLE_SIZE);

    let mut prev_code: Option<u16> = None;

    loop {
        let Some(code) = reader.read(width) else {
            // End of compressed bytes without an explicit EOI: spec
            // §F.2 says EOI "must be the last code output by the
            // encoder", so the absence is a defect. Treat it as such.
            return Err(Error::InvalidData(
                "LZW stream ended before End-of-Information code".into(),
            ));
        };

        if code == clear_code {
            // §F.1 — reset table state.
            next_code = reset_next_code;
            width = initial_width;
            prev_code = None;
            continue;
        }
        if code == eoi_code {
            // §F.2 — terminator.
            return Ok(output);
        }

        // Materialise the entry's byte string into `scratch`. Walk the
        // parent chain in reverse, then push it in forward order.
        let entry_first_byte: u8;
        scratch.clear();
        if code < next_code {
            // Known entry.
            let mut walk = code;
            loop {
                scratch.push(suffix[walk as usize]);
                if parent[walk as usize] == NONE {
                    break;
                }
                walk = parent[walk as usize];
            }
            entry_first_byte = *scratch.last().unwrap();
        } else if code == next_code {
            // KwKwK case (§F implicit, reasoned below):
            //
            // The encoder may emit a code that has not yet been added
            // to the decoder's dictionary in exactly one situation:
            // the next entry it's about to define is `prev + first(prev)`.
            // The decoder reconstructs that by extending `prev`'s
            // string with its own first byte.
            //
            // This is required because the encoder added the entry at
            // the moment it emitted `prev`, while the decoder only
            // knows the entry's existence one code later — yet the
            // encoder may emit that fresh entry as the very next code.
            let Some(prev) = prev_code else {
                return Err(Error::InvalidData(
                    "first non-Clear LZW code references uninitialised prefix".into(),
                ));
            };
            // Walk prev's chain to find prev's FIRST byte.
            let mut walk = prev;
            while parent[walk as usize] != NONE {
                walk = parent[walk as usize];
            }
            let prev_first = suffix[walk as usize];
            // Materialise prev's string, then append its first byte.
            let mut walk = prev;
            loop {
                scratch.push(suffix[walk as usize]);
                if parent[walk as usize] == NONE {
                    break;
                }
                walk = parent[walk as usize];
            }
            scratch.reverse();
            scratch.push(prev_first);
            entry_first_byte = scratch[0];
            // Restore reverse order so the unified emit path below works.
            scratch.reverse();
        } else {
            return Err(Error::InvalidData(format!(
                "LZW code {code} exceeds dictionary size {next_code}"
            )));
        }

        // Emit in forward order.
        for &b in scratch.iter().rev() {
            if output.len() >= expected_pixels {
                // Decoder may have produced "trailing" output beyond
                // the image area — accept and stop. The spec allows
                // the encoder to over-produce in deferred-clear mode.
                break;
            }
            output.push(b);
        }

        // Add `prev + entry_first_byte` as the next entry, mirroring
        // the encoder's table growth (lagged by one — see module docs).
        if let Some(p) = prev_code {
            if next_code < MAX_TABLE_SIZE as u16 {
                parent[next_code as usize] = p;
                suffix[next_code as usize] = entry_first_byte;
                let assigned = next_code;
                next_code += 1;
                // Decoder bump rule: assigned == 2^W − 2 (one entry
                // earlier than the encoder's own bump trigger). See the
                // module-level `When the bit-width grows` derivation —
                // the decoder lags the encoder by one assignment in
                // normal flow because the decoder's first non-Clear
                // code adds no entry; so a decoder threshold of
                // `2^W − 2` lines up with the encoder's `2^W − 1`.
                let threshold = (1u16 << width).wrapping_sub(2);
                if assigned == threshold && width < MAX_CODE_WIDTH {
                    width += 1;
                }
            }
            // else: deferred-clear regime — keep decoding without
            // growing the table until a Clear arrives.
        }
        prev_code = Some(code);
    }
}

// ---------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_small_palette() {
        // 4-color palette → min_code_size = 2 (Appendix F floor).
        let pixels: Vec<u8> = (0..1000u32).map(|i| (i % 4) as u8).collect();
        let encoded = encode(2, &pixels).unwrap();
        let decoded = decode(2, &encoded, pixels.len()).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn known_good_4color_byte_pattern() {
        // Hand-derived fixture: pixels [0,1,2,3]x4 at min_code_size=2.
        //
        // Emission sequence per the encoder state machine
        // (Appendix F):
        //   Clear=4 (w=3), 0 (w=3), 1 (w=3),
        //   2 (w=4), 3, 6, 8, 10, 9, 7, 3, EOI=5 (all w=4)
        //
        // Width bump after assigning entry 7 (= 2^3 − 1).
        // Bits packed right-to-left per "BUILD 8-BIT BYTES".
        // Total bits = 3*3 + 9*4 = 45 → 6 bytes (last has 3 trailing zero bits).
        const EXPECTED: [u8; 6] = [0x44, 0x64, 0x0C, 0x35, 0x6F, 0x0A];
        let pixels: Vec<u8> = vec![0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3];
        let encoded = encode(2, &pixels).unwrap();
        assert_eq!(encoded.as_slice(), &EXPECTED);
        let decoded = decode(2, &encoded, pixels.len()).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn roundtrip_8bit() {
        // 256-colour palette → min_code_size = 8.
        let pixels: Vec<u8> = (0..5000u32)
            .map(|i| (i.wrapping_mul(31) & 0xFF) as u8)
            .collect();
        let encoded = encode(8, &pixels).unwrap();
        let decoded = decode(8, &encoded, pixels.len()).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn roundtrip_runs_force_kwkwk() {
        // Long monotone runs trigger the KwKwK pattern early.
        let pixels = vec![3u8; 3000];
        let encoded = encode(2, &pixels).unwrap();
        let decoded = decode(2, &encoded, pixels.len()).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn roundtrip_random_4bit() {
        // 16-colour palette — exercises width bumps 5 → 6 → 7 → 8 → 9.
        let mut pixels = Vec::with_capacity(20_000);
        // Deterministic xorshift so tests stay reproducible without a
        // crate-level RNG dependency.
        let mut state: u32 = 0x1234_5678;
        for _ in 0..20_000 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            pixels.push((state & 0x0F) as u8);
        }
        let encoded = encode(4, &pixels).unwrap();
        let decoded = decode(4, &encoded, pixels.len()).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn roundtrip_forces_table_overflow() {
        // 80_000 pseudo-random pixels at min_code_size=8 saturates the
        // 4096-entry dictionary and exercises the deferred-clear path
        // (encoder side) and table-full path (decoder side).
        let mut pixels = Vec::with_capacity(80_000);
        let mut state: u32 = 0xDEAD_BEEF;
        for _ in 0..80_000 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            pixels.push((state & 0xFF) as u8);
        }
        let encoded = encode(8, &pixels).unwrap();
        let decoded = decode(8, &encoded, pixels.len()).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn empty_pixels_emits_only_clear_and_eoi() {
        let encoded = encode(2, &[]).unwrap();
        // First 3 bits = Clear (0b100 with min_code_size=2 ⇒ Clear=4),
        // next 3 bits = EOI (5 = 0b101). Packed right-to-left:
        // bits = ...101_100 = 0b101_100 = 0x2C.
        // Width=3 bits, total 6 bits → one byte 0b00_101_100 = 0x2C.
        assert_eq!(encoded, vec![0x2C]);
    }

    #[test]
    fn rejects_pixel_above_palette() {
        // min_code_size=2 ⇒ legal pixel range is 0..=3.
        let result = encode(2, &[0, 1, 5]);
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn rejects_min_code_size_out_of_range() {
        assert!(matches!(encode(1, &[0]), Err(Error::InvalidInput(_))));
        assert!(matches!(encode(9, &[0]), Err(Error::InvalidInput(_))));
        assert!(matches!(decode(1, &[], 0), Err(Error::InvalidData(_))));
        assert!(matches!(decode(9, &[], 0), Err(Error::InvalidData(_))));
    }

    #[test]
    fn lzw_run_at_8color_endpoint_roundtrips() {
        // Triggered by the encoder optimisation tests at min_code_size=3
        // with a 16-pixel monochrome raster. Ensures the decoder bump
        // rule does not over-shoot when the encoder finishes mid-width.
        let pixels = vec![0u8; 16];
        let encoded = encode(3, &pixels).unwrap();
        let decoded = decode(3, &encoded, pixels.len()).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn truncated_stream_errors_cleanly() {
        let pixels = vec![0u8, 1, 2, 3, 0, 1, 2, 3];
        let mut encoded = encode(2, &pixels).unwrap();
        encoded.pop();
        let result = decode(2, &encoded, pixels.len());
        // Either we ran out of bits before EOI, or the stream
        // mis-decoded. Both are recoverable errors, never panics.
        assert!(matches!(result, Err(Error::InvalidData(_))) || result.is_ok());
    }

    #[test]
    fn decode_caps_upfront_allocation_by_input_length() {
        // Regression: a hostile §20.c `width × height` (e.g. 65535×65535
        // ≈ 4 GiB) used to be passed straight to `Vec::with_capacity`,
        // OOM-aborting the process before any LZW byte was read. The
        // decoder now clamps the up-front reservation against
        // `src.len() * MAX_TABLE_SIZE` so an empty-or-short compressed
        // payload cannot trick the allocator into reserving gigabytes.
        let huge_expected: usize = (u32::MAX as usize).saturating_mul(4);
        // Empty input + huge expected_pixels: must error, not OOM.
        let r = decode(2, &[], huge_expected);
        assert!(matches!(r, Err(Error::InvalidData(_))));
        // Same with a single byte of compressed data.
        let r = decode(2, &[0xFF], huge_expected);
        assert!(matches!(r, Err(Error::InvalidData(_))) || r.is_ok());
    }
}
