//! Criterion benchmarks for the direct LZW codec pair.
//!
//! Round 194 (depth-mode benchmarks): the sibling harnesses
//! `benches/{encode,decode,roundtrip}.rs` measure the full §17/§18/§20
//! GIF encode + decode loop. This harness measures the **Appendix F LZW
//! codec in isolation** — the `oxideav_gif::lzw::encode(min_code_size,
//! pixels)` / `lzw::decode(min_code_size, src, expected_pixels)` public
//! surface — so a future "smarter LZW" optimisation round can attribute
//! its delta to the codec rather than to the surrounding container code.
//!
//! Each scenario synthesises a deterministic palette-index buffer with
//! `xorshift32`, runs `lzw::encode` once outside the timed region to
//! produce the compressed payload, then alternates `encode_<size>` and
//! `decode_<size>` benchmarks that drive the codec end-to-end.
//!
//! Scenarios (paired encode + decode per row, `Throughput::Bytes(W*H)`
//! so criterion reports MB/s):
//!
//!   - **16x16 / palette=4 / min_code_size=2** — smallest meaningful
//!     payload. Fires the §F.4 width-bump and §F.1 Clear path many
//!     times per pixel because the 2→3→4→… growth happens within tens
//!     of codes.
//!   - **256x256 / palette=64 / min_code_size=6** — mid-size payload
//!     with a 6-bit alphabet. The dictionary saturates (4096 entries)
//!     well before end-of-input, so the cover-sheet "deferred clear"
//!     branch is exercised.
//!   - **1024x1024 / palette=256 / min_code_size=8** — natural-image
//!     stress case at the §22.c.i maximum 8-bit `min_code_size`.
//!     Codes start at 9 bits and reach the §F.4 12-bit ceiling almost
//!     immediately.
//!   - **anim_100x_64x64 / palette=16 / min_code_size=4** — 100
//!     re-keyed 64×64 frames concatenated through 100 independent
//!     `encode` calls (and 100 `decode` calls), simulating the
//!     per-frame fixed-cost portion of animated-GIF compression. Each
//!     frame uses its own xorshift32 seed so the dictionary churn is
//!     realistic rather than degenerate.
//!
//! Run with:
//!     cargo bench -p oxideav-gif --bench lzw
//!
//! For a faster baseline (10s/scenario instead of the default ~30s):
//!     cargo bench -p oxideav-gif --bench lzw -- --quick

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_gif::lzw;

/// Cheap deterministic xorshift32 — keeps the synthesised pixel stream
/// reproducible so a regression run lands on the same bytes every time.
fn xorshift_byte(state: &mut u32) -> u8 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    (*state & 0xff) as u8
}

/// Synthesise a `width × height` palette-index buffer drawn from a
/// `palette_size`-entry palette. The pattern is a smooth gradient with
/// a low-amplitude noise overlay — smooth enough that the LZW
/// dictionary grows past trivial 2-symbol matches, noisy enough that
/// one match never dominates the bytes.
fn build_indices(width: usize, height: usize, palette_size: u16, seed: u32) -> Vec<u8> {
    let mut out = vec![0u8; width * height];
    let mut state: u32 = 0x0001_0001u32.wrapping_mul(seed.wrapping_add(1));
    let modulus = palette_size as u32;
    for r in 0..height {
        for c in 0..width {
            let gradient = ((r * 255) / height.max(1) + (c * 255) / width.max(1)) / 2;
            let noise = xorshift_byte(&mut state) & 0x07;
            let idx = (gradient as u32).wrapping_add(noise as u32) % modulus;
            out[r * width + c] = idx as u8;
        }
    }
    out
}

// ---------------------------------------------------------------------
// 16x16 / palette=4 (min_code_size=2)
// ---------------------------------------------------------------------

fn bench_lzw_encode_16x16(c: &mut Criterion) {
    let pixels = build_indices(16, 16, 4, 0);
    let mut g = c.benchmark_group("lzw_encode_16x16");
    g.throughput(Throughput::Bytes(pixels.len() as u64));
    g.bench_function(BenchmarkId::from_parameter("encode/16x16/pal=4"), |b| {
        b.iter(|| lzw::encode(2, criterion::black_box(&pixels)).expect("lzw::encode"));
    });
    g.finish();
}

fn bench_lzw_decode_16x16(c: &mut Criterion) {
    let pixels = build_indices(16, 16, 4, 0);
    let payload = lzw::encode(2, &pixels).expect("lzw::encode setup");
    let expected = pixels.len();
    let mut g = c.benchmark_group("lzw_decode_16x16");
    g.throughput(Throughput::Bytes(expected as u64));
    g.bench_function(BenchmarkId::from_parameter("decode/16x16/pal=4"), |b| {
        b.iter(|| lzw::decode(2, criterion::black_box(&payload), expected).expect("lzw::decode"));
    });
    g.finish();
}

// ---------------------------------------------------------------------
// 256x256 / palette=64 (min_code_size=6)
// ---------------------------------------------------------------------

fn bench_lzw_encode_256x256(c: &mut Criterion) {
    let pixels = build_indices(256, 256, 64, 0);
    let mut g = c.benchmark_group("lzw_encode_256x256");
    g.throughput(Throughput::Bytes(pixels.len() as u64));
    g.bench_function(BenchmarkId::from_parameter("encode/256x256/pal=64"), |b| {
        b.iter(|| lzw::encode(6, criterion::black_box(&pixels)).expect("lzw::encode"));
    });
    g.finish();
}

fn bench_lzw_decode_256x256(c: &mut Criterion) {
    let pixels = build_indices(256, 256, 64, 0);
    let payload = lzw::encode(6, &pixels).expect("lzw::encode setup");
    let expected = pixels.len();
    let mut g = c.benchmark_group("lzw_decode_256x256");
    g.throughput(Throughput::Bytes(expected as u64));
    g.bench_function(BenchmarkId::from_parameter("decode/256x256/pal=64"), |b| {
        b.iter(|| lzw::decode(6, criterion::black_box(&payload), expected).expect("lzw::decode"));
    });
    g.finish();
}

// ---------------------------------------------------------------------
// 1024x1024 / palette=256 (min_code_size=8)
// ---------------------------------------------------------------------

fn bench_lzw_encode_1024x1024(c: &mut Criterion) {
    let pixels = build_indices(1024, 1024, 256, 0);
    let mut g = c.benchmark_group("lzw_encode_1024x1024");
    g.throughput(Throughput::Bytes(pixels.len() as u64));
    // The 1 MiB raster takes long enough per iteration that the default
    // 100-sample target would blow out the round's bench budget; 20 is
    // plenty for an order-of-magnitude baseline.
    g.sample_size(20);
    g.bench_function(
        BenchmarkId::from_parameter("encode/1024x1024/pal=256"),
        |b| {
            b.iter(|| lzw::encode(8, criterion::black_box(&pixels)).expect("lzw::encode"));
        },
    );
    g.finish();
}

fn bench_lzw_decode_1024x1024(c: &mut Criterion) {
    let pixels = build_indices(1024, 1024, 256, 0);
    let payload = lzw::encode(8, &pixels).expect("lzw::encode setup");
    let expected = pixels.len();
    let mut g = c.benchmark_group("lzw_decode_1024x1024");
    g.throughput(Throughput::Bytes(expected as u64));
    g.sample_size(20);
    g.bench_function(
        BenchmarkId::from_parameter("decode/1024x1024/pal=256"),
        |b| {
            b.iter(|| {
                lzw::decode(8, criterion::black_box(&payload), expected).expect("lzw::decode")
            });
        },
    );
    g.finish();
}

// ---------------------------------------------------------------------
// 100 × (64x64) / palette=16 (min_code_size=4)
// ---------------------------------------------------------------------

const ANIM_FRAME_COUNT: usize = 100;
const ANIM_WIDTH: usize = 64;
const ANIM_HEIGHT: usize = 64;
const ANIM_PAL_SIZE: u16 = 16;
const ANIM_MIN_CODE_SIZE: u8 = 4;

fn build_anim_frames() -> Vec<Vec<u8>> {
    (0..ANIM_FRAME_COUNT)
        .map(|f| build_indices(ANIM_WIDTH, ANIM_HEIGHT, ANIM_PAL_SIZE, f as u32))
        .collect()
}

fn bench_lzw_encode_anim_100x_64x64(c: &mut Criterion) {
    let frames = build_anim_frames();
    let total_pixels = (ANIM_FRAME_COUNT * ANIM_WIDTH * ANIM_HEIGHT) as u64;
    let mut g = c.benchmark_group("lzw_encode_anim_100x_64x64");
    g.throughput(Throughput::Bytes(total_pixels));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("encode/anim/100x_64x64"), |b| {
        b.iter(|| {
            let mut total = 0usize;
            for frame in &frames {
                let payload = lzw::encode(ANIM_MIN_CODE_SIZE, criterion::black_box(frame))
                    .expect("lzw::encode");
                total += payload.len();
            }
            criterion::black_box(total);
        });
    });
    g.finish();
}

fn bench_lzw_decode_anim_100x_64x64(c: &mut Criterion) {
    let frames = build_anim_frames();
    let payloads: Vec<Vec<u8>> = frames
        .iter()
        .map(|f| lzw::encode(ANIM_MIN_CODE_SIZE, f).expect("lzw::encode setup"))
        .collect();
    let frame_pixels = ANIM_WIDTH * ANIM_HEIGHT;
    let total_pixels = (ANIM_FRAME_COUNT * frame_pixels) as u64;
    let mut g = c.benchmark_group("lzw_decode_anim_100x_64x64");
    g.throughput(Throughput::Bytes(total_pixels));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("decode/anim/100x_64x64"), |b| {
        b.iter(|| {
            let mut total = 0usize;
            for payload in &payloads {
                let out = lzw::decode(
                    ANIM_MIN_CODE_SIZE,
                    criterion::black_box(payload),
                    frame_pixels,
                )
                .expect("lzw::decode");
                total += out.len();
            }
            criterion::black_box(total);
        });
    });
    g.finish();
}

criterion_group!(
    benches,
    bench_lzw_encode_16x16,
    bench_lzw_decode_16x16,
    bench_lzw_encode_256x256,
    bench_lzw_decode_256x256,
    bench_lzw_encode_1024x1024,
    bench_lzw_decode_1024x1024,
    bench_lzw_encode_anim_100x_64x64,
    bench_lzw_decode_anim_100x_64x64,
);
criterion_main!(benches);
