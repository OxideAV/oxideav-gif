#!/usr/bin/env python3
"""
Seed corpus generator for oxideav-gif fuzz targets.

Emits byte-exact, spec-derived seed files for the
`decode`, `decode_panic_free`, and `decode_lenient_panic_free`
corpora (a flat copy under `fuzz/seed_corpus/<target>/`):

  1. 1x1 GIF87a minimal — from tests/spec_fixtures.rs (§17/§18/§19/§20/
     §22/§15/§27 walk).
  2. 2x2 GIF89a + GCE — from tests/spec_fixtures.rs (§17/§18/§19/§23/
     §20/§22/§15/§27 walk).
  3. Truncated-trailer (§27 missing) — the 1x1 GIF87a with the final
     0x3B chopped off. Tests EOF handling on the trailer state machine.
  4. Oversized-LZW (§22.c.i illegal value 12; max legal LZW code width
     is 12 bits = min code size <= 11) — the 1x1 GIF87a with the LZW
     min code size byte forced to 0x0C. Tests Appendix F width-clamp.
  5. NETSCAPE2.0 Application Extension (§26) with a bogus sub-block
     length (claims 0x42 bytes when only 0x03 follow) — exercises the
     §15 sub-block-chain framing on adversarial length prefixes.

Also emits the `plain_text` (§25) seeds and the round-318 `lzw` seeds
(direct Appendix F codec-pair parameter streams: two well-formed mcs=2
compressed payloads from the `lzw::decode` unit fixtures + four
adversarial parameter perturbations).

All seeds are pure spec-walks; no external library code consulted.
"""
import hashlib
import os

OUT_DECODE        = "fuzz/seed_corpus/decode_panic_free"
OUT_LENIENT       = "fuzz/seed_corpus/decode_lenient_panic_free"
OUT_DECODE_E2E    = "fuzz/seed_corpus/decode"
OUT_PLAIN_TEXT    = "fuzz/seed_corpus/plain_text"

# ---- Fixture 1: 1x1 GIF87a minimal (from tests/spec_fixtures.rs:28..) -------
fix_87a = bytes([
    # Header §17 — 'G','I','F','8','7','a'
    0x47, 0x49, 0x46, 0x38, 0x37, 0x61,
    # Logical Screen Descriptor §18
    0x01, 0x00, 0x01, 0x00, 0b1000_0000, 0x00, 0x00,
    # Global Color Table §19 — 2 entries × 3 bytes
    0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF,
    # Image Descriptor §20
    0x2C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00,
    # LZW Minimum Code Size §22.c.i
    0x02,
    # Sub-block §15
    0x02, 0x44, 0x01,
    # §16 terminator + §27 trailer
    0x00, 0x3B,
])

# ---- Fixture 2: 2x2 GIF89a + GCE (from tests/spec_fixtures.rs:100..) --------
fix_89a = bytes([
    # Header §17 — GIF89a
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61,
    # Logical Screen Descriptor §18
    0x02, 0x00, 0x02, 0x00, 0b1000_0001, 0x00, 0x00,
    # Global Color Table §19 — 4 RGB entries
    0x00, 0x00, 0x00, 0xFF, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x00, 0x00, 0xFF,
    # Graphic Control Extension §23
    0x21, 0xF9, 0x04, 0x09, 0x05, 0x00, 0x03, 0x00,
    # Image Descriptor §20
    0x2C, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x02, 0x00, 0x00,
    # LZW Minimum Code Size §22.c.i
    0x02,
    # Sub-block §15
    0x03, 0x44, 0x64, 0x0A,
    # §16 terminator + §27 trailer
    0x00, 0x3B,
])

# ---- Malformed 1: truncated §27 trailer -------------------------------------
# 1x1 GIF87a minus the final 0x3B byte. Decoder must hit EOF on trailer
# read and surface a Result::Err, never panic.
malformed_truncated = fix_87a[:-1]

# ---- Malformed 2: oversized LZW min code size -------------------------------
# Take the 1x1 GIF87a and overwrite the §22.c.i Minimum Code Size byte
# with 0x0C (12). Per Appendix F, the LZW code width starts at min+1
# bits and tops out at 12 bits, so a min of 12 is illegal. Locate the
# byte: it's the first byte after the §20 Image Descriptor (the 10
# bytes 0x2C,left,top,w,h,packed starting at offset 18 in fix_87a).
# Image Descriptor is at offset 19..29; LZW min-code-size is byte 29.
malformed_oversize_lzw = bytearray(fix_87a)
assert malformed_oversize_lzw[29] == 0x02, f"unexpected layout: byte 29 = {malformed_oversize_lzw[29]:#x}"
malformed_oversize_lzw[29] = 0x0C
malformed_oversize_lzw = bytes(malformed_oversize_lzw)

# ---- Malformed 3: NETSCAPE2.0 App Ext with bad sub-block length -------------
# A §26 Application Extension whose sub-block length byte claims 0x42
# bytes of follow-on data when only 3 bytes actually exist before the
# §15 sub-block terminator. Wraps the §23 disposal-method state machine
# around a 1x1 transparent GIF89a, with the malformed App Ext placed
# between the Logical Screen Descriptor and the Image Descriptor.
malformed_app_ext_overrun = bytes([
    # Header §17 — GIF89a
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61,
    # Logical Screen Descriptor §18 — 1x1, GCT flag, 2 entries
    0x01, 0x00, 0x01, 0x00, 0b1000_0000, 0x00, 0x00,
    # Global Color Table §19 — 2 entries
    0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF,
    # Application Extension §26
    0x21, 0xFF,
    0x0B,                                                          # block size = 11
    0x4E, 0x45, 0x54, 0x53, 0x43, 0x41, 0x50, 0x45, 0x32, 0x2E, 0x30,   # "NETSCAPE2.0"
    # Sub-block §15 — *claims* 0x42 (66) bytes but only 3 actually follow.
    # The decoder must treat the over-claim as a Result::Err / lenient
    # resync, never panic.
    0x42, 0x03, 0x01, 0x00,
    0x00,                                                          # §16 terminator
    # Image Descriptor §20 — 1x1
    0x2C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00,
    # LZW Minimum Code Size §22.c.i
    0x02,
    # Sub-block §15
    0x02, 0x44, 0x01,
    # §16 terminator + §27 trailer
    0x00, 0x3B,
])

seeds = {
    "spec_1x1_gif87a_minimal":            fix_87a,
    "spec_2x2_gif89a_with_gce":           fix_89a,
    "malformed_truncated_trailer":        malformed_truncated,
    "malformed_oversize_lzw_min_code":    malformed_oversize_lzw,
    "malformed_app_ext_subblock_overrun": malformed_app_ext_overrun,
}

# =============================================================================
# Plain Text fuzz target seeds (fuzz/fuzz_targets/plain_text.rs).
# =============================================================================
#
# The `plain_text` harness consumes raw fuzz bytes (NOT GIF on-disk
# format) and synthesises a `GifImage` whose `blocks` are §25 Plain
# Text Extensions. The byte layout the harness reads:
#
#     data[0]   screen_width  (`% 256 + 1`, then `+ 1`)
#     data[1]   screen_height (same)
#     data[2]   palette seed
#     data[3]   background_index
#     data[4..] one or more 8-byte block headers + payload chunks:
#               b0..b3  rect bits (left/top/width/height via pick_rect)
#               b4      cell_width  (also payload-len scaler / delay_centis)
#               b5      cell_height (also disposal high bits)
#               b6      fg_color_index (bit 0x10 = attach GCE)
#               b7      bg_color_index (bit 0x01 = user_input, 0x02 = transparent)
#               then    `(cell_w as usize) << 2` bytes of text payload
#                       (clamped to MAX_PT_TEXT_LEN=1024 and to remaining input).
#
# Each seed walks one specific path through the harness so a fresh
# fuzz session reaches the relevant `render_plain_text` branch within
# the first few iterations rather than after coverage-guided warm-up.

# Seed 1 — small in-bounds PT block, no GCE.
# screen 64×64, palette seed 0x55, bg_idx=0.
# One block: rect bits 0x10100808 → left=8, top=16, width=17, height=17.
# cell_w=8, cell_h=8, fg=0 (bit 0x10 unset → no GCE), bg=1.
# Payload: "AB" (2 bytes — well under (8<<2)=32 cap).
pt_seed_basic = bytes([
    0x40, 0x40, 0x55, 0x00,
    # Block 0 header (8 B)
    0x08, 0x10, 0x10, 0x10,   # rect bits → (8, 16, 17, 17)
    0x08, 0x08, 0x00, 0x01,   # cell_w=8 cell_h=8 fg=0 bg=1
    # Payload (cell_w=8, so harness wants (8<<2)=32 bytes; provide 2 → clipped)
    0x41, 0x42,               # "AB"
])

# Seed 2 — PT block with attached GCE running RestorePrevious disposal.
# Exercises the §23.f.i snapshot-on-render + revert-on-disposal path
# on a non-image block (the snapshot/restore code path is the trickiest
# disposal value and is rarely reached on Plain Text blocks via the
# decode-side fuzzer).
# screen 32×32, palette seed 0xA5, bg_idx=3.
# Two blocks back-to-back so the second sees the post-revert canvas.
pt_seed_gce_restore_previous = bytes([
    0x20, 0x20, 0xA5, 0x03,
    # Block 0: PT with GCE+RestorePrevious. cell_h>>6 = 0b11 = RestorePrevious.
    0x00, 0x00, 0x08, 0x08,   # rect bits → small placement near origin
    0x04, 0xC0, 0x10, 0x00,   # cell_w=4 cell_h=0xC0 fg=0x10 (GCE on) bg=0
    # Payload (cell_w=4 → wants (4<<2)=16 bytes; provide 16)
    0x21, 0x40, 0x7F, 0x80, 0xFF, 0x00, 0x20, 0x7E,
    0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    # Block 1: PT with no GCE.
    0x10, 0x10, 0x10, 0x10,
    0x08, 0x08, 0x02, 0x05,
    # Payload
    0x48, 0x65, 0x6C, 0x6C, 0x6F,   # "Hello"
])

# Seed 3 — degenerate cell sizes (cell_w=0, cell_h=0). Hits the early
# no-op branch in `render_plain_text` (`if cell_width == 0 …`).
# screen 8×8, palette seed 0, bg_idx=0.
pt_seed_degenerate_cell = bytes([
    0x08, 0x08, 0x00, 0x00,
    0x00, 0x00, 0x04, 0x04,   # rect → (0, 0, 5, 5)
    0x00, 0x00, 0x00, 0x00,   # cell_w=0 cell_h=0 → no-op render
    # No payload bytes — cell_w=0 means harness wants 0 bytes of text.
])

plain_text_seeds = {
    "pt_basic_no_gce":            pt_seed_basic,
    "pt_gce_restore_previous":    pt_seed_gce_restore_previous,
    "pt_degenerate_cell_size":    pt_seed_degenerate_cell,
}

# =============================================================================
# Direct LZW codec fuzz target seeds (fuzz/fuzz_targets/lzw.rs, round 318).
# =============================================================================
#
# The `lzw` harness consumes raw fuzz bytes (NOT GIF on-disk format) and
# drives `oxideav_gif::lzw::{decode,encode}` directly. The byte layout
# the harness reads:
#
#     data[0]      min_code_size (the FULL u8 range; [2,8] are spec-valid,
#                  everything else must `Err` cleanly)
#     data[1..5]   expected_pixels selector (u32, little-endian)
#     data[5..]    the compressed-byte payload (also reused as the
#                  `lzw::encode` palette-index buffer)
#
# Each seed anchors one Appendix F decode path so a fresh fuzz session
# reaches it on iteration 1..N rather than after coverage warm-up. The
# compressed payloads are byte-for-byte the encoder's own output for a
# known index buffer (hand-derived from the §F state machine + verified
# against the in-tree `lzw::encode` unit-test fixtures — no external
# library consulted).

def _lzw_seed(min_code_size, expected_pixels, payload):
    return bytes([min_code_size & 0xFF]) + \
        int(expected_pixels & 0xFFFFFFFF).to_bytes(4, "little") + bytes(payload)

# Seed 1 — well-formed mcs=2 stream for the 16-pixel [0,1,2,3]×4 buffer.
# Compressed bytes are the `known_good_4color_byte_pattern` unit-test
# fixture in src/lzw.rs (§F emission sequence Clear,0,1,2,3,6,8,10,9,7,3,EOI).
# expected_pixels = 16 → decodes exactly back to the source indices.
lzw_seed_valid_4color = _lzw_seed(
    0x02, 16, [0x44, 0x64, 0x0C, 0x35, 0x6F, 0x0A]
)

# Seed 2 — well-formed mcs=2 single-pixel stream (the §22.c.i payload
# from the 1×1 GIF87a fixture: Clear,0,EOI packed as 0x44,0x01).
# expected_pixels = 1.
lzw_seed_valid_1px = _lzw_seed(0x02, 1, [0x44, 0x01])

# Seed 3 — illegal min_code_size = 12 (> §F ceiling of 8). Must hit the
# `[2,8]` validation rejection in `lzw::decode` (and the encoder's mirror
# guard), never the `1 << 12` / `clear_code + 2` arithmetic. Payload and
# expected_pixels are arbitrary — the rejection fires before they matter.
lzw_seed_illegal_mcs = _lzw_seed(0x0C, 64, [0x44, 0x64, 0x0C])

# Seed 4 — hostile expected_pixels (near u32::MAX) against a tiny 2-byte
# compressed payload. Forces the `expected_pixels.min(src.len() *
# MAX_TABLE_SIZE)` allocation clamp: 2 * 4096 = 8192 caps the
# `Vec::with_capacity`, never a multi-gigabyte reservation. mcs=2.
lzw_seed_alloc_clamp = _lzw_seed(0x02, 0xFFFF_FFFF, [0x44, 0x01])

# Seed 5 — a non-Clear first code that references an out-of-range entry
# (KwKwK / uninitialised-prefix path). mcs=8 so the first code is 9 bits;
# the bytes 0xFF,0xFF feed code 0x1FF which exceeds the initial dictionary
# size, so the decoder must `Err` ("uninitialised prefix" / "exceeds
# dictionary size") rather than index past `next_code`.
lzw_seed_bad_first_code = _lzw_seed(0x08, 32, [0xFF, 0xFF])

# Seed 6 — truncated stream that ends before EOI. A lone Clear code
# (0x04 at mcs=2 = clear_code, 3 bits) then end-of-input: the decoder
# must `Err` ("ended before End-of-Information code"), never loop.
lzw_seed_no_eoi = _lzw_seed(0x02, 8, [0x04])

lzw_seeds = {
    "lzw_valid_4color_16px":   lzw_seed_valid_4color,
    "lzw_valid_1px":           lzw_seed_valid_1px,
    "lzw_illegal_min_code":    lzw_seed_illegal_mcs,
    "lzw_alloc_clamp":         lzw_seed_alloc_clamp,
    "lzw_bad_first_code":      lzw_seed_bad_first_code,
    "lzw_no_eoi":              lzw_seed_no_eoi,
}

OUT_LZW = "fuzz/seed_corpus/lzw"

def sha1hex(b): return hashlib.sha1(b).hexdigest()

written = []
for label, blob in seeds.items():
    name = sha1hex(blob)
    for out in (OUT_DECODE, OUT_LENIENT, OUT_DECODE_E2E):
        path = os.path.join(out, name)
        # Skip if file with that sha already exists (corpus is content-addressed).
        if os.path.exists(path):
            print(f"[skip] {label}: {out}/{name} already present")
            continue
        with open(path, "wb") as f:
            f.write(blob)
        written.append((label, out, name, len(blob)))
        print(f"[write] {label} -> {out}/{name} ({len(blob)} B)")

for label, blob in plain_text_seeds.items():
    name = sha1hex(blob)
    path = os.path.join(OUT_PLAIN_TEXT, name)
    if os.path.exists(path):
        print(f"[skip] {label}: {OUT_PLAIN_TEXT}/{name} already present")
        continue
    os.makedirs(OUT_PLAIN_TEXT, exist_ok=True)
    with open(path, "wb") as f:
        f.write(blob)
    written.append((label, OUT_PLAIN_TEXT, name, len(blob)))
    print(f"[write] {label} -> {OUT_PLAIN_TEXT}/{name} ({len(blob)} B)")

for label, blob in lzw_seeds.items():
    name = sha1hex(blob)
    path = os.path.join(OUT_LZW, name)
    if os.path.exists(path):
        print(f"[skip] {label}: {OUT_LZW}/{name} already present")
        continue
    os.makedirs(OUT_LZW, exist_ok=True)
    with open(path, "wb") as f:
        f.write(blob)
    written.append((label, OUT_LZW, name, len(blob)))
    print(f"[write] {label} -> {OUT_LZW}/{name} ({len(blob)} B)")

print(
    f"\n{len(written)} seed file(s) written across "
    f"{OUT_DECODE}, {OUT_LENIENT}, {OUT_DECODE_E2E}, {OUT_PLAIN_TEXT}, "
    f"and {OUT_LZW}."
)
