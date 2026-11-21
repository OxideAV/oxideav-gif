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
//! code length, the code length is increased by one." The *code value*
//! that grows is the index of the dictionary entry just assigned: once
//! an entry with index 2^w exists, a w-bit field can no longer name
//! every entry, so the width becomes w + 1 for every code that follows.
//!
//! Encoder side: the encoder emits a code and then assigns the next
//! entry. It bumps the width the instant the assigned index equals
//! 2^w — the code it just emitted went out at w bits, the next one goes
//! out at w + 1.
//!
//! Decoder side: the decoder is intrinsically one entry behind the
//! encoder — its first non-Clear code adds no entry (there is no prior
//! `prev_code`), so after K received codes the decoder has assigned
//! K − 1 entries while the encoder has assigned K. The encoder's bump
//! after assigning entry 2^w therefore lines up with the decoder's bump
//! after assigning entry 2^w − 1: the decoder reads a code, assigns an
//! entry, and widens when that entry's index is 2^w − 1 (equivalently,
//! when its post-add `next_code` reaches 2^w). This is the rule every
//! interoperating GIF reader applies; the stream written under it was
//! verified against two independent black-box decoders in both
//! directions (round 466).
//!
//! The encoder mirrors the decoder's "one extra entry" at end-of-input
//! by performing one phantom dictionary extension during its final
//! flush (see [`crate::encode_file`] — the post-loop block accounts for the
//! `(prev, prev's own first byte)` entry the decoder adds on receipt of
//! `prev`; that entry's index is the encoder's `next_code`). Without
//! that phantom add the two sides would desync at the exact moment the
//! decoder's final assignment lands on `2^w − 1` (decoder bumps;
//! encoder doesn't), and the encoder's EOI would be written at the old
//! width while the decoder reads at the new one.
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
    acc: u64,
    nbits: u32,
}

impl BitWriter {
    fn with_capacity(bytes: usize) -> Self {
        Self {
            out: Vec::with_capacity(bytes),
            acc: 0,
            nbits: 0,
        }
    }

    #[inline]
    fn write(&mut self, code: u32, width: u32) {
        debug_assert!(width <= u32::from(MAX_CODE_WIDTH));
        // Place the new code's bits ABOVE any partial bits already in
        // the accumulator: the in-byte ordering specified in
        // "BUILD 8-BIT BYTES" puts the previous code's tail in the LSBs.
        self.acc |= u64::from(code) << self.nbits;
        self.nbits += width;
        if self.nbits >= 32 {
            self.out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.nbits -= 32;
        }
    }

    /// Bits written so far.
    fn bits(&self) -> u64 {
        self.out.len() as u64 * 8 + u64::from(self.nbits)
    }

    /// Flush the trailing partial bytes (if any) and return the encoded
    /// bytes.
    fn finish(mut self) -> Vec<u8> {
        while self.nbits > 0 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.nbits = self.nbits.saturating_sub(8);
        }
        self.out
    }
}

/// Right-to-left bit reader over a 64-bit window.
struct BitReader<'a> {
    src: &'a [u8],
    pos: usize,
    acc: u64,
    nbits: u32,
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

    /// Top the window up to more than 56 bits, or to the end of the
    /// input.
    #[inline]
    fn refill(&mut self) {
        if let Some(chunk) = self.src.get(self.pos..self.pos + 8) {
            // Eight bytes are at hand: OR them all in above the bits
            // already held and account for as many whole bytes as fit.
            // Bits of a byte that did not fit whole are OR-ed in again,
            // identically, by the next refill.
            let word = u64::from_le_bytes(chunk.try_into().expect("8-byte slice"));
            self.acc |= word << self.nbits;
            let taken = (63 - self.nbits) / 8;
            self.pos += taken as usize;
            self.nbits += taken * 8;
        } else {
            while self.nbits <= 56 && self.pos < self.src.len() {
                self.acc |= u64::from(self.src[self.pos]) << self.nbits;
                self.pos += 1;
                self.nbits += 8;
            }
        }
    }

    /// Read the next `width` bits as a code. Returns `None` when the
    /// underlying byte stream is exhausted before `width` bits could be
    /// assembled — the caller treats that as truncation.
    #[inline]
    fn read(&mut self, width: u32) -> Option<u16> {
        debug_assert!(width <= u32::from(MAX_CODE_WIDTH));
        if self.nbits < width {
            self.refill();
            if self.nbits < width {
                return None;
            }
        }
        let code = (self.acc & ((1u64 << width) - 1)) as u16;
        self.acc >>= width;
        self.nbits -= width;
        Some(code)
    }
}

// ---------------------------------------------------------------------
// Encoder.
// ---------------------------------------------------------------------

/// When the encoder sends a Clear code once the dictionary has reached
/// its 4096-entry ceiling. Every policy is within Appendix F: the cover
/// sheet lets an encoder keep a full table for as long as it likes and
/// §F.1 lets it send a Clear at any time. All of them decode to the same
/// pixels, and they emit identical bytes for a raster that never fills
/// the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ClearPolicy {
    /// Never: keep coding against the full table until end of image.
    Never,
    /// At once, the moment the table fills.
    OnFull,
    /// When the full table stops paying for itself. While the table is
    /// being built the encoder notes what the build achieved, in bits
    /// per pixel, from its Clear to the moment it filled. Once full, it
    /// measures the bits per pixel over runs of up to `MONITOR_CODES`
    /// codes, looking every `MONITOR_STEP` codes; a run that costs more
    /// than the build did on average (by a margin that narrows as the
    /// run grows) means the table no longer describes what is being
    /// coded, and a fresh one — short codes, strings that fit — will do
    /// better, so a Clear is sent. A table that still fits (its long
    /// strings beat the build average) is kept.
    #[default]
    Monitored,
}

/// Codes per measurement run of [`ClearPolicy::Monitored`].
const MONITOR_CODES: u32 = 96;
/// Codes between two looks at the run in progress.
const MONITOR_STEP: u32 = 16;

/// Reusable LZW encoder state.
///
/// The dictionary maps `(prefix code, next pixel)` to the code of the
/// extended string. It is a directly indexed table — slot
/// `prefix × 256 + pixel` of `4096 × 256` 16-bit slots (2 MiB) holds the
/// code plus one, zero meaning "no such string" — so a lookup is a
/// single load. The slots written since the last Clear are logged, and a
/// Clear zeroes exactly those (at most 4094) instead of the whole table.
///
/// (An open-addressing hash table of 8192 packed 32-bit slots, 32 KiB,
/// was measured against this layout: the multiply and the probe on every
/// pixel cost more than the cache misses of the direct table — 145 to
/// 250 megapixels per second against 300 to 500 on the same index
/// planes — so the direct table stays.)
///
/// Keeping one `LzwEncoder` across the frames of an animation saves the
/// allocation per frame; output is identical to the free functions.
pub struct LzwEncoder {
    /// `code + 1` per `(prefix, pixel)`; `0` when the string is unknown.
    table: Vec<u16>,
    /// Slots written since the last Clear.
    written: Vec<u32>,
}

impl LzwEncoder {
    /// Allocate an encoder with an empty dictionary.
    pub fn new() -> Self {
        Self {
            table: vec![0u16; MAX_TABLE_SIZE * 256],
            written: Vec::with_capacity(MAX_TABLE_SIZE),
        }
    }

    /// Forget every string learnt since the last Clear.
    fn forget(table: &mut [u16], written: &mut Vec<u32>) {
        for slot in written.drain(..) {
            table[slot as usize] = 0;
        }
    }

    /// Encode one raster, keeping a full table until end of image
    /// ([`ClearPolicy::Never`]). Byte-identical to [`encode`].
    pub fn encode_frame(&mut self, min_code_size: u8, pixels: &[u8]) -> Result<Vec<u8>> {
        self.encode_frame_with(min_code_size, pixels, ClearPolicy::Never)
    }

    /// Encode `pixels` (palette indices) under the variable-length-code
    /// LZW scheme of Appendix F with the given [`ClearPolicy`]. Returns
    /// the raw compressed byte stream excluding the leading LZW Minimum
    /// Code Size byte and the surrounding sub-block framing (§22.c.i and
    /// §15 are applied by the caller).
    pub fn encode_frame_with(
        &mut self,
        min_code_size: u8,
        pixels: &[u8],
        policy: ClearPolicy,
    ) -> Result<Vec<u8>> {
        if !(MIN_CODE_SIZE_FLOOR..=MIN_CODE_SIZE_CEIL).contains(&min_code_size) {
            return Err(Error::InvalidInput(format!(
                "LZW minimum code size {min_code_size} outside [2, 8]"
            )));
        }
        let clear_code: u32 = 1 << min_code_size;
        if let Some(&bad) = pixels.iter().find(|&&b| u32::from(b) >= clear_code) {
            return Err(Error::InvalidInput(format!(
                "pixel index {bad} >= 2^min_code_size ({clear_code})"
            )));
        }
        let eoi_code = clear_code + 1;
        let first_entry = clear_code + 2;
        let initial_width = u32::from(min_code_size) + 1;
        let ceiling = MAX_TABLE_SIZE as u32;
        let max_width = u32::from(MAX_CODE_WIDTH);

        let mut writer = BitWriter::with_capacity(pixels.len() / 4 + 16);
        // §F.1 recommendation — output Clear as the first code.
        writer.write(clear_code, initial_width);

        let Some((&first, rest)) = pixels.split_first() else {
            // Empty raster: emit only EOI after the Clear we already wrote.
            writer.write(eoi_code, initial_width);
            return Ok(writer.finish());
        };

        let table = &mut self.table[..MAX_TABLE_SIZE * 256];
        let written = &mut self.written;
        let mut next_code = first_entry;
        let mut width = initial_width;
        let mut prev = u32::from(first);

        // [`ClearPolicy::Monitored`] bookkeeping: where (in pixels and in
        // output bits) the current table's build began, what the build
        // cost once it filled, and the run being measured.
        let mut build_from = (0usize, writer.bits());
        let mut build_cost = (0u64, 1u64); // (bits, pixels)
        let mut run_from = 0usize;
        let mut run_codes = 0u32;

        for (at, &pixel) in rest.iter().enumerate() {
            let slot = (prev << 8 | u32::from(pixel)) as usize;
            let held = table[slot];
            if held != 0 {
                // The string `prev + pixel` already has a code — extend.
                prev = u32::from(held) - 1;
                continue;
            }

            // Emit the code for `prev` and learn the new string.
            writer.write(prev, width);
            prev = u32::from(pixel);

            if next_code < ceiling {
                table[slot] = next_code as u16 + 1;
                written.push(slot as u32);
                // Width-bump rule (Appendix F.4) — see the module-level
                // doc-comment for the derivation.
                if next_code == 1 << width && width < max_width {
                    width += 1;
                }
                next_code += 1;
                if next_code < ceiling {
                    continue;
                }
                // The table just filled.
                match policy {
                    ClearPolicy::Never => continue,
                    ClearPolicy::OnFull => {}
                    ClearPolicy::Monitored => {
                        build_cost = (
                            writer.bits() - build_from.1,
                            (at + 1 - build_from.0).max(1) as u64,
                        );
                        run_from = at + 1;
                        run_codes = 0;
                        continue;
                    }
                }
            } else {
                // Full table (cover sheet: keep emitting 12-bit codes
                // against it until a Clear is chosen).
                if policy != ClearPolicy::Monitored {
                    continue;
                }
                run_codes += 1;
                if !run_codes.is_multiple_of(MONITOR_STEP) {
                    continue;
                }
                // bits/pixel of the run so far against the build's, with
                // a margin that narrows as the run (and so the evidence)
                // grows:
                //   run_bits / run_pixels
                //       >  (1 + MONITOR_STEP / 2 / run_codes) × build_bits / build_pixels
                let run_bits = u64::from(run_codes * max_width);
                let run_pixels = (at + 1 - run_from).max(1) as u64;
                let lhs = run_bits * build_cost.1 * u64::from(2 * run_codes);
                let rhs = build_cost.0 * run_pixels * u64::from(2 * run_codes + MONITOR_STEP);
                if lhs <= rhs {
                    if run_codes >= MONITOR_CODES {
                        run_from = at + 1;
                        run_codes = 0;
                    }
                    continue;
                }
            }

            // §F.1 Clear: written at the current width, after which both
            // sides restart from the initial table.
            writer.write(clear_code, width);
            Self::forget(table, written);
            next_code = first_entry;
            width = initial_width;
            build_from = (at + 1, writer.bits());
        }

        // Final pending prefix. The decoder, on receiving this code, will
        // add a dictionary entry (prev_code + first_byte_of_this) at index
        // `next_code` and may bump its width when that lands on `2^W − 1`.
        // Mirror that here so the encoder's `width` advances in lock-step;
        // otherwise the EOI emission below would go out at the old width
        // while the decoder reads at the new one. We do not actually need
        // the dictionary entry's contents (no further compression
        // happens), only the bump-trigger side-effect.
        writer.write(prev, width);
        if next_code < ceiling && next_code == 1 << width && width < max_width {
            width += 1;
        }

        // §F.2 — EOI must be the last code output for an image.
        writer.write(eoi_code, width);
        // Leave the dictionary empty for the next raster.
        Self::forget(table, written);
        Ok(writer.finish())
    }
}

impl Default for LzwEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Encode `pixels` (palette indices) under the variable-length-code
/// LZW scheme of Appendix F, keeping a full dictionary until end of
/// image (the cover-sheet *deferred clear* rule). Returns the raw
/// compressed byte stream excluding the leading LZW Minimum Code Size
/// byte and excluding the surrounding sub-block framing (§22.c.i and
/// §15 are applied by the caller).
///
/// Callers encoding many frames in sequence should reuse a single
/// [`LzwEncoder`].
pub fn encode(min_code_size: u8, pixels: &[u8]) -> Result<Vec<u8>> {
    encode_with(min_code_size, pixels, ClearPolicy::Never)
}

/// Encode `pixels` like [`encode`], but emit a Clear code and rebuild
/// the dictionary from scratch whenever the table fills
/// ([`ClearPolicy::OnFull`]) rather than freezing it.
///
/// Both outputs decode to the same pixels — the reader honours a
/// mid-stream Clear (§F.1 "reset table state"). For rasters that never
/// fill the table the two functions emit identical bytes.
pub fn encode_with_clear_on_full(min_code_size: u8, pixels: &[u8]) -> Result<Vec<u8>> {
    encode_with(min_code_size, pixels, ClearPolicy::OnFull)
}

/// Encode `pixels` with the given [`ClearPolicy`] (one-shot form of
/// [`LzwEncoder::encode_frame_with`]).
pub fn encode_with(min_code_size: u8, pixels: &[u8], policy: ClearPolicy) -> Result<Vec<u8>> {
    LzwEncoder::new().encode_frame_with(min_code_size, pixels, policy)
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
/// # How strings are stored
///
/// A dictionary entry is the string of the code received before it plus
/// the first pixel of the code that created it. Those pixels were
/// written to the output one after the other, so the entry's string
/// already sits in the output: it starts where the earlier code's string
/// starts and is one pixel longer. Each entry is therefore just an
/// `(output offset, length)` pair, and expanding a code is one copy from
/// earlier output — a long run costs one block move, with no
/// pixel-by-pixel chain walk. The table is 4096 such pairs; nothing else
/// is allocated besides the output.
///
/// # Memory bound
///
/// The capacity reserved for the output is clamped against the
/// compressed input length (a code is at least three bits and expands
/// to at most 4096 pixels), so a hostile `width × height` cannot make
/// the decoder reserve memory the payload could never fill.
pub fn decode(min_code_size: u8, src: &[u8], expected_pixels: usize) -> Result<Vec<u8>> {
    if !(MIN_CODE_SIZE_FLOOR..=MIN_CODE_SIZE_CEIL).contains(&min_code_size) {
        return Err(Error::InvalidData(format!(
            "LZW minimum code size {min_code_size} outside [2, 8]"
        )));
    }

    let clear_code: u16 = 1 << min_code_size;
    let eoi_code: u16 = clear_code + 1;
    let first_entry: u16 = clear_code + 2;
    let initial_width = u32::from(min_code_size) + 1;
    let max_width = u32::from(MAX_CODE_WIDTH);
    let ceiling = MAX_TABLE_SIZE as u16;
    // Output offsets are kept in 32 bits; a GIF raster (at most
    // 65535 × 65535 pixels) always fits.
    let expected_pixels = expected_pixels.min(u32::MAX as usize);

    // Where each entry's string starts in the output, and its length.
    let mut starts = [0u32; MAX_TABLE_SIZE];
    let mut lengths = [0u16; MAX_TABLE_SIZE];

    let mut next_code = first_entry;
    let mut width = initial_width;
    let mut reader = BitReader::new(src);

    let cap_ceiling = src.len().saturating_mul(MAX_TABLE_SIZE);
    let mut output: Vec<u8> = Vec::with_capacity(expected_pixels.min(cap_ceiling));

    // The previous code's string: (output offset, length). `None` right
    // after a Clear.
    let mut prev: Option<(usize, usize)> = None;

    let missing_eoi =
        || Error::InvalidData("LZW stream ended before End-of-Information code".into());

    loop {
        let code = reader.read(width).ok_or_else(missing_eoi)?;

        if code == clear_code {
            // §F.1 — reset table state.
            next_code = first_entry;
            width = initial_width;
            prev = None;
            continue;
        }
        if code == eoi_code {
            // §F.2 — terminator.
            return Ok(output);
        }

        let at = output.len();
        let room = expected_pixels - at;
        let length = if code < clear_code {
            // A single pixel.
            if room > 0 {
                output.push(code as u8);
            }
            1
        } else if code < next_code {
            // A known string: one copy from earlier output.
            let (from, length) = (
                starts[code as usize] as usize,
                lengths[code as usize] as usize,
            );
            let take = length.min(room);
            output.extend_from_within(from..from + take);
            length
        } else if code == next_code {
            // The entry being defined by this very code: the previous
            // string followed by its own first pixel (the encoder added
            // `prev + first(prev)` when it emitted `prev`, and may use
            // it at once).
            let Some((from, length)) = prev else {
                return Err(Error::InvalidData(
                    "first non-Clear LZW code references uninitialised prefix".into(),
                ));
            };
            let take = length.min(room);
            output.extend_from_within(from..from + take);
            if room > length {
                let first = output[from];
                output.push(first);
            }
            length + 1
        } else {
            return Err(Error::InvalidData(format!(
                "LZW code {code} exceeds dictionary size {next_code}"
            )));
        };

        // Add `prev + first pixel of this string` as the next entry,
        // mirroring the encoder's table growth (lagged by one — see
        // module docs). It starts where `prev` starts.
        if let Some((from, prev_length)) = prev
            && next_code < ceiling
        {
            starts[next_code as usize] = from as u32;
            lengths[next_code as usize] = (prev_length + 1) as u16;
            // Decoder bump rule: assigned == 2^W − 1 (one entry earlier
            // than the encoder's own bump trigger at 2^W).
            if u32::from(next_code) == (1 << width) - 1 && width < max_width {
                width += 1;
            }
            next_code += 1;
        }
        // else: deferred-clear regime — keep decoding without growing
        // the table until a Clear arrives.
        prev = Some((at, length));

        if output.len() == expected_pixels {
            // The raster is complete. Whatever follows produces no
            // pixels; it is still walked so that a missing terminator or
            // an out-of-range code is reported.
            return drain(
                reader,
                DrainState {
                    clear_code,
                    next_code,
                    width,
                    after_clear: false,
                },
            )
            .map(|()| output);
        }
    }
}

/// Decoder state carried into [`drain`].
struct DrainState {
    clear_code: u16,
    next_code: u16,
    width: u32,
    /// No string code has been read since the last Clear.
    after_clear: bool,
}

/// Walk the codes that follow a complete raster up to the
/// End-of-Information code, tracking only the table size and code width.
fn drain(mut reader: BitReader<'_>, state: DrainState) -> Result<()> {
    let DrainState {
        clear_code,
        mut next_code,
        mut width,
        mut after_clear,
    } = state;
    let eoi_code = clear_code + 1;
    let first_entry = clear_code + 2;
    let initial_width = clear_code.trailing_zeros() + 1;
    let max_width = u32::from(MAX_CODE_WIDTH);
    loop {
        let Some(code) = reader.read(width) else {
            return Err(Error::InvalidData(
                "LZW stream ended before End-of-Information code".into(),
            ));
        };
        if code == clear_code {
            next_code = first_entry;
            width = initial_width;
            after_clear = true;
            continue;
        }
        if code == eoi_code {
            return Ok(());
        }
        if code > next_code {
            return Err(Error::InvalidData(format!(
                "LZW code {code} exceeds dictionary size {next_code}"
            )));
        }
        if code == next_code && after_clear {
            return Err(Error::InvalidData(
                "first non-Clear LZW code references uninitialised prefix".into(),
            ));
        }
        if !after_clear && next_code < MAX_TABLE_SIZE as u16 {
            if u32::from(next_code) == (1 << width) - 1 && width < max_width {
                width += 1;
            }
            next_code += 1;
        }
        after_clear = false;
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
        //   Clear=4 (w=3), 0 (w=3), 1 (w=3), 2 (w=3),
        //   3, 6, 8, 10, 9, 7, 3 (all w=4), EOI=5 (w=5)
        //
        // Width bump 3 → 4 after assigning entry 8 (= 2^3); the final
        // `3` makes the decoder assign entry 15 (= 2^4 − 1), so EOI goes
        // out at 5 bits. Bits packed right-to-left per "BUILD 8-BIT
        // BYTES". Total bits = 4*3 + 7*4 + 5 = 45 → 6 bytes (last has
        // 3 trailing zero bits). The whole-file form of this raster
        // decodes identically through two independent black-box GIF
        // readers.
        const EXPECTED: [u8; 6] = [0x44, 0x34, 0x86, 0x9A, 0x37, 0x05];
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

    // -----------------------------------------------------------------
    // `encode_with_clear_on_full` — Clear-on-full table strategy.
    //
    // Decodes to the same pixels as `encode` (the §F.1 mid-stream Clear
    // is honoured by `decode`); for rasters that never fill the table it
    // emits byte-identical output; on a table-overflowing raster it
    // re-adapts the dictionary instead of freezing it.
    // -----------------------------------------------------------------

    /// A table-overflowing raster encoded with the Clear-on-full
    /// strategy must round-trip through `decode` bit-for-bit. This is
    /// the core correctness claim: the decoder's §F.1 reset on a
    /// mid-stream Clear keeps the two sides in lock-step across every
    /// dictionary rebuild.
    #[test]
    fn clear_on_full_roundtrips_across_multiple_fills() {
        // 200_000 pseudo-random 8-bit pixels fills the 4096-entry
        // dictionary several times over, so the encoder emits multiple
        // mid-stream Clears.
        let mut pixels = Vec::with_capacity(200_000);
        let mut state: u32 = 0xC0FF_EE11;
        for _ in 0..200_000 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            pixels.push((state & 0xFF) as u8);
        }
        let encoded = encode_with_clear_on_full(8, &pixels).unwrap();
        let decoded = decode(8, &encoded, pixels.len()).unwrap();
        assert_eq!(decoded, pixels);
    }

    /// For a raster that never fills the dictionary, Clear-on-full and
    /// the deferred-clear `encode` must emit identical bytes — the new
    /// strategy only diverges once the table is full.
    #[test]
    fn clear_on_full_matches_encode_below_table_full() {
        // Each of these stays well under the 4096-entry ceiling, so the
        // deferred-clear freeze never triggers and the two strategies
        // emit the same bytes. (A raster that *does* fill the table —
        // e.g. tens of thousands of high-entropy pixels — is expected to
        // diverge; that divergence is what `clear_on_full_*` cover.)
        let cases: Vec<(u8, Vec<u8>)> = vec![
            (2, Vec::new()),
            (2, vec![0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3]),
            (2, vec![3u8; 3000]),
            (3, vec![0u8; 16]),
            (4, (0..600u32).map(|i| (i % 16) as u8).collect()),
        ];
        for (mcs, pixels) in &cases {
            let baseline = encode(*mcs, pixels).expect("encode");
            let variant = encode_with_clear_on_full(*mcs, pixels).expect("clear-on-full");
            assert_eq!(
                variant,
                baseline,
                "diverged below table-full at min_code_size={mcs}, len={}",
                pixels.len()
            );
        }
    }

    /// On a large raster whose later content differs from its early
    /// content, re-adapting the dictionary (Clear-on-full) must not
    /// produce a *larger* stream than freezing it (deferred clear). The
    /// raster is two regimes concatenated so the frozen first-half
    /// dictionary is a poor fit for the second half — the case the
    /// strategy exists to win.
    #[test]
    fn clear_on_full_not_worse_on_regime_change() {
        let mut pixels = Vec::with_capacity(160_000);
        // First regime: a low-entropy repeating texture that fills the
        // table with patterns tuned to itself.
        let mut s: u32 = 0xABCD_1234;
        for _ in 0..80_000 {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            pixels.push((s & 0x0F) as u8); // only 16 distinct values
        }
        // Second regime: full 8-bit entropy, unrelated to the first.
        for _ in 0..80_000 {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            pixels.push((s & 0xFF) as u8);
        }
        let frozen = encode(8, &pixels).unwrap();
        let readapt = encode_with_clear_on_full(8, &pixels).unwrap();
        // Both must decode to the source.
        assert_eq!(decode(8, &frozen, pixels.len()).unwrap(), pixels);
        assert_eq!(decode(8, &readapt, pixels.len()).unwrap(), pixels);
        // Re-adapting must not lose to freezing on this regime-change
        // input (it typically wins).
        assert!(
            readapt.len() <= frozen.len(),
            "clear-on-full {} bytes worse than deferred-clear {} bytes",
            readapt.len(),
            frozen.len()
        );
    }

    #[test]
    fn clear_on_full_empty_and_error_paths() {
        // Empty raster: Clear + EOI, same as `encode`.
        assert_eq!(
            encode_with_clear_on_full(2, &[]).unwrap(),
            encode(2, &[]).unwrap()
        );
        // Out-of-palette pixel and out-of-range min_code_size reject
        // exactly like `encode`.
        assert!(matches!(
            encode_with_clear_on_full(2, &[0, 1, 5]),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            encode_with_clear_on_full(1, &[0]),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            encode_with_clear_on_full(9, &[0]),
            Err(Error::InvalidInput(_))
        ));
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

    // -----------------------------------------------------------------
    // `LzwEncoder` reusable state.
    //
    // The reusable encoder must produce byte-identical output to the
    // free `encode` function — same Clear, same per-prefix entries,
    // same width-bump points, same EOI. The dictionary reset between
    // frames must walk only the touched-keys log and leave a clean
    // table for the next frame.
    // -----------------------------------------------------------------

    /// Re-encoding the same raster on a fresh `LzwEncoder` and on
    /// the free `encode` must produce the exact same bytes —
    /// `encode_frame` is the same state machine, just hoisting the
    /// dictionary's allocation out of the call.
    #[test]
    fn lzw_encoder_matches_free_function_byte_for_byte() {
        // Cover the spec corners: empty raster, the hand-derived 4-color
        // fixture, the 8-bit table-overflow stress, a long monotone
        // KwKwK run, and a 16-colour width-bump stairstep.
        let cases: Vec<(u8, Vec<u8>)> = vec![
            (2, Vec::new()),
            (2, vec![0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3]),
            (2, vec![3u8; 3000]),
            (3, vec![0u8; 16]),
            (4, {
                let mut p = Vec::with_capacity(20_000);
                let mut s: u32 = 0x1234_5678;
                for _ in 0..20_000 {
                    s ^= s << 13;
                    s ^= s >> 17;
                    s ^= s << 5;
                    p.push((s & 0x0F) as u8);
                }
                p
            }),
            (8, {
                let mut p = Vec::with_capacity(5_000);
                for i in 0..5_000u32 {
                    p.push((i.wrapping_mul(31) & 0xFF) as u8);
                }
                p
            }),
        ];
        let mut encoder = LzwEncoder::new();
        for (mcs, pixels) in &cases {
            let via_free = encode(*mcs, pixels).expect("free encode");
            let via_reuse = encoder.encode_frame(*mcs, pixels).expect("reusable encode");
            assert_eq!(
                via_reuse,
                via_free,
                "byte mismatch at min_code_size={mcs}, raster len={}",
                pixels.len()
            );
        }
    }

    /// Two back-to-back frames through one `LzwEncoder` must each
    /// produce the same bytes the free function would have produced
    /// for that frame in isolation. This pins the dictionary reset:
    /// frame 2 must not see entries left over from frame 1.
    #[test]
    fn lzw_encoder_resets_dictionary_between_frames() {
        let frame_a: Vec<u8> = (0..200u32).map(|i| (i % 4) as u8).collect();
        let frame_b: Vec<u8> = vec![3u8; 200];

        let expected_a = encode(2, &frame_a).expect("frame a");
        let expected_b = encode(2, &frame_b).expect("frame b");

        let mut encoder = LzwEncoder::new();
        let got_a = encoder.encode_frame(2, &frame_a).expect("reused a");
        let got_b = encoder.encode_frame(2, &frame_b).expect("reused b");
        assert_eq!(got_a, expected_a);
        assert_eq!(got_b, expected_b);

        // Sanity: same again on a third call must still produce
        // the right bytes for frame_a — dictionary state is clean.
        let got_a_again = encoder.encode_frame(2, &frame_a).expect("reused a again");
        assert_eq!(got_a_again, expected_a);
    }

    /// An `InvalidInput` failure mid-frame must leave the encoder
    /// ready for the next call; the dictionary reset runs regardless
    /// of the error path so a hostile or bug-induced input doesn't
    /// permanently corrupt the encoder's table.
    #[test]
    fn lzw_encoder_recovers_dictionary_after_error() {
        let mut encoder = LzwEncoder::new();

        // First, exercise the encoder with a happy-path frame so the
        // dictionary is non-trivially populated and reset before the
        // failing call. Without this lead-in the post-reset state
        // would be identical to the construction state and the test
        // would not actually pin the reset-on-error path.
        let happy: Vec<u8> = vec![0, 1, 2, 3, 0, 1, 2, 3];
        encoder.encode_frame(2, &happy).expect("happy");

        // Out-of-range pixel index: encoder must surface the error
        // and leave its dictionary clean.
        let bad = vec![0u8, 1, 7];
        let err = encoder.encode_frame(2, &bad);
        assert!(matches!(err, Err(Error::InvalidInput(_))));

        // Next call on a healthy raster must produce the same bytes
        // the free function would, confirming the dictionary was
        // reset after the failed call.
        let recovery: Vec<u8> = vec![0, 1, 2, 3];
        let expected = encode(2, &recovery).expect("free recovery");
        let got = encoder.encode_frame(2, &recovery).expect("reused recovery");
        assert_eq!(got, expected);
    }

    /// `LzwEncoder::default()` must be equivalent to
    /// `LzwEncoder::new()` — same fresh dictionary, same per-call
    /// behaviour.
    #[test]
    fn lzw_encoder_default_matches_new() {
        let pixels: Vec<u8> = (0..256u32).map(|i| (i % 4) as u8).collect();
        let via_new = LzwEncoder::new()
            .encode_frame(2, &pixels)
            .expect("new encode");
        let via_default = LzwEncoder::default()
            .encode_frame(2, &pixels)
            .expect("default encode");
        assert_eq!(via_new, via_default);
    }

    /// A frame that triggers the deferred-clear regime (dictionary
    /// fills at min_code_size=8) must reset cleanly so a subsequent
    /// small frame still produces correct bytes.
    #[test]
    fn lzw_encoder_resets_after_dictionary_overflow() {
        let mut encoder = LzwEncoder::new();
        // Saturate the 4096-entry table on the first frame.
        let mut big = Vec::with_capacity(80_000);
        let mut state: u32 = 0xDEAD_BEEF;
        for _ in 0..80_000 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            big.push((state & 0xFF) as u8);
        }
        let big_expected = encode(8, &big).expect("free big");
        assert_eq!(
            encoder.encode_frame(8, &big).expect("reused big"),
            big_expected
        );

        // Now drive a tiny frame at min_code_size=2 (different
        // alphabet, much smaller table population). Output must
        // still match the free function — no stale entries from the
        // saturated frame leak through.
        let small: Vec<u8> = vec![0, 1, 2, 3, 0, 1, 2, 3];
        let small_expected = encode(2, &small).expect("free small");
        assert_eq!(
            encoder.encode_frame(2, &small).expect("reused small"),
            small_expected
        );
    }

    // ----- clear policies -----

    fn xorshift_plane(len: usize, mask: u8, mut state: u32) -> Vec<u8> {
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state as u8) & mask
            })
            .collect()
    }

    const POLICIES: [ClearPolicy; 3] = [
        ClearPolicy::Never,
        ClearPolicy::OnFull,
        ClearPolicy::Monitored,
    ];

    #[test]
    fn every_policy_roundtrips_at_every_code_size() {
        for mcs in MIN_CODE_SIZE_FLOOR..=MIN_CODE_SIZE_CEIL {
            let mask = ((1u16 << mcs) - 1) as u8;
            // Noise, then long runs, then a repeating texture: three
            // different statistics, each long enough to fill the table.
            let mut plane = xorshift_plane(30_000, mask, 0x1234_5678 + u32::from(mcs));
            plane.extend(std::iter::repeat_n(mask, 20_000));
            plane.extend((0..30_000u32).map(|i| ((i / 3 + i % 7) as u8) & mask));
            for policy in POLICIES {
                let packed = encode_with(mcs, &plane, policy).unwrap();
                assert_eq!(
                    decode(mcs, &packed, plane.len()).unwrap(),
                    plane,
                    "{policy:?} at code size {mcs}"
                );
            }
        }
    }

    #[test]
    fn policies_agree_until_the_table_fills() {
        let plane = xorshift_plane(600, 0x0F, 99);
        let never = encode_with(4, &plane, ClearPolicy::Never).unwrap();
        for policy in POLICIES {
            assert_eq!(encode_with(4, &plane, policy).unwrap(), never, "{policy:?}");
        }
    }

    #[test]
    fn monitored_clear_tracks_a_change_of_statistics() {
        // Noise that fills the table with short strings, followed by a
        // regular texture: the frozen table of `Never` cannot follow, the
        // monitored policy notices and rebuilds.
        let mut plane = xorshift_plane(40_000, 0x3F, 0xC0FF_EE11);
        plane.extend((0..200_000u32).map(|i| (64 + (i * 5) % 59 + (i / 211) % 3) as u8));
        let never = encode_with(8, &plane, ClearPolicy::Never).unwrap();
        let monitored = encode_with(8, &plane, ClearPolicy::Monitored).unwrap();
        assert!(
            monitored.len() * 2 < never.len(),
            "monitored {} vs never {}",
            monitored.len(),
            never.len()
        );
    }

    #[test]
    fn monitored_clear_keeps_a_table_that_still_fits() {
        // One texture throughout: once learnt, the table keeps paying, so
        // the monitored stream stays close to the never-cleared one.
        let plane: Vec<u8> = (0..300_000u32).map(|i| ((i * 7) % 61) as u8).collect();
        let never = encode_with(8, &plane, ClearPolicy::Never).unwrap();
        let monitored = encode_with(8, &plane, ClearPolicy::Monitored).unwrap();
        let on_full = encode_with(8, &plane, ClearPolicy::OnFull).unwrap();
        assert!(monitored.len() <= never.len() + never.len() / 20);
        assert!(monitored.len() < on_full.len());
    }

    // ----- decoder -----

    #[test]
    fn long_runs_expand_exactly() {
        for len in [1usize, 2, 3, 255, 4096, 4097, 100_000] {
            let plane = vec![3u8; len];
            let packed = encode(2, &plane).unwrap();
            assert_eq!(decode(2, &packed, len).unwrap(), plane, "run of {len}");
        }
    }

    #[test]
    fn output_is_cut_at_the_expected_size_and_the_rest_is_still_checked() {
        let plane = xorshift_plane(5_000, 0x3F, 7);
        let packed = encode(6, &plane).unwrap();
        for keep in [0usize, 1, 2, 17, 4_999] {
            assert_eq!(decode(6, &packed, keep).unwrap(), plane[..keep], "{keep}");
        }
        // A stream cut short after the raster is complete still lacks its
        // End-of-Information code.
        let cut = &packed[..packed.len() - 2];
        assert!(decode(6, cut, 100).is_err());
        // More room than pixels: the decoder returns what the stream held.
        assert_eq!(decode(6, &packed, 9_000).unwrap(), plane);
    }

    #[test]
    fn codes_past_the_table_are_rejected_before_and_after_the_cut() {
        // Clear (4), pixel 0, then code 7 while the table ends at 6.
        let mut w = BitWriter::with_capacity(8);
        for code in [4u32, 0, 7, 5] {
            w.write(code, 3);
        }
        let bytes = w.finish();
        assert!(decode(2, &bytes, 100).is_err());
        assert!(decode(2, &bytes, 1).is_err());
        // The not-yet-defined code straight after a Clear has no prefix.
        let mut w = BitWriter::with_capacity(8);
        for code in [4u32, 6, 5] {
            w.write(code, 3);
        }
        let bytes = w.finish();
        assert!(decode(2, &bytes, 100).is_err());
        assert!(decode(2, &bytes, 0).is_err());
    }

    #[test]
    fn bit_reader_matches_bit_writer_at_every_width() {
        let mut w = BitWriter::with_capacity(64);
        let mut codes = Vec::new();
        let mut state = 1u32;
        for i in 0..2_000u32 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let width = 3 + i % 10;
            let code = (state >> 8) & ((1 << width) - 1);
            w.write(code, width);
            codes.push((code, width));
        }
        assert_eq!(w.bits(), codes.iter().map(|c| u64::from(c.1)).sum::<u64>());
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        for (code, width) in codes {
            assert_eq!(r.read(width), Some(code as u16));
        }
        assert_eq!(r.read(12), None);
    }
}
