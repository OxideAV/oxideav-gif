# Seed Corpus

Spec-derived seed files for the `oxideav-gif` cargo-fuzz targets.

`fuzz/corpus/` is gitignored (per `.gitignore`) because it is the
local fuzz flywheel — every interesting input the fuzzer discovers
gets added there during a run, and pinning that whole tree in git
would balloon repo size. The seeds in this directory are different:
they are a small, hand-derived, audit-grade set of inputs that
exercise the spec's classic problem areas, and they are tracked so
every contributor (and CI) can bootstrap a fresh corpus from a known
baseline.

## Layout

One subdirectory per fuzz target. Each subdirectory mirrors the
`fuzz/corpus/<target>/` layout, so re-seeding is a flat copy:

```
fuzz/seed_corpus/decode/...
fuzz/seed_corpus/decode_panic_free/...
fuzz/seed_corpus/decode_lenient_panic_free/...
fuzz/seed_corpus/plain_text/...
fuzz/seed_corpus/lzw/...
```

## Bootstrapping `fuzz/corpus/<target>/`

Run once per target before invoking `cargo fuzz run`:

```
cp -n fuzz/seed_corpus/<target>/* fuzz/corpus/<target>/
cargo fuzz run <target>
```

`-n` is `--no-clobber`: it never overwrites an existing fuzz-flywheel
input, so re-seeding is idempotent and never blows away the local
flywheel's accumulated corpus.

## Seed inventory

Every file is content-addressed by SHA-1 of its bytes (the libFuzzer
default naming scheme), so the on-disk name is meaningless to a
human. The labels below correspond to the bytes-for-bytes payloads
emitted by `tools/seedgen.py` (run from the crate root); see that
script for the spec section each blob walks.

### `decode` / `decode_panic_free` / `decode_lenient_panic_free`

| Label                                | Bytes | Spec sections walked                        |
|--------------------------------------|-------|---------------------------------------------|
| `spec_1x1_gif87a_minimal`            | 35    | §17 / §18 / §19 / §20 / §22 / §15 / §27     |
| `spec_2x2_gif89a_with_gce`           | 50    | §17 / §18 / §19 / §23 / §20 / §22 / §15 / §27 |
| `malformed_truncated_trailer`        | 34    | §27 truncation — EOF on trailer state machine |
| `malformed_oversize_lzw_min_code`    | 35    | §22.c.i Appendix F width clamp (illegal min=12) |
| `malformed_app_ext_subblock_overrun` | 54    | §26 / §15 — App Ext sub-block claims 0x42 B, has 3 |

The two well-formed fixtures are byte-for-byte copies of the test
arrays in `tests/spec_fixtures.rs`, derived by walking each field's
syntax diagram in the GIF89a specification (no external decoder
consulted). The three malformed fixtures perturb a single field of
the 1×1 GIF87a or wrap a §26 Application Extension around it.

### `plain_text` (round 200)

Unlike the other targets, `plain_text` consumes a *fuzz-encoded*
parameter stream — not GIF on-disk bytes — and synthesises
§25 Plain Text Extension blocks directly via the in-process API.
The seeds below walk specific render paths so a fresh fuzz session
reaches them on iteration 1..3 rather than after coverage warm-up.

| Label                       | Bytes | Render path exercised                       |
|-----------------------------|-------|---------------------------------------------|
| `pt_basic_no_gce`           | 14    | One in-bounds §25 block, no §23 GCE attached; standard glyph render through `compose` |
| `pt_gce_restore_previous`   | 41    | §23.f.i pre-render snapshot + §23.c.iv `RestorePrevious` revert on a non-image block; back-to-back blocks share the canvas |
| `pt_degenerate_cell_size`   | 12    | §25.c.viii/ix `cell_width = cell_height = 0` — `render_plain_text` short-circuits to a no-op |

### `lzw` (round 318)

Like `plain_text`, the `lzw` harness consumes a *fuzz-encoded*
parameter stream — not GIF on-disk bytes — and drives the direct
Appendix F codec pair (`lzw::decode` / `lzw::encode`). The byte
layout the harness reads:

```
data[0]      min_code_size (full u8 range; [2,8] spec-valid)
data[1..5]   expected_pixels (u32, little-endian)
data[5..]    compressed-byte payload / encode index buffer
```

| Label                   | Bytes | Appendix F path anchored                       |
|-------------------------|-------|------------------------------------------------|
| `lzw_valid_4color_16px` | 11    | well-formed mcs=2 stream for `[0,1,2,3]×4`; §F width-bump + KwKwK (from the `lzw::decode` unit fixture) |
| `lzw_valid_1px`         | 7     | well-formed mcs=2 single-pixel stream (Clear,0,EOI) |
| `lzw_illegal_min_code`  | 8     | `min_code_size = 12` — §F [2,8] validation rejection |
| `lzw_alloc_clamp`       | 7     | `expected_pixels ≈ u32::MAX` vs 2-byte payload — `src.len() × MAX_TABLE_SIZE` allocation clamp |
| `lzw_bad_first_code`    | 7     | non-Clear first code referencing an out-of-range entry — KwKwK / uninitialised-prefix `Err` |
| `lzw_no_eoi`            | 6     | Clear-only stream ending before EOI — §F.2 "ended before EOI" `Err` |

The two well-formed compressed payloads are byte-for-byte the
`lzw::decode` unit-test fixtures in `src/lzw.rs` (derived by walking
the §F emission state machine; no external library consulted). The
four adversarial seeds perturb a single parameter to anchor a
specific decode-side error or allocation path.

## Verification (round 153)

Each well-formed seed decodes via both `decode()` and
`decode_lenient()`. Each malformed seed surfaces `Err(_)` on
`decode()` (no panic) and a recovered `Ok(_)` on `decode_lenient()`
(no panic) — exactly the contract the `decode_panic_free` and
`decode_lenient_panic_free` fuzz targets assert.

## Verification (round 200, `plain_text` seeds)

All 3 `plain_text` seeds run to completion through the fuzz harness
(`-runs=0` against the seed corpus prints `DONE` with zero finds).
A follow-up `-runs=200000` random walk seeded from these inputs
likewise reports zero crashes — the harness contract holds on every
spec-classic path the seeds anchor.

## Verification (round 318, `lzw` seeds)

All 6 `lzw` seeds run to completion through the fuzz harness
(`-runs=0` against the seed corpus prints `DONE` with zero finds).
A follow-up random walk seeded from these inputs cleared 437K runs
in 46 s with zero crashes — the direct-codec panic-free + idempotence
contract holds on every Appendix F path the seeds anchor.

## Regenerating

```
python3 tools/seedgen.py
```

run from the crate root. The script is pure-Python with no external
deps; the seeds are produced by literal spec walks, not by invoking
any GIF library.
