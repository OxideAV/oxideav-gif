#![no_main]

//! End-to-end GIF encode-side fuzz harness.
//!
//! Sibling to `fuzz/fuzz_targets/decode.rs` (decode-side end-to-end).
//! The decode-side fuzzer drives `decode` then `encode`-on-decoder-
//! output; this one inverts the relationship: it builds a `GifImage`
//! out of arbitrary fuzz bytes via `AnimationBuilder`, encodes it, then
//! sanity-checks the encoded bytes through every entry point so panics
//! in the encoder LZW / sub-block / version-required paths surface
//! here regardless of which downstream consumer first walks them.
//!
//! Contract: every called function returns to its caller. A `panic!`,
//! `unwrap()` on `None`, slice-OOB, integer-overflow in debug, or OOM
//! abort is a finding and fails the fuzzer. Wrong-but-non-panic
//! behaviour (e.g. a builder validation error on adversarial input) is
//! out of scope; the harness early-returns on `Err`.
//!
//! Why this exists separately from the decode-side fuzzer: the
//! encoder's hot paths (LZW table-growth at every `min_code_size`, §15
//! sub-block-chain framing, §7 Required Version inference, §23 GCE
//! emission, NETSCAPE2.0 *Looping* sub-block construction) are reached
//! via `AnimationBuilder` + `encode` rather than via `decode` + re-
//! `encode`. A decoder-output-only fuzzer can't reach builder-only
//! configurations (e.g. a frame whose `width × height` doesn't match
//! its declared index buffer length, an animation with mismatched
//! palette sizes across frames) — this harness can.

use libfuzzer_sys::fuzz_target;
use oxideav_gif::{
    compose, decode, decode_first_frame, decode_lenient, encode, image::Rgb, playback::Playback,
    quantize_frames_shared, quantize_rgba_with_options, AnimationBuilder, DisposalMethod, Dither,
    GifImage, QuantizeOptions,
};

// Cap downstream work on suspiciously large screens so the harness
// doesn't fault for OOM on a single ~17 GiB `Vec<u8>` allocation in
// `RgbaCanvas` (`compose` allocates `screen_width × screen_height × 4`
// bytes per canvas).
const MAX_CANVAS_PIXELS: u32 = 1 << 20; // 1 Mpx — generous for fuzzing.

// Cap the number of frames the harness asks the builder to construct.
// Without this, a fuzzer that finds a 1×1 / 1-entry-palette code path
// can wedge the harness in builder-construction time for thousands of
// micro-frames per iteration.
const MAX_FUZZ_FRAMES: usize = 16;

// Cap looping playback so a `loop_forever()` build doesn't pin the
// fuzzer indefinitely.
const MAX_PLAYBACK_FRAMES: usize = 64;

/// Derive an (`x`, `y`) within `[0, screen)` plus a `(w, h)` that fits
/// from a single 16-bit field — keeps the fuzzed rectangle bounded to
/// the §18 Logical Screen so the builder's validation accepts it.
fn pick_rect(bits: u16, screen_w: u16, screen_h: u16) -> (u16, u16, u16, u16) {
    let xy = bits as u32;
    let x = (xy & 0xff) as u16 % screen_w.max(1);
    let y = ((xy >> 8) & 0xff) as u16 % screen_h.max(1);
    // Minimum 1×1; size up to whatever remains in the screen.
    let w_max = (screen_w - x).max(1);
    let h_max = (screen_h - y).max(1);
    let w = (1 + (xy >> 16) as u16) % w_max + 1;
    let w = w.min(w_max);
    let h = (1 + (xy >> 24) as u16) % h_max + 1;
    let h = h.min(h_max);
    (x, y, w, h)
}

/// Derive a §23.c.iv Disposal Method from the low 2 bits.
fn pick_disposal(bits: u8) -> DisposalMethod {
    match bits & 0x03 {
        0 => DisposalMethod::None,
        1 => DisposalMethod::Keep,
        2 => DisposalMethod::RestoreBackground,
        _ => DisposalMethod::RestorePrevious,
    }
}

/// Build a palette of `palette_size` entries, deterministically seeded
/// from one byte of fuzz input.
fn build_palette(palette_size: u16, seed: u8) -> Vec<Rgb> {
    let mut out = Vec::with_capacity(palette_size as usize);
    let mut s = seed as u32;
    for _ in 0..palette_size {
        s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        out.push(Rgb::new(
            (s & 0xff) as u8,
            ((s >> 8) & 0xff) as u8,
            ((s >> 16) & 0xff) as u8,
        ));
    }
    out
}

fuzz_target!(|data: &[u8]| {
    // Need at least the screen + palette + loop-behaviour header.
    if data.len() < 8 {
        return;
    }

    // Screen dimensions: derive from two pairs of bytes, masked to a
    // small range so the harness doesn't allocate a 65535² canvas. The
    // `% 256` cap keeps every fuzzed screen at most 256×256 = 65k px,
    // well under MAX_CANVAS_PIXELS even with a hostile expansion.
    let screen_w = (1u16 + (data[0] as u16)) % 256 + 1;
    let screen_h = (1u16 + (data[1] as u16)) % 256 + 1;
    let pixels = screen_w as u32 * screen_h as u32;
    if pixels > MAX_CANVAS_PIXELS {
        return;
    }

    // Palette: 1..=256 entries (the §19 valid range). Use one byte
    // for size and another for the LCG seed.
    let palette_size = 1u16 + (data[2] as u16); // 1..=256
    let palette = build_palette(palette_size, data[3]);

    // Loop behaviour from the low 2 bits of data[4]. The remaining
    // bits steer the background index.
    //
    // `background_index` must be `< palette_size`. `palette_size` is in
    // `1..=256`, but a u8 index can only address `0..=255`, so the
    // largest legal index is `min(palette_size, 256) - 1`, which is in
    // `0..=255`. Reducing `data[5]` modulo `(legal_max + 1)` keeps the
    // index in range; the `+ 1` is what makes the divisor non-zero (a
    // bare `palette_size.min(256) as u8` wraps 256 -> 0 and divides by
    // zero, which is the harness bug the encode fuzzer surfaced).
    let mut builder = AnimationBuilder::new(screen_w, screen_h, palette.clone());
    let index_modulus = palette_size.min(256); // u16 in 1..=256, never 0
    builder = builder.background_index((data[5] as u16 % index_modulus) as u8);
    match data[4] & 0x03 {
        0 => builder = builder.play_once(),
        1 => builder = builder.loop_forever(),
        2 => {
            builder = builder.loop_count(u16::from_le_bytes([data[6], data[7]]) % 8 + 1);
        }
        _ => {
            // Default already.
        }
    }

    // Each subsequent 8-byte chunk encodes one frame: (rect_bits[0..2],
    // disposal/delay byte, palette-index seed[3], MAX_FUZZ_FRAMES cap).
    let mut cursor = 8;
    let mut frame_count = 0;
    while cursor + 4 <= data.len() && frame_count < MAX_FUZZ_FRAMES {
        let rect_bits = u16::from_le_bytes([data[cursor], data[cursor + 1]]);
        let (left, top, w, h) = pick_rect(
            rect_bits ^ ((data[cursor + 2] as u16) << 8 | data[cursor + 3] as u16),
            screen_w,
            screen_h,
        );

        // Cap the per-frame index buffer so the harness doesn't burn
        // memory on a giant frame inside a small screen.
        let frame_pixels = w as usize * h as usize;
        if frame_pixels == 0 || frame_pixels > MAX_CANVAS_PIXELS as usize {
            break;
        }

        // Palette-index payload: fill `frame_pixels` bytes from a
        // deterministic LCG seeded by the fuzz byte. The builder
        // validates every index is `< palette.len()`, so reduce modulo
        // palette_size before handing off.
        let mut seed = data[cursor + 3] as u32;
        let palette_size = palette.len() as u8;
        let mut indices = Vec::with_capacity(frame_pixels);
        for _ in 0..frame_pixels {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            indices.push((seed & 0xff) as u8 % palette_size.max(1));
        }

        let delay = (data[cursor + 2] as u16) * 2; // 0..510 centiseconds
        let disposal = pick_disposal(data[cursor + 2]);

        match builder.add_placed_frame(left, top, w, h, indices, delay, disposal) {
            Ok(b) => builder = b,
            Err(_) => return, // builder rejected — out of scope
        }

        cursor += 4;
        frame_count += 1;
    }

    if frame_count == 0 {
        return;
    }

    // Build → encode → decode → compose → playback. Each step early-
    // returns on `Err`; panics fail the fuzzer.
    let img = match builder.build() {
        Ok(i) => i,
        Err(_) => return,
    };

    // Exercise the timing accessors before encode so we cover the
    // pre-encode state machine.
    let _ = img.is_animated();
    let _ = img.single_pass_duration();
    let _ = img.total_play_duration();
    let _: Vec<_> = img.frame_delays().collect();
    let _ = img.required_version();

    let bytes = match encode(&img) {
        Ok(b) => b,
        Err(_) => return,
    };

    // Strict, lenient, and cover-frame entry points — all three must
    // return on the encoded bytes regardless of which configuration
    // the builder ended up with.
    let _ = decode_lenient(&bytes);
    let _ = decode_first_frame(&bytes);

    let Ok(decoded) = decode(&bytes) else {
        // Encoder produced bytes the decoder can't read: that's a
        // finding regardless of what the fuzzer fed us. Panic to surface.
        panic!("encoder emitted bytes the decoder rejected");
    };

    // Compose + lazy playback — exercise the §23 disposal-method state
    // machine on the encoder's output. The playback iterator is
    // explicitly bounded because `loop_forever()` makes it infinite.
    let _ = compose(&decoded);
    let pb = Playback::new(&decoded);
    for _ in pb.frames().take(MAX_PLAYBACK_FRAMES) {}
    for _ in pb.looping_frames().take(MAX_PLAYBACK_FRAMES) {}

    // Truecolor quantiser paths. Synthesise small RGBA frames from the
    // fuzz bytes and drive every option-taking quantiser entry point —
    // the dither index-plane assignment, the shared-palette pooling, and
    // the three RGBA constructors — so panics in the new colour-reduction
    // code surface here. The geometry is capped tiny (≤ 32×32) so the
    // diffusion/pooling work stays bounded per iteration.
    let qw = 1 + (data[0] as usize % 32);
    let qh = 1 + (data[1] as usize % 32);
    let qpx = qw * qh;
    // Build two RGBA frames by tiling the fuzz input across the grid.
    let frame_a: Vec<u8> = (0..qpx * 4).map(|i| data[i % data.len()]).collect();
    let frame_b: Vec<u8> = (0..qpx * 4)
        .map(|i| data[(i + 1) % data.len()] ^ 0x5a)
        .collect();
    let max_colors = 1 + (data[2] as usize % 256);

    for dither in [Dither::None, Dither::FloydSteinberg] {
        let opts = QuantizeOptions::with_max_colors(max_colors).dither(dither);

        // Per-frame quantiser: index plane must stay in palette range.
        if let Ok(q) = quantize_rgba_with_options(&frame_a, qw, qh, opts) {
            assert!(q.indices.iter().all(|&i| (i as usize) < q.palette.len()));
        }

        // Shared-palette pooling over both frames.
        if let Ok(s) = quantize_frames_shared(&[&frame_a, &frame_b], qw, qh, opts) {
            for plane in &s.frame_indices {
                assert!(plane.iter().all(|&i| (i as usize) < s.palette.len()));
            }
        }

        // Constructors → encode → decode round-trip. Each must produce a
        // stream the decoder accepts.
        if qw <= u16::MAX as usize && qh <= u16::MAX as usize {
            let (cw, ch) = (qw as u16, qh as u16);
            if let Ok(img) = GifImage::from_rgba_frame_with_options(&frame_a, cw, ch, opts) {
                if let Ok(b) = encode(&img) {
                    assert!(decode(&b).is_ok(), "dithered still rejected by decoder");
                }
            }
            let frames = [
                (&frame_a[..], 7u16, DisposalMethod::None),
                (&frame_b[..], 7u16, DisposalMethod::Keep),
            ];
            if let Ok(img) =
                GifImage::from_rgba_frames_shared_palette(&frames, cw, ch, opts, Some(0))
            {
                if let Ok(b) = encode(&img) {
                    assert!(
                        decode(&b).is_ok(),
                        "shared-palette animation rejected by decoder"
                    );
                }
            }
        }
    }
});
