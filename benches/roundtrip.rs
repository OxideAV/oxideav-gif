//! Criterion benchmarks for the GIF encoder + decoder roundtrip — the
//! realistic "build an animation, encode it, decode every frame" path.
//!
//! Round 129 (depth-mode benchmarks): real consumers don't run the
//! encoder and decoder in isolation; they encode-then-decode (e.g. as a
//! sanity check) or build → encode → compose for playback. These
//! benches drive the full pipeline so a later optimisation round that
//! makes only one half faster can be evaluated on the integrated cost.
//!
//! Scenarios (one bench per line below):
//!
//!   - `roundtrip_still_320x240_256pal` — build a 320×240 still, `encode`
//!     it, `decode` the bytes, and `compose` the result onto an RGBA
//!     canvas. The largest single-frame fixture.
//!   - `roundtrip_anim_64x64_8f` — build an 8-frame 64×64 animation,
//!     encode, decode, compose. Exercises the §23 disposal-method state
//!     machine on every iteration.
//!   - `roundtrip_anim_64x64_8f_playback` — same animation but driven
//!     through the lazy `Playback::frames` iterator (one canvas at a
//!     time) rather than the eager `compose`. The two paths share the
//!     compositor but differ in allocation discipline (eager `Vec` vs
//!     per-frame iterator).
//!   - `build_encode_decode_only_anim_64x64_8f` — build → encode →
//!     decode without compose. Isolates the pure-bytes pipeline from the
//!     compositor cost so a future "decoder got 2× faster" change can be
//!     attributed to the bytes path rather than to compose.
//!
//! Run with:
//!     cargo bench -p oxideav-gif --bench roundtrip

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_gif::{
    compose, decode, encode, image::Rgb, playback::Playback, AnimationBuilder, DisposalMethod,
};

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

fn bench_roundtrip_still_320x240_256pal(c: &mut Criterion) {
    let palette = build_palette(255);
    let indices = build_indices(320, 240, 255, 0);
    let img = AnimationBuilder::new(320, 240, palette)
        .add_full_frame(indices, 0, DisposalMethod::None)
        .expect("add still frame")
        .build()
        .expect("build still");

    let mut g = c.benchmark_group("roundtrip_still_320x240_256pal");
    g.throughput(Throughput::Bytes((320 * 240) as u64));
    g.sample_size(10);
    g.bench_function(BenchmarkId::from_parameter("still/320x240/pal=255"), |b| {
        b.iter(|| {
            let bytes = encode(criterion::black_box(&img)).expect("encode");
            let decoded = decode(&bytes).expect("decode");
            compose(&decoded).expect("compose")
        });
    });
    g.finish();
}

fn bench_roundtrip_anim_64x64_8f(c: &mut Criterion) {
    let palette = build_palette(16);
    let mut builder = AnimationBuilder::new(64, 64, palette).loop_forever();
    for f in 0..8u32 {
        let indices = build_indices(64, 64, 16, f);
        builder = builder
            .add_full_frame(indices, 4, DisposalMethod::RestoreBackground)
            .expect("add animation frame");
    }
    let img = builder.build().expect("build animation");

    let mut g = c.benchmark_group("roundtrip_anim_64x64_8f");
    g.throughput(Throughput::Bytes((64 * 64 * 8) as u64));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("anim/64x64/8f"), |b| {
        b.iter(|| {
            let bytes = encode(criterion::black_box(&img)).expect("encode");
            let decoded = decode(&bytes).expect("decode");
            compose(&decoded).expect("compose")
        });
    });
    g.finish();
}

fn bench_roundtrip_anim_64x64_8f_playback(c: &mut Criterion) {
    let palette = build_palette(16);
    let mut builder = AnimationBuilder::new(64, 64, palette).loop_forever();
    for f in 0..8u32 {
        let indices = build_indices(64, 64, 16, f);
        builder = builder
            .add_full_frame(indices, 4, DisposalMethod::RestoreBackground)
            .expect("add animation frame");
    }
    let img = builder.build().expect("build animation");

    let mut g = c.benchmark_group("roundtrip_anim_64x64_8f_playback");
    g.throughput(Throughput::Bytes((64 * 64 * 8) as u64));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("anim-playback/64x64/8f"), |b| {
        b.iter(|| {
            let bytes = encode(criterion::black_box(&img)).expect("encode");
            let decoded = decode(&bytes).expect("decode");
            let pb = Playback::new(&decoded);
            let mut n = 0usize;
            for _frame in pb.frames() {
                n += 1;
            }
            n
        });
    });
    g.finish();
}

fn bench_build_encode_decode_only_anim_64x64_8f(c: &mut Criterion) {
    // Bytes-only pipeline — no compose, no playback. Pre-compute the
    // index buffers so the per-iteration cost is the actual build +
    // encode + decode work, not the synthetic-data pass.
    let palette = build_palette(16);
    let frames: Vec<Vec<u8>> = (0..8)
        .map(|f| build_indices(64, 64, 16, f as u32))
        .collect();

    let mut g = c.benchmark_group("build_encode_decode_only_anim_64x64_8f");
    g.throughput(Throughput::Bytes((64 * 64 * 8) as u64));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("bytes-only/64x64/8f"), |b| {
        b.iter(|| {
            let mut builder = AnimationBuilder::new(64, 64, palette.clone()).loop_forever();
            for indices in &frames {
                builder = builder
                    .add_full_frame(indices.clone(), 4, DisposalMethod::RestoreBackground)
                    .expect("add animation frame");
            }
            let img = builder.build().expect("build");
            let bytes = encode(&img).expect("encode");
            decode(&bytes).expect("decode")
        });
    });
    g.finish();
}

criterion_group!(
    benches,
    bench_roundtrip_still_320x240_256pal,
    bench_roundtrip_anim_64x64_8f,
    bench_roundtrip_anim_64x64_8f_playback,
    bench_build_encode_decode_only_anim_64x64_8f,
);
criterion_main!(benches);
