//! The registry encoder's `quality` option (round 477, contract r476
//! "quality governs every codec behind a container encoder"): GIF's one
//! meaningful knob is the colour-table budget, so `quality` maps onto
//! `max_colors` (`quality_to_max_colors`) unless `max_colors` is given
//! explicitly.

#![cfg(feature = "registry")]

use oxideav_core::{CodecId, CodecParameters, Frame as CoreFrame, PixelFormat, VideoFrame};
use oxideav_gif::{make_encoder, parse, quality_to_max_colors, GifImage, CODEC_ID_STR};

const W: u32 = 17;
const H: u32 = 16;

/// 272 distinct colours → any budget below 272 quantises.
fn picture() -> VideoFrame {
    let mut rgb = Vec::with_capacity((W * H * 3) as usize);
    for i in 0..(W * H) {
        rgb.extend_from_slice(&[(i & 0xFF) as u8, ((i >> 4) & 0xFF) as u8, (i * 7) as u8]);
    }
    VideoFrame::from(GifImage::from_rgb8(W, H, rgb).unwrap())
}

fn encode_with(options: &[(&str, &str)]) -> Vec<u8> {
    let mut p = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    p.width = Some(W);
    p.height = Some(H);
    p.pixel_format = Some(PixelFormat::Rgb24);
    for (k, v) in options {
        p.options.insert(*k, *v);
    }
    let mut enc = make_encoder(&p).unwrap();
    enc.send_frame(&CoreFrame::Video(picture())).unwrap();
    enc.receive_packet().unwrap().data
}

/// Distinct colours the decoded picture uses (the wire table is padded
/// to a power of two, so its length is not the budget).
fn colours(bytes: &[u8]) -> usize {
    let rgb = oxideav_gif::decode(bytes).unwrap().to_rgb8();
    let set: std::collections::BTreeSet<[u8; 3]> =
        rgb.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
    set.len()
}

fn table_len(bytes: &[u8]) -> usize {
    parse(bytes).unwrap().global_palette.unwrap().len()
}

#[test]
fn quality_maps_monotonically_onto_the_colour_budget() {
    assert_eq!(quality_to_max_colors(100), 256);
    assert_eq!(quality_to_max_colors(90), 230);
    assert_eq!(quality_to_max_colors(50), 128);
    assert_eq!(quality_to_max_colors(10), 26);
    assert_eq!(quality_to_max_colors(1), 3);
    assert_eq!(quality_to_max_colors(0), 2);
    assert_eq!(quality_to_max_colors(500), 256, "clamped");
    let mut last = 0;
    for q in 0..=100 {
        let n = quality_to_max_colors(q);
        assert!(n >= last, "monotone at {q}");
        last = n;
    }
}

#[test]
fn quality_changes_the_table_and_the_bytes() {
    let q100 = encode_with(&[("quality", "100")]);
    let q90 = encode_with(&[("quality", "90")]);
    let q50 = encode_with(&[("quality", "50")]);
    let q10 = encode_with(&[("quality", "10")]);
    let default = encode_with(&[]);
    assert_eq!(q100, default, "100 = the default budget");
    assert_eq!(table_len(&q100), 256);
    assert_eq!(colours(&q100), 256, "272 source colours → the full budget");
    assert!(
        colours(&q90) <= 230 && colours(&q90) > 128,
        "{}",
        colours(&q90)
    );
    assert!(
        colours(&q50) <= 128 && colours(&q50) > 26,
        "{}",
        colours(&q50)
    );
    assert!(
        colours(&q10) <= 26 && colours(&q10) > 2,
        "{}",
        colours(&q10)
    );
    assert!(
        table_len(&q50) <= 128 && table_len(&q10) <= 32,
        "wire tables shrink too"
    );
    assert_ne!(q100, q90);
    assert_ne!(q90, q50);
    assert_ne!(q50, q10);
    assert!(q10.len() < q100.len(), "fewer colours, smaller file");
    // Every one decodes to the right geometry.
    for b in [&q100, &q90, &q50, &q10] {
        let img = oxideav_gif::decode(b).unwrap();
        assert_eq!((img.width, img.height), (W, H));
    }
}

#[test]
fn explicit_max_colors_wins_over_quality_in_either_order() {
    let a = encode_with(&[("quality", "50"), ("max_colors", "8")]);
    let b = encode_with(&[("max_colors", "8"), ("quality", "50")]);
    assert_eq!(a, b);
    assert_eq!(table_len(&a), 8);
    // And a quality of 100 does not widen an explicit budget.
    let c = encode_with(&[("max_colors", "4"), ("quality", "100")]);
    assert_eq!(table_len(&c), 4);
}

#[test]
fn quality_out_of_range_is_refused_at_the_factory() {
    let mut p = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    p.width = Some(1);
    p.height = Some(1);
    p.options.insert("quality", "101");
    assert!(make_encoder(&p).is_err());
    p.options.insert("quality", "-1");
    assert!(make_encoder(&p).is_err());
    p.options.insert("quality", "0");
    assert!(make_encoder(&p).is_ok());
}

#[test]
fn quality_is_declared_in_the_schema() {
    use oxideav_core::CodecOptionsStruct;
    let names: Vec<&str> = oxideav_gif::EncodeOptions::SCHEMA
        .iter()
        .map(|f| f.name)
        .collect();
    assert!(names.contains(&"quality"));
    assert!(names.contains(&"max_colors"));
}
