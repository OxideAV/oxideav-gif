//! LZW size and speed report on index planes of the synthetic pictures.
//! With `OXIDEAV_GIF_REPORT=1` it prints the tables (run in release);
//! without it the same code runs at a small size as a smoke test.

#[path = "support/synth.rs"]
mod synth;

use oxideav_gif::lzw;
use oxideav_gif::quantize::{Dither, QuantizeOptions, quantize_rgb_with_options};

fn full() -> bool {
    std::env::var_os("OXIDEAV_GIF_REPORT").is_some()
}

fn planes(w: usize, h: usize) -> Vec<(&'static str, u8, Vec<u8>)> {
    let q = |rgb: &[u8], dither: Dither| {
        let q = quantize_rgb_with_options(rgb, w, h, QuantizeOptions::default().dither(dither))
            .unwrap();
        let bits = (usize::BITS - (q.palette.len() - 1).leading_zeros()).clamp(2, 8) as u8;
        (bits, q.indices)
    };
    let photo = synth::photo(w, h, 1);
    let screen = synth::screen(w, h, 2);
    let flat = synth::flat_art(w, h);
    let gradient = synth::gradient(w, h);
    let (pb, p) = q(&photo, Dither::BlueNoise);
    let (sb, s) = q(&screen, Dither::None);
    let (fb, f) = q(&flat, Dither::None);
    let (gb, g) = q(&gradient, Dither::None);
    // A picture whose statistics change half-way: screen rows, then
    // photograph rows.
    let mut mixed = s[..s.len() / 2].to_vec();
    mixed.extend_from_slice(&p[p.len() / 2..]);
    vec![
        ("photo, dithered", pb, p),
        ("gradient", gb, g),
        ("screen", sb, s),
        ("flat art", fb, f),
        ("screen then photo", pb.max(sb), mixed),
        ("one colour", 2, vec![1u8; w * h]),
    ]
}

type Encode = fn(u8, &[u8]) -> oxideav_gif::Result<Vec<u8>>;

fn strategies() -> Vec<(&'static str, Encode)> {
    vec![
        ("deferred clear", lzw::encode as Encode),
        ("clear on full", lzw::encode_with_clear_on_full as Encode),
        ("monitored clear", |bits, px| {
            lzw::encode_with(bits, px, lzw::ClearPolicy::Monitored)
        }),
    ]
}

#[test]
fn lzw_report() {
    let (w, h) = if full() { (1024, 768) } else { (96, 72) };
    let runs = if full() { 9 } else { 1 };
    let mp = (w * h) as f64 / 1e6;
    println!("| index plane | strategy | bytes | encode MP/s | decode MP/s |");
    println!("|---|---|---|---|---|");
    for (name, bits, plane) in planes(w, h) {
        for (label, encode) in strategies() {
            let packed = encode(bits, &plane).unwrap();
            assert_eq!(lzw::decode(bits, &packed, plane.len()).unwrap(), plane);
            let enc = synth::median_secs(runs, || encode(bits, &plane).unwrap());
            let dec = synth::median_secs(runs, || lzw::decode(bits, &packed, plane.len()).unwrap());
            println!(
                "| {name} | {label} | {} | {:.0} | {:.0} |",
                packed.len(),
                mp / enc,
                mp / dec
            );
        }
    }
}
