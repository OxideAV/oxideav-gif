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

All seeds are pure spec-walks; no external library code consulted.
"""
import hashlib
import os

OUT_DECODE        = "fuzz/seed_corpus/decode_panic_free"
OUT_LENIENT       = "fuzz/seed_corpus/decode_lenient_panic_free"
OUT_DECODE_E2E    = "fuzz/seed_corpus/decode"

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

print(
    f"\n{len(written)} seed file(s) written across "
    f"{OUT_DECODE}, {OUT_LENIENT}, and {OUT_DECODE_E2E}."
)
