//! Criterion benchmarks for the GIF decoder hot paths.
//!
//! Round 129 (depth-mode benchmarks): `oxideav-gif` has hit the
//! per-codec saturation point so per the workspace "saturated →
//! fuzz/bench/profile" memo this round wires up `criterion` benches to
//! let future optimisation rounds A/B-test their changes. This file
//! covers the **decoder**; sibling files cover `encode` (the
//! `AnimationBuilder` + `encode` path) and `roundtrip` (build →
//! `encode` → `decode` end-to-end).
//!
//! Each scenario is self-contained: the bench synthesises a GIF on the
//! fly with `AnimationBuilder` + `encode`, then iterates `decode` (or a
//! companion entry point) on the encoded bytes. No `docs/` fixtures
//! and no external files are read.
//!
//!   - **decode_still_320x240_256pal**: 320×240 single-frame still with
//!     a 256-entry palette — the "natural-image" decode-side baseline.
//!     Exercises §17 Header, §18 Logical Screen Descriptor, §19 Global
//!     Color Table, §20 Image Descriptor, §22 Table-Based Image Data
//!     (LZW Appendix F at the maximum 8-bit `min_code_size`).
//!   - **decode_still_64x64_8pal**: 64×64 single-frame still with an
//!     8-entry palette (the smallest legitimate non-degenerate
//!     animation cell). Measures per-frame fixed overhead at small
//!     code-size (3-bit `min_code_size`); the LZW table-growth state
//!     machine fires once per code rather than once every dozen.
//!   - **decode_anim_64x64_8f**: 8-frame 64×64 animation with a
//!     NETSCAPE2.0 loop block — exercises §23 Graphic Control nesting
//!     and §26 Application Extension parsing on top of the per-frame
//!     image-decoder cost.
//!   - **decode_lenient_anim_64x64_8f**: same animation, but routed
//!     through `decode_lenient` — different state machine (skips
//!     malformed sub-blocks instead of returning `Err`), so the
//!     bench measures the resync path's per-frame overhead even when
//!     no resync is needed.
//!   - **decode_first_frame_anim_64x64_8f**: same animation, decoded
//!     through the cover-frame fast-path `decode_first_frame` —
//!     short-circuits at the first image-bearing block. Should be
//!     materially faster than `decode` on multi-frame input.
//!
//! Run with:
//!     cargo bench -p oxideav-gif --bench decode

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_gif::{
    decode, decode_first_frame, decode_lenient, encode, image::Rgb, AnimationBuilder,
    DisposalMethod,
};

/// Cheap deterministic xorshift32 — keeps the synthesised pixel stream
/// reproducible so a regression run lands on the same bytes every time.
fn xorshift_byte(state: &mut u32) -> u8 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    (*state & 0xff) as u8
}

/// Synthesise a `width × height` palette-index buffer drawn from a
/// `palette_size`-entry palette. Indices are a noisy gradient — smooth
/// enough that the LZW dictionary grows past trivial 2-symbol matches,
/// noisy enough that one match never dominates the bytes.
fn build_indices(width: usize, height: usize, palette_size: u8) -> Vec<u8> {
    let mut out = vec![0u8; width * height];
    let mut state: u32 = 0x1234_5678;
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

/// Build a `palette_size`-entry RGB palette — a smooth ramp so the
/// LZW codec sees a realistic alphabet.
fn build_palette(palette_size: u8) -> Vec<Rgb> {
    (0..palette_size)
        .map(|i| {
            let v = ((i as u32 * 255) / palette_size.max(1) as u32) as u8;
            Rgb::new(v, v.wrapping_add(64), v.wrapping_add(128))
        })
        .collect()
}

/// Encode a still GIF of `width × height` using a palette of
/// `palette_size` entries. Returns the GIF bytes ready for `decode`.
fn build_still(width: u16, height: u16, palette_size: u8) -> Vec<u8> {
    let palette = build_palette(palette_size);
    let indices = build_indices(width as usize, height as usize, palette_size);
    let img = AnimationBuilder::new(width, height, palette)
        .add_full_frame(indices, 0, DisposalMethod::None)
        .expect("add still frame")
        .build()
        .expect("build still");
    encode(&img).expect("encode still")
}

/// Encode an animation of `frame_count` frames at `width × height`
/// drawn from a shared `palette_size`-entry palette. Each frame
/// carries a 4-centisecond delay; the animation loops forever
/// (NETSCAPE2.0 `loop_count = 0`).
fn build_animation(width: u16, height: u16, palette_size: u8, frame_count: u32) -> Vec<u8> {
    let palette = build_palette(palette_size);
    let mut builder = AnimationBuilder::new(width, height, palette).loop_forever();
    for f in 0..frame_count {
        // Re-seed the per-frame xorshift state with `f` so frames are
        // different but reproducible.
        let mut state: u32 = 0x0001_0001u32.wrapping_mul(f.wrapping_add(1));
        let mut indices = vec![0u8; width as usize * height as usize];
        let modulus = palette_size as u32;
        for r in 0..height as usize {
            for c in 0..width as usize {
                let gradient = ((r * 255) / height as usize + (c * 255) / width as usize) / 2;
                let noise = xorshift_byte(&mut state) & 0x07;
                let idx = (gradient as u32).wrapping_add(noise as u32) % modulus;
                indices[r * width as usize + c] = idx as u8;
            }
        }
        builder = builder
            .add_full_frame(indices, 4, DisposalMethod::RestoreBackground)
            .expect("add animation frame");
    }
    let img = builder.build().expect("build animation");
    encode(&img).expect("encode animation")
}

fn bench_decode_still_320x240_256pal(c: &mut Criterion) {
    let bytes = build_still(320, 240, 255);
    let mut g = c.benchmark_group("decode_still_320x240_256pal");
    g.throughput(Throughput::Bytes((320 * 240) as u64));
    g.bench_function(BenchmarkId::from_parameter("still/320x240/pal=255"), |b| {
        b.iter(|| decode(criterion::black_box(&bytes)).expect("decode"));
    });
    g.finish();
}

fn bench_decode_still_64x64_8pal(c: &mut Criterion) {
    let bytes = build_still(64, 64, 8);
    let mut g = c.benchmark_group("decode_still_64x64_8pal");
    g.throughput(Throughput::Bytes((64 * 64) as u64));
    g.bench_function(BenchmarkId::from_parameter("still/64x64/pal=8"), |b| {
        b.iter(|| decode(criterion::black_box(&bytes)).expect("decode"));
    });
    g.finish();
}

fn bench_decode_anim_64x64_8f(c: &mut Criterion) {
    let bytes = build_animation(64, 64, 16, 8);
    let mut g = c.benchmark_group("decode_anim_64x64_8f");
    // Throughput = total source bytes across all frames.
    g.throughput(Throughput::Bytes((64 * 64 * 8) as u64));
    g.bench_function(BenchmarkId::from_parameter("anim/64x64/8f"), |b| {
        b.iter(|| decode(criterion::black_box(&bytes)).expect("decode"));
    });
    g.finish();
}

fn bench_decode_lenient_anim_64x64_8f(c: &mut Criterion) {
    let bytes = build_animation(64, 64, 16, 8);
    let mut g = c.benchmark_group("decode_lenient_anim_64x64_8f");
    g.throughput(Throughput::Bytes((64 * 64 * 8) as u64));
    g.bench_function(BenchmarkId::from_parameter("anim-lenient/64x64/8f"), |b| {
        b.iter(|| decode_lenient(criterion::black_box(&bytes)).expect("decode_lenient"));
    });
    g.finish();
}

fn bench_decode_first_frame_anim_64x64_8f(c: &mut Criterion) {
    let bytes = build_animation(64, 64, 16, 8);
    let mut g = c.benchmark_group("decode_first_frame_anim_64x64_8f");
    // First-frame fast-path: only the first 64×64 frame is touched.
    g.throughput(Throughput::Bytes((64 * 64) as u64));
    g.bench_function(BenchmarkId::from_parameter("first-frame/64x64/8f"), |b| {
        b.iter(|| decode_first_frame(criterion::black_box(&bytes)).expect("decode_first_frame"));
    });
    g.finish();
}

criterion_group!(
    benches,
    bench_decode_still_320x240_256pal,
    bench_decode_still_64x64_8pal,
    bench_decode_anim_64x64_8f,
    bench_decode_lenient_anim_64x64_8f,
    bench_decode_first_frame_anim_64x64_8f,
);
criterion_main!(benches);
