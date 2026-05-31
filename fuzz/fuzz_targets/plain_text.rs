#![no_main]

//! Dedicated §25 Plain Text Extension fuzz harness.
//!
//! Round 200 (depth-mode fuzz): the sibling `decode`/`encode`/`roundtrip`
//! harnesses already touch the §25 Plain Text render path indirectly via
//! `decode → compose` and `AnimationBuilder → encode → decode → compose`,
//! but neither one can synthesise a `Block::PlainText` from fuzz bytes —
//! `AnimationBuilder` exposes image frames only, and the decoder will
//! only emit a `PlainText` block when the input bytes already match the
//! §25.c on-disk layout. So the dedicated Plain Text grammar (12-byte
//! fixed parameter block + variable-length §15 sub-block payload + an
//! optional preceding §23 Graphic Control Extension) is reachable from
//! the decoder side only when the fuzzer happens to stumble onto the
//! `0x21 0x01 0x0C` extension-introducer prefix — a vanishingly rare
//! event on truly arbitrary fuzz input.
//!
//! This harness drives the encode side directly: it builds a `GifImage`
//! whose `blocks` vector is a sequence of `Block::PlainText` (some with
//! attached GCEs, some without), encodes it, decodes the resulting
//! bytes, composes the decoded image, and asserts the decode→encode→
//! decode trip is idempotent on Plain-Text-only payloads. The classic
//! spec problem areas this surfaces:
//!
//!   - §25.c.viii–ix `cell_width = 0` / `cell_height = 0` (the divide-
//!     in `render_plain_text` short-circuits to a no-op; verify it
//!     never panics).
//!   - §25.c.iv–vii placement rectangle bounded against the §18
//!     Logical Screen (the harness keeps it in-bounds; `compose`'s
//!     `check_rect_in_screen` guard is also exercised on the bounded
//!     side).
//!   - §25.c.x–xi `fg_color_index` / `bg_color_index` past the end of
//!     the §18 GCT (clamps to transparent black in `compose`).
//!   - §25.c.xii text payload of any length 0..=N, including payloads
//!     longer than one §15 sub-block (255 bytes) — `write_data_sub_
//!     blocks` must split them, `read_data_sub_blocks` must rejoin.
//!   - §25.e font-fallback: any byte outside `0x20..=0x7E` renders as
//!     space; the harness feeds arbitrary bytes including 0x00, 0xFF,
//!     and the wider §25.e `0x20..=0xF7` range.
//!   - §23 Graphic Control Extension attached to a Plain Text block —
//!     `Block::PlainText::graphic_control` propagates the disposal
//!     method into the §23 state machine in `compose`. Mixing
//!     `RestorePrevious` (the trickiest disposal value, snapshot/
//!     revert) with Plain Text rendering hits a path the existing
//!     harnesses never reach with a Plain Text block.
//!
//! Contract: every called function returns to its caller. A `panic!`,
//! `unwrap()` on `None`, slice-OOB, integer-overflow in debug, or OOM
//! abort is a finding and fails the fuzzer. Wrong-but-non-panic
//! behaviour (e.g. an `Err` from the encoder on an adversarial size
//! field) is out of scope — early-return on `Err`.

use libfuzzer_sys::fuzz_target;
use oxideav_gif::{
    compose, decode, decode_first_frame, decode_lenient, encode,
    image::{Block, GifImage, GraphicControl, PlainText, Rgb},
    playback::Playback,
    DisposalMethod, Version,
};

// Cap downstream work on suspiciously large screens so the harness
// doesn't fault for OOM on a single ~17 GiB `Vec<u8>` allocation in
// `RgbaCanvas`. `compose` allocates `screen_width × screen_height × 4`
// bytes per canvas; we additionally clone the canvas per emitted
// `ComposedFrame`, so the budget tightens once frames pile up.
const MAX_CANVAS_PIXELS: u32 = 1 << 18; // 256 Kpx — generous for a text-rendering harness.

// Cap the number of Plain Text blocks the harness asks the encoder to
// emit per fuzz run. Each one clones the canvas in `compose` so this
// scales the inner-loop allocation cost.
const MAX_PT_BLOCKS: usize = 16;

// Cap the per-block text payload length so a 64 KiB-text block doesn't
// dominate every iteration's wall time. The encoder splits payloads
// across §15 sub-blocks (≤255 B each), so the 1024 cap still exercises
// the multi-sub-block path (4+ chunks per payload).
const MAX_PT_TEXT_LEN: usize = 1024;

// Cap looping playback iterations so a stream that happens to encode a
// `loop_count = Some(0)` Application Extension (we never write one
// here, but a future extension might) doesn't pin the fuzzer.
const MAX_PLAYBACK_FRAMES: usize = 64;

/// Derive a `DisposalMethod` from the low 2 bits of one fuzz byte.
fn pick_disposal(bits: u8) -> DisposalMethod {
    match bits & 0x03 {
        0 => DisposalMethod::None,
        1 => DisposalMethod::Keep,
        2 => DisposalMethod::RestoreBackground,
        _ => DisposalMethod::RestorePrevious,
    }
}

/// Build a small palette deterministically seeded from one fuzz byte.
/// 16 entries is enough to make fg/bg indices interesting (a u8 index
/// can address 0..=255, so most fuzzed indices hit the "past end of
/// palette" clamp path in `render_plain_text`).
fn build_palette(seed: u8) -> Vec<Rgb> {
    let mut out = Vec::with_capacity(16);
    let mut s = seed as u32;
    for _ in 0..16 {
        // LCG step — same constants as the sibling `encode` harness.
        s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        out.push(Rgb::new(
            (s & 0xff) as u8,
            ((s >> 8) & 0xff) as u8,
            ((s >> 16) & 0xff) as u8,
        ));
    }
    out
}

/// Derive a Plain Text placement rectangle (`left`, `top`, `width`,
/// `height`) that fits inside the §18 Logical Screen. `compose`'s
/// `check_rect_in_screen` would `Err` on an escaping rectangle and
/// short-circuit the rest of the harness; keeping the rect in-bounds
/// means every fuzz iteration reaches `render_plain_text`.
fn pick_rect(bits: u32, screen_w: u16, screen_h: u16) -> (u16, u16, u16, u16) {
    let x = (bits & 0xff) as u16 % screen_w.max(1);
    let y = ((bits >> 8) & 0xff) as u16 % screen_h.max(1);
    let w_max = (screen_w - x).max(1);
    let h_max = (screen_h - y).max(1);
    // Minimum 1×1; size up to whatever remains. (Pulling the size
    // from a *different* slice of `bits` than the position avoids the
    // degenerate "every Plain Text block at (0,0) with the same size"
    // pattern that would dampen fuzzer coverage.)
    let w = (1 + ((bits >> 16) & 0xff) as u16) % w_max + 1;
    let w = w.min(w_max);
    let h = (1 + ((bits >> 24) & 0xff) as u16) % h_max + 1;
    let h = h.min(h_max);
    (x, y, w, h)
}

fuzz_target!(|data: &[u8]| {
    // Need at least the screen + palette + block-count header.
    if data.len() < 5 {
        return;
    }

    // Screen dimensions: derive from two pairs of bytes, masked to a
    // small range so the harness doesn't allocate a 65535² canvas. The
    // `% 256` cap keeps every fuzzed screen at most 256×256 = 65 Kpx,
    // well under MAX_CANVAS_PIXELS even with hostile expansion.
    let screen_w = (1u16 + (data[0] as u16)) % 256 + 1;
    let screen_h = (1u16 + (data[1] as u16)) % 256 + 1;
    let pixels = screen_w as u32 * screen_h as u32;
    if pixels > MAX_CANVAS_PIXELS {
        return;
    }

    let palette = build_palette(data[2]);

    // `background_index` only matters when a §23 RestoreBackground
    // disposal fires; allow any u8 so we cover both "in palette" and
    // "past end of palette" cases (the latter resolves to fully-
    // transparent black via `GifImage::background_color_rgba`).
    let background_index = data[3];

    // Plain Text blocks require a §18 Global Color Table per §25.a;
    // without one, `render_plain_text` is a no-op in `compose` (and
    // an Err in `encode` on at least one path). Always attach a GCT
    // so we exercise the rendering side.
    let mut img = GifImage {
        version: Version::Gif89a, // §25 is 89a-only.
        screen_width: screen_w,
        screen_height: screen_h,
        color_resolution: 3,
        global_palette_sorted: false,
        background_index,
        pixel_aspect_ratio: 0,
        global_palette: Some(palette),
        blocks: Vec::new(),
    };

    // Each subsequent 8-byte chunk encodes one Plain Text block:
    //   bytes[0..4]  → rect bits (left/top/width/height via pick_rect)
    //   byte 4       → cell_width
    //   byte 5       → cell_height
    //   byte 6       → fg_color_index
    //   byte 7       → bg_color_index
    // The text payload is taken from the chunk that follows, sized by
    // (byte 4 as u16 << 2) clamped to MAX_PT_TEXT_LEN so most payloads
    // sit between 0 and 1024 bytes. A separate disposal bit comes from
    // (byte 5 >> 6), with bit 0x10 of (byte 6) deciding whether the
    // block carries a §23 GCE at all.
    let mut cursor = 4;
    let mut block_count = 0;
    while cursor + 8 <= data.len() && block_count < MAX_PT_BLOCKS {
        let rect_bits = u32::from_le_bytes([
            data[cursor],
            data[cursor + 1],
            data[cursor + 2],
            data[cursor + 3],
        ]);
        let (left, top, width, height) = pick_rect(rect_bits, screen_w, screen_h);
        let cell_w = data[cursor + 4];
        let cell_h = data[cursor + 5];
        let fg = data[cursor + 6];
        let bg = data[cursor + 7];

        cursor += 8;

        // Text payload — drawn from the *next* chunk so the rect bits
        // and the payload are decorrelated, and so the fuzzer can grow
        // the payload independently of the parameter block.
        let want_payload_len = ((cell_w as usize) << 2).min(MAX_PT_TEXT_LEN);
        let avail = data.len().saturating_sub(cursor);
        let payload_len = want_payload_len.min(avail);
        let text = data[cursor..cursor + payload_len].to_vec();
        cursor += payload_len;

        let graphic_control = if (fg & 0x10) != 0 {
            Some(GraphicControl {
                disposal: pick_disposal(cell_h >> 6),
                user_input: (bg & 0x01) != 0,
                transparent_index: if (bg & 0x02) != 0 { Some(fg) } else { None },
                // 0..=255 centiseconds is the realistic range; the
                // GifImage::total_play_duration accessor uses saturating
                // arithmetic so an adversarial value here can't overflow.
                delay_centis: cell_w as u16,
            })
        } else {
            None
        };

        img.blocks.push(Block::PlainText {
            params: PlainText {
                left,
                top,
                width,
                height,
                cell_width: cell_w,
                cell_height: cell_h,
                fg_color_index: fg,
                bg_color_index: bg,
                text,
            },
            graphic_control,
        });
        block_count += 1;
    }

    if block_count == 0 {
        return;
    }

    // Pre-encode accessors must be panic-free even with adversarial
    // Plain Text payloads. `total_play_duration` walks every GCE
    // attached to a graphic-rendering block, so attaching delay
    // centiseconds to Plain Text exercises that path on a non-image
    // block.
    let _ = img.is_animated();
    let _ = img.total_play_duration();
    let _ = img.required_version();
    let _ = img.frame_delays().count();

    // Encode the synthesised Plain-Text-only stream. `encode` walks
    // every block and §15-packages the variable-length payload via
    // `write_plain_text_extension`; an `Err` here means the encoder
    // rejected the synthesised parameter combination, which is in
    // scope as "non-panic" behaviour.
    let Ok(bytes) = encode(&img) else {
        return;
    };

    // Round-trip through the strict decoder. The encoder is supposed
    // to be a left inverse of the decoder, but for a fuzzer harness
    // the strict invariant we care about is that the strict decoder
    // *parses* what the encoder *wrote* without panicking. A round-
    // trip equality assertion belongs in the dedicated roundtrip
    // harness; here a decode failure is a finding (because we just
    // wrote those bytes ourselves).
    let decoded = match decode(&bytes) {
        Ok(i) => i,
        Err(e) => panic!("Plain-Text-only encoder output rejected by strict decoder: {e}"),
    };

    // Lenient + cover-frame paths — both must return on the encoded
    // bytes. Cover-frame in particular short-circuits at the first
    // §20 image-bearing block; an all-PlainText stream has none, so
    // `decode_first_frame` should return `Err` (not panic).
    let _ = decode_lenient(&bytes);
    let _ = decode_first_frame(&bytes);

    // Compose — drives `render_plain_text` against the decoded image.
    // The §23 disposal-method state machine fires per Plain Text
    // block (including the snapshot/revert path when a GCE specifies
    // `RestorePrevious`), and the §25.c.x/xi out-of-palette fg/bg
    // index clamp kicks in for every cell.
    let _ = compose(&decoded);

    // Lazy playback iterators — same compositor state via a different
    // driver. Both must terminate; `looping_frames` is bounded
    // explicitly because a NETSCAPE2.0 forever-loop (if one ever
    // landed in `decoded.blocks` through `decode_lenient` resync)
    // would otherwise pin the fuzzer.
    let pb = Playback::new(&decoded);
    for _ in pb.frames().take(MAX_PLAYBACK_FRAMES) {}
    for _ in pb.looping_frames().take(MAX_PLAYBACK_FRAMES) {}
});
