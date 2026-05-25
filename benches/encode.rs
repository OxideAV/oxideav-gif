//! Criterion benchmarks for the GIF encoder hot paths.
//!
//! Round 129 (depth-mode benchmarks): the encoder's per-frame cost is
//! dominated by Appendix F LZW compression (variable-width-code table
//! growth + sub-block-chain framing) and §22 Table-Based Image Data
//! emission. These benches make those costs measurable so future
//! "smarter LZW" or "smarter sub-block sizing" rounds can A/B against
//! the round-129 baseline.
//!
//! Scenarios:
//!
//!   - **encode_still_320x240_256pal**: 320×240 still with a 256-entry
//!     palette — the natural-image encode-side baseline. Exercises LZW
//!     at the maximum 8-bit `min_code_size`.
//!   - **encode_still_64x64_8pal**: 64×64 still at 3-bit `min_code_size`
//!     — measures per-call fixed overhead (header + LSD + GCT + LZW
//!     setup) plus a tiny IDAT.
//!   - **encode_anim_64x64_8f**: 8-frame 64×64 animation with one
//!     NETSCAPE2.0 *Looping* sub-block — exercises the per-frame §23
//!     Graphic Control Extension emit path on top of the LZW pass.
//!   - **encode_anim_320x240_4f**: 4-frame 320×240 animation — the
//!     larger encode-side multi-frame fixture, dominated by LZW.
//!   - **build_anim_320x240_4f**: just the `AnimationBuilder::build`
//!     validation pass (no `encode`) — isolates the index/palette/
//!     placement validation cost from the LZW serialiser, so an
//!     "encoder got faster" change can be attributed correctly.
//!
//! Run with:
//!     cargo bench -p oxideav-gif --bench encode

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_gif::{encode, image::Rgb, AnimationBuilder, DisposalMethod};

fn xorshift_byte(state: &mut u32) -> u8 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    (*state & 0xff) as u8
}

fn build_palette(palette_size: u8) -> Vec<Rgb> {
    (0..palette_size)
        .map(|i| {
            let v = ((i as u32 * 255) / palette_size.max(1) as u32) as u8;
            Rgb::new(v, v.wrapping_add(64), v.wrapping_add(128))
        })
        .collect()
}

fn build_indices(width: usize, height: usize, palette_size: u8, seed: u32) -> Vec<u8> {
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

fn bench_encode_still_320x240_256pal(c: &mut Criterion) {
    let palette = build_palette(255);
    let indices = build_indices(320, 240, 255, 0);
    let img = AnimationBuilder::new(320, 240, palette)
        .add_full_frame(indices, 0, DisposalMethod::None)
        .expect("add still frame")
        .build()
        .expect("build still");

    let mut g = c.benchmark_group("encode_still_320x240_256pal");
    g.throughput(Throughput::Bytes((320 * 240) as u64));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("still/320x240/pal=255"), |b| {
        b.iter(|| encode(criterion::black_box(&img)).expect("encode"));
    });
    g.finish();
}

fn bench_encode_still_64x64_8pal(c: &mut Criterion) {
    let palette = build_palette(8);
    let indices = build_indices(64, 64, 8, 0);
    let img = AnimationBuilder::new(64, 64, palette)
        .add_full_frame(indices, 0, DisposalMethod::None)
        .expect("add still frame")
        .build()
        .expect("build still");

    let mut g = c.benchmark_group("encode_still_64x64_8pal");
    g.throughput(Throughput::Bytes((64 * 64) as u64));
    g.bench_function(BenchmarkId::from_parameter("still/64x64/pal=8"), |b| {
        b.iter(|| encode(criterion::black_box(&img)).expect("encode"));
    });
    g.finish();
}

fn bench_encode_anim_64x64_8f(c: &mut Criterion) {
    let palette = build_palette(16);
    let mut builder = AnimationBuilder::new(64, 64, palette).loop_forever();
    for f in 0..8u32 {
        let indices = build_indices(64, 64, 16, f);
        builder = builder
            .add_full_frame(indices, 4, DisposalMethod::RestoreBackground)
            .expect("add animation frame");
    }
    let img = builder.build().expect("build animation");

    let mut g = c.benchmark_group("encode_anim_64x64_8f");
    g.throughput(Throughput::Bytes((64 * 64 * 8) as u64));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("anim/64x64/8f"), |b| {
        b.iter(|| encode(criterion::black_box(&img)).expect("encode"));
    });
    g.finish();
}

fn bench_encode_anim_320x240_4f(c: &mut Criterion) {
    let palette = build_palette(255);
    let mut builder = AnimationBuilder::new(320, 240, palette).loop_forever();
    for f in 0..4u32 {
        let indices = build_indices(320, 240, 255, f);
        builder = builder
            .add_full_frame(indices, 4, DisposalMethod::RestoreBackground)
            .expect("add animation frame");
    }
    let img = builder.build().expect("build animation");

    let mut g = c.benchmark_group("encode_anim_320x240_4f");
    g.throughput(Throughput::Bytes((320 * 240 * 4) as u64));
    g.sample_size(10);
    g.bench_function(BenchmarkId::from_parameter("anim/320x240/4f"), |b| {
        b.iter(|| encode(criterion::black_box(&img)).expect("encode"));
    });
    g.finish();
}

fn bench_build_anim_320x240_4f(c: &mut Criterion) {
    // Just the validation pass — no LZW. Pre-compute the four index
    // buffers so the bench measures placement/index/palette validation
    // rather than the synthetic-data pass.
    let palette = build_palette(255);
    let frames: Vec<Vec<u8>> = (0..4)
        .map(|f| build_indices(320, 240, 255, f as u32))
        .collect();

    let mut g = c.benchmark_group("build_anim_320x240_4f");
    g.throughput(Throughput::Bytes((320 * 240 * 4) as u64));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("build/320x240/4f"), |b| {
        b.iter(|| {
            let mut builder = AnimationBuilder::new(320, 240, palette.clone()).loop_forever();
            for indices in &frames {
                builder = builder
                    .add_full_frame(indices.clone(), 4, DisposalMethod::RestoreBackground)
                    .expect("add animation frame");
            }
            builder.build().expect("build")
        });
        let _ = criterion::black_box(&frames);
    });
    g.finish();
}

criterion_group!(
    benches,
    bench_encode_still_320x240_256pal,
    bench_encode_still_64x64_8pal,
    bench_encode_anim_64x64_8f,
    bench_encode_anim_320x240_4f,
    bench_build_anim_320x240_4f,
);
criterion_main!(benches);
