# oxideav-gif benchmark suite

This crate ships four Criterion bench harnesses driven by
`cargo bench -p oxideav-gif`:

| Harness        | Scope                                                              | Run                                              |
| -------------- | ------------------------------------------------------------------ | ------------------------------------------------ |
| `decode`       | Full GIF decoder hot paths (§17/§18/§20/§22 + §23/§26).            | `cargo bench -p oxideav-gif --bench decode`      |
| `encode`       | Full GIF encoder + `AnimationBuilder` paths.                       | `cargo bench -p oxideav-gif --bench encode`      |
| `roundtrip`    | Build → encode → decode end-to-end loops.                          | `cargo bench -p oxideav-gif --bench roundtrip`   |
| `lzw` (r194)   | **Direct Appendix F LZW codec pair** — `lzw::encode` / `lzw::decode` in isolation. | `cargo bench -p oxideav-gif --bench lzw`         |

Each harness is self-contained: every bench scenario synthesises its
pixel data with a deterministic `xorshift32` generator inside the
harness, so no test fixtures need to be committed and the run is
reproducible across machines.

## `lzw` harness — what it measures

The other three harnesses route input through the GIF container path:
§17 Header → §18 Logical Screen Descriptor → §19 Global Color Table →
§20 Image Descriptor → §22 Table-Based Image Data → §15 sub-block
framing. The Appendix F LZW codec is one stage in that pipeline. The
`lzw` harness skips every other stage and benchmarks the codec call
pair directly, so a future optimisation round can attribute its
delta to the codec itself rather than to the surrounding container
serialisation.

Each scenario builds a deterministic palette-index buffer of
`width × height` bytes, calls `oxideav_gif::lzw::encode(min_code_size,
&pixels)` to produce the compressed payload (outside the timed region
for the decode benches), then drives `lzw::encode` or `lzw::decode`
inside `b.iter`. `Throughput::Bytes(width * height)` reports the
per-second pixel-decompression / pixel-compression rate.

## Scenarios

| Scenario                  | W×H        | Palette | `min_code_size` | Comments                                                                        |
| ------------------------- | ---------- | ------- | --------------- | ------------------------------------------------------------------------------- |
| `16x16/pal=4`             | 16×16      | 4       | 2               | Smallest meaningful input — §F.4 width-bump fires repeatedly per pixel.         |
| `256x256/pal=64`          | 256×256    | 64      | 6               | Mid-size; dictionary saturates (4096 entries) → cover-sheet "deferred clear".   |
| `1024x1024/pal=256`       | 1024×1024  | 256     | 8               | Natural-image stress at §22.c.i maximum `min_code_size`; reaches 12-bit ceiling fast. |
| `anim/100x_64x64`         | 100×(64×64)| 16      | 4               | 100 independent `encode`/`decode` calls; per-frame fixed-cost path.             |

## Round 194 baseline (Apple M-series, `cargo bench -- --quick`)

Median throughput, reported by Criterion. Numbers are
order-of-magnitude baselines from a single dev-machine run — use them
as a regression guard, not as cross-platform specs.

| Scenario                  | `lzw::encode`     | `lzw::decode`     |
| ------------------------- | ----------------- | ----------------- |
| `16x16/pal=4`             | ~1.79 µs (137 MiB/s) | ~692 ns (353 MiB/s)  |
| `256x256/pal=64`          | ~129 µs (486 MiB/s)  | ~180 µs (348 MiB/s)  |
| `1024x1024/pal=256`       | ~2.93 ms (342 MiB/s) | ~3.25 ms (308 MiB/s) |
| `anim/100x_64x64` (×100)  | ~2.31 ms (169 MiB/s) | ~1.67 ms (234 MiB/s) |

Observations from the baseline:

- At the maximum 8-bit alphabet (`min_code_size=8`) encode and
  decode are within ~10% of each other — both spend most of their
  time chasing the 4096-entry dictionary table.
- At small alphabets (`min_code_size=2`) decode is roughly 2.5× faster
  than encode: the encoder's `(prev_code, next_byte) → code` lookup
  table is `4096 × 256 = 1 MiB` of zeroed memory per call, which
  dominates the 16×16 run.
- The `anim/100x_64x64` row pays the dictionary setup cost 100×, so
  its MiB/s drops well below the single 256×256 number even though
  the per-frame raster is smaller — a useful baseline for any future
  "amortise the encode dictionary across frames" optimisation.

## Re-running

Full statistically-significant run (≈30s/scenario):

    cargo bench -p oxideav-gif --bench lzw

Faster baseline (10s/scenario, ±5% noise):

    cargo bench -p oxideav-gif --bench lzw -- --quick

Per-scenario filter:

    cargo bench -p oxideav-gif --bench lzw -- 'lzw_encode_256x256'

Criterion writes HTML reports to `target/criterion/` under the chosen
`CARGO_TARGET_DIR`; the bench harnesses themselves emit no committed
artefacts.
