//! Encoder quality / size / speed report on synthetic pictures and
//! animations. With `OXIDEAV_GIF_REPORT=1` it prints the full tables
//! (run in release: `cargo test --release --test encode_report --
//! --nocapture`); without it the same code runs at a small size as a
//! smoke test.

#[path = "support/synth.rs"]
mod synth;

use std::time::Duration;

use oxideav_gif::quantize::{ColorMetric, Dither, QuantizeOptions};
use oxideav_gif::{EncodeOptions, Frame, GifImage};

fn full() -> bool {
    std::env::var_os("OXIDEAV_GIF_REPORT").is_some()
}

fn still_configs() -> Vec<(&'static str, EncodeOptions)> {
    let q = QuantizeOptions::default;
    let with = |q: QuantizeOptions| EncodeOptions::default().with_quantize(q);
    vec![
        ("median cut, no dither", with(QuantizeOptions::median_cut())),
        (
            "median cut, error diffusion",
            with(QuantizeOptions::median_cut().dither(Dither::FloydSteinberg)),
        ),
        (
            "variance split, RGB metric",
            with(q().metric(ColorMetric::Rgb).dither(Dither::None)),
        ),
        ("no dither", with(q().dither(Dither::None))),
        ("error diffusion", with(q().dither(Dither::FloydSteinberg))),
        (
            "error diffusion 50 %",
            with(q().dither(Dither::FloydSteinberg).dither_strength(50)),
        ),
        ("ordered 8x8", with(q().dither(Dither::OrderedBayer8x8))),
        ("blue noise", with(q().dither(Dither::BlueNoise))),
        (
            "blue noise 50 %",
            with(q().dither(Dither::BlueNoise).dither_strength(50)),
        ),
        ("default", EncodeOptions::default()),
    ]
}

#[test]
fn stills_report() {
    let (w, h) = if full() { (640, 480) } else { (96, 72) };
    let runs = if full() { 5 } else { 1 };
    let pictures: Vec<(&str, Vec<u8>)> = vec![
        ("photo", synth::photo(w, h, 1)),
        ("gradient", synth::gradient(w, h)),
        ("flat art", synth::flat_art(w, h)),
        ("screen", synth::screen(w, h, 2)),
    ];
    let mp = (w * h) as f64 / 1e6;
    println!(
        "| picture | settings | bytes | PSNR dB | mean dE | mean dE low-pass | encode MP/s | decode MP/s |"
    );
    println!("|---|---|---|---|---|---|---|---|");
    for (name, rgb) in &pictures {
        for (cfg, opts) in still_configs() {
            let bytes = oxideav_gif::encode_rgb8(w as u32, h as u32, rgb, &opts).unwrap();
            let enc = synth::median_secs(runs, || {
                oxideav_gif::encode_rgb8(w as u32, h as u32, rgb, &opts).unwrap()
            });
            let dec = synth::median_secs(runs, || oxideav_gif::decode_rgb8(&bytes).unwrap());
            let back = oxideav_gif::decode_rgb8(&bytes).unwrap();
            let back = back.as_bytes();
            assert_eq!(back.len(), rgb.len());
            println!(
                "| {name} | {cfg} | {} | {:.2} | {:.3} | {:.3} | {:.1} | {:.0} |",
                bytes.len(),
                synth::psnr(rgb, back),
                synth::mean_delta_e(rgb, back),
                synth::mean_delta_e_low_pass(rgb, back, w, h),
                mp / enc,
                mp / dec,
            );
        }
    }
}

fn frames_of(raw: &[Vec<u8>], w: usize, h: usize) -> Vec<Frame> {
    raw.iter()
        .map(|f| {
            Frame::new(
                GifImage::from_rgba8(w as u32, h as u32, f.clone()).unwrap(),
                Some(Duration::from_millis(40)),
            )
        })
        .collect()
}

fn anim_configs() -> Vec<(&'static str, EncodeOptions)> {
    vec![("default", EncodeOptions::default())]
}

#[test]
fn animations_report() {
    let (w, h, n) = if full() { (320, 240, 48) } else { (64, 48, 8) };
    let runs = if full() { 3 } else { 1 };
    let anims: Vec<(&str, Vec<Vec<u8>>)> = vec![
        ("screen recording", synth::anim_screen(w, h, n)),
        ("cartoon", synth::anim_cartoon(w, h, n)),
        ("video", synth::anim_video(w, h, n)),
    ];
    let mp = (w * h * n) as f64 / 1e6;
    println!("| animation | settings | bytes | PSNR dB | mean dE | encode MP/s | decode MP/s |");
    println!("|---|---|---|---|---|---|---|");
    for (name, raw) in &anims {
        let frames = frames_of(raw, w, h);
        for (cfg, opts) in anim_configs() {
            let bytes = oxideav_gif::encode_all(&frames, &opts).unwrap();
            let enc = synth::median_secs(runs, || oxideav_gif::encode_all(&frames, &opts).unwrap());
            let dec = synth::median_secs(runs, || oxideav_gif::decode_all(&bytes).unwrap());
            let back = oxideav_gif::decode_all(&bytes).unwrap();
            // Displayed pictures over time: compare at each source frame's
            // start time (merged duplicates shorten the frame list).
            let mut src_rgb = Vec::new();
            let mut out_rgb = Vec::new();
            let total: Duration = back.iter().map(|f| f.delay.unwrap_or_default()).sum();
            assert_eq!(
                total,
                Duration::from_millis(40 * raw.len() as u64),
                "{name} {cfg}"
            );
            let mut bi = 0usize;
            let mut b_end = back[0].delay.unwrap_or_default();
            for (i, src) in raw.iter().enumerate() {
                let t = Duration::from_millis(40 * i as u64);
                while t >= b_end && bi + 1 < back.len() {
                    bi += 1;
                    b_end += back[bi].delay.unwrap_or_default();
                }
                let shown = back[bi].image.to_rgba8();
                for (p, q) in src.as_chunks::<4>().0.iter().zip(shown.as_chunks::<4>().0) {
                    src_rgb.extend_from_slice(&p[..3]);
                    out_rgb.extend_from_slice(&q[..3]);
                }
            }
            println!(
                "| {name} | {cfg} | {} | {:.2} | {:.3} | {:.1} | {:.0} |",
                bytes.len(),
                synth::psnr(&src_rgb, &out_rgb),
                synth::mean_delta_e(&src_rgb, &out_rgb),
                mp / enc,
                mp / dec,
            );
        }
    }
}
