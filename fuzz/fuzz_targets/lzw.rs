#![no_main]

//! Dedicated Appendix F LZW codec fuzz harness.
//!
//! Round 318 (depth-mode fuzz): the sibling `decode`/`decode_panic_free`/
//! `decode_lenient_panic_free` harnesses reach the LZW decoder only
//! *through* the full §17/§18/§20 container parser, which constrains
//! every parameter that flows into [`oxideav_gif::lzw::decode`] to a
//! spec-well-formed shape: `min_code_size` is read from the §22.c.i
//! "LZW Minimum Code Size" byte but only after the surrounding Image
//! Descriptor framing validates, the compressed bytes are re-assembled
//! from §15 sub-blocks (so they can never be a bare arbitrary
//! bitstream), and `expected_pixels` is always exactly the Image
//! Descriptor's `width × height`. The `roundtrip` harness only ever
//! feeds the decoder bytes the *encoder* produced.
//!
//! None of those harnesses can drive the direct codec pair with
//! adversarial parameters. This one does: it slices the fuzz input into
//! a `min_code_size`, an `expected_pixels`, and a raw compressed-byte
//! payload, then calls `lzw::decode` / `lzw::encode` directly. The
//! Appendix F decode machinery this surfaces — none of it reachable
//! from a container-gated harness:
//!
//!   - §F-floor/ceil `min_code_size` validation: values outside [2, 8]
//!     (including 0, 1, and 9..=255) must return `Err`, never panic on
//!     the `1 << min_code_size` shift or the `clear_code + 2` add.
//!   - the `expected_pixels.min(src.len() * MAX_TABLE_SIZE)` allocation
//!     ceiling (the DoS guard): a hostile `expected_pixels` near
//!     `usize::MAX` paired with a tiny `src` must clamp to the
//!     `src.len() * 4096` ceiling rather than attempt a multi-exabyte
//!     `Vec::with_capacity`.
//!   - §F.4 code-width growth driven by an *arbitrary* bitstream: the
//!     decoder's `2^W − 2` bump threshold and the 12-bit ceiling are
//!     walked by random code sequences the encoder would never emit.
//!   - the KwKwK self-reference branch (`code == next_code`) reached on
//!     the first non-Clear code (must `Err`, not panic on the `None`
//!     prefix) and mid-stream.
//!   - the over-dictionary branch (`code > next_code`) — an out-of-range
//!     code must `Err`, never index past `next_code`.
//!   - §F.1 Clear / §F.2 EOI handling at arbitrary positions, including
//!     a stream that ends with no EOI (must `Err`).
//!   - deferred-clear regime: a table that saturates at 4096 entries
//!     without a Clear must keep decoding (no table growth, no panic).
//!
//! Plus the direct `lzw::encode` path with arbitrary `min_code_size` +
//! arbitrary palette-index payload, and a `decode(encode(x)) == x`
//! idempotence assertion on the encoder's own output (the encoder is a
//! right inverse of the decoder on every accepted index buffer).
//!
//! Contract: every called function returns to its caller. A `panic!`,
//! `unwrap()` on `None`, slice-OOB, integer-overflow in debug, or OOM
//! abort is a finding and fails the fuzzer. A returned `Err` on
//! adversarial input is in-contract and ignored.

use libfuzzer_sys::fuzz_target;
use oxideav_gif::lzw;

// Cap `expected_pixels` so a legitimately-decodable stream doesn't pin
// the fuzzer materialising a giant output buffer. The codec's own
// `src.len() * MAX_TABLE_SIZE` ceiling already bounds the *allocation*;
// this bounds the *work* (the decode loop stops at `expected_pixels`).
const MAX_EXPECTED_PIXELS: usize = 1 << 22; // 4 Mpx of decoded output.

// Cap the payload fed to `lzw::encode` so the harness doesn't spend an
// iteration compressing a multi-megabyte index buffer.
const MAX_ENCODE_PIXELS: usize = 1 << 18; // 256 Ki indices.

fuzz_target!(|data: &[u8]| {
    // Layout of the fuzz input:
    //   data[0]      -> min_code_size selector
    //   data[1..5]   -> expected_pixels selector (4 bytes, little-endian)
    //   data[5..]    -> the compressed-byte payload / encode index buffer
    if data.len() < 5 {
        // Even with no payload, exercise the empty-`src` decode path for
        // every spec-valid `min_code_size`: a zero-length compressed
        // stream must `Err` ("ended before EOI"), never panic.
        for mcs in 0u8..=10 {
            let _ = lzw::decode(mcs, &[], 0);
            let _ = lzw::decode(mcs, &[], MAX_EXPECTED_PIXELS);
        }
        return;
    }

    // 1. min_code_size — feed the FULL u8 range, not just [2, 8], so the
    //    validation rejection path (and the absence of a panic on the
    //    `1 << min_code_size` shift for mcs >= 8 and the `clear_code + 2`
    //    add for mcs == 8) is exercised. `data[0]` is used verbatim:
    //    0, 1, 9..=255 must all return `Err` cleanly.
    let min_code_size = data[0];

    // 2. expected_pixels — a 32-bit selector so the fuzzer can drive a
    //    value far larger than `src.len()`, forcing the codec's
    //    `expected_pixels.min(src.len() * MAX_TABLE_SIZE)` allocation
    //    clamp. Capped at MAX_EXPECTED_PIXELS so a *legitimately*
    //    decodable payload can't wedge the harness on output work; the
    //    clamp arithmetic itself is still exercised because the codec
    //    sees the capped-but-still-large value against a tiny `src`.
    let expected_raw = u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as usize;
    let expected_pixels = expected_raw.min(MAX_EXPECTED_PIXELS);

    let payload = &data[5..];

    // 3. Direct decode of an ARBITRARY bitstream. This is the core of
    //    the harness: the §F decode loop walking a code sequence the
    //    encoder would never produce. Any returned bytes are discarded;
    //    the contract is "returns a Result, never panics".
    let _ = lzw::decode(min_code_size, payload, expected_pixels);

    // Also probe the degenerate `expected_pixels == 0` decode: the loop
    // must emit nothing and still terminate on Clear/EOI/end-of-input
    // without an underflow on the `output.len() >= expected_pixels`
    // guard (which is true from the first iteration).
    let _ = lzw::decode(min_code_size, payload, 0);

    // 4. Direct encode of an arbitrary palette-index buffer, then a
    //    round-trip idempotence check on the encoder's own output. The
    //    encoder only accepts a spec-valid `min_code_size`; for the
    //    rejected range it returns `Err` and we skip the round-trip.
    let indices: &[u8] = if payload.len() > MAX_ENCODE_PIXELS {
        &payload[..MAX_ENCODE_PIXELS]
    } else {
        payload
    };

    // The encoder requires every index to be addressable by the
    // declared `min_code_size` alphabet (`index < 2^min_code_size`);
    // arbitrary bytes will frequently exceed that and the encoder
    // returns `Err`, which is in-contract. We only assert idempotence
    // when the encoder accepts the buffer.
    if let Ok(compressed) = lzw::encode(min_code_size, indices) {
        // The encoder produced a stream for `indices.len()` pixels; the
        // decoder must reproduce them exactly.
        match lzw::decode(min_code_size, &compressed, indices.len()) {
            Ok(decoded) => {
                assert_eq!(
                    decoded.as_slice(),
                    indices,
                    "lzw::decode(lzw::encode(x)) != x (min_code_size={min_code_size})"
                );
            }
            Err(e) => {
                panic!("encoder output rejected by decoder (min_code_size={min_code_size}): {e}")
            }
        }

        // Re-decode the encoder's output with `expected_pixels` SHORTER
        // than the true length: the decoder must stop early at the
        // `output.len() >= expected_pixels` guard and return a prefix,
        // never over-run or panic. (Exercises the §F deferred /
        // over-production early-break path on real encoder bytes.)
        if !indices.is_empty() {
            let short = indices.len() / 2;
            let _ = lzw::decode(min_code_size, &compressed, short);
        }
    }

    // 5. The reusable-state encoder path (`LzwEncoder`) must produce
    //    byte-identical output to the free function and survive arbitrary
    //    parameters the same way. Drive a single frame so the per-frame
    //    reset bookkeeping is exercised on adversarial input too.
    let mut enc = lzw::LzwEncoder::new();
    if let Ok(via_state) = enc.encode_frame(min_code_size, indices) {
        if let Ok(via_free) = lzw::encode(min_code_size, indices) {
            assert_eq!(
                via_state, via_free,
                "LzwEncoder::encode_frame != lzw::encode (min_code_size={min_code_size})"
            );
        }
    }

    // 6. The clear-on-full encoder variant: same panic-free contract.
    let _ = lzw::encode_with_clear_on_full(min_code_size, indices);
});
