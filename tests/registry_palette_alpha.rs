//! The palette-alpha side-channel record on the registry path (round
//! 477, core 0.1.40): a still with a §23.c.viii Transparency Index
//! stays `Pal8` and its transparency rides `VideoFrame::palette_alpha`
//! (`0` at that entry, `255` everywhere else); the registry encoder
//! maps the record back to a Transparency Index by the
//! `ALPHA_OPAQUE_THRESHOLD` rule.

#![cfg(feature = "registry")]

use oxideav_core::{
    CodecId, CodecParameters, Frame as CoreFrame, Packet, PixelFormat, TimeBase, VideoFrame,
    VideoPlane,
};
use oxideav_gif::quantize::ALPHA_OPAQUE_THRESHOLD;
use oxideav_gif::registry::{palette_alpha_record, palette_from_frame};
use oxideav_gif::{
    compose, decode, encode, make_decoder, make_encoder, parse, EncodeOptions, GifImage, Palette,
    CODEC_ID_STR,
};

fn still_params(w: u32, h: u32, pf: PixelFormat) -> CodecParameters {
    let mut p = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    p.width = Some(w);
    p.height = Some(h);
    p.pixel_format = Some(pf);
    p
}

/// Decode `bytes` (one still) through the registry decoder.
fn registry_decode(bytes: Vec<u8>) -> VideoFrame {
    let params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    let mut dec = make_decoder(&params).unwrap();
    dec.send_packet(&Packet::new(0u32, TimeBase::new(1, 1), bytes))
        .unwrap();
    let CoreFrame::Video(v) = dec.receive_frame().unwrap() else {
        panic!("non-video frame");
    };
    v
}

/// Encode one `Pal8` frame (palette + optional alpha record) through
/// the registry encoder and return the packet bytes.
fn registry_encode_pal8(
    w: u32,
    h: u32,
    indices: Vec<u8>,
    rgb: Vec<u8>,
    alpha: Option<Vec<u8>>,
) -> Vec<u8> {
    let params = still_params(w, h, PixelFormat::Pal8);
    let mut enc = make_encoder(&params).unwrap();
    let mut vf = VideoFrame {
        pts: Some(0),
        planes: vec![VideoPlane {
            stride: w as usize,
            data: indices,
        }],
    };
    vf.set_palette(rgb);
    if let Some(a) = alpha {
        vf.set_palette_alpha(a);
    }
    enc.send_frame(&CoreFrame::Video(vf)).unwrap();
    enc.receive_packet().unwrap().data
}

#[test]
fn still_with_transparency_index_decodes_as_pal8_with_alpha_record() {
    // 3×1: red, transparent, blue — Layer 1 keeps it indexed with the
    // transparent entry at alpha 0.
    let img = GifImage::from_rgba8(3, 1, vec![255, 0, 0, 255, 7, 7, 7, 0, 0, 0, 255, 255]).unwrap();
    let bytes = encode(&img, &EncodeOptions::default()).unwrap();
    let expect = decode(&bytes).unwrap();
    assert_eq!(expect.format, oxideav_gif::PixelFormat::Pal8);
    let pal = expect.palette.as_ref().unwrap();
    let ti = pal
        .transparent_index()
        .expect("Layer 1 palette has the transparent entry");

    let v = registry_decode(bytes);
    assert_eq!(v.planes[0].stride, 3, "still stays Pal8");
    let record = v.palette_alpha().expect("alpha record attached");
    assert_eq!(record.len(), pal.len(), "one byte per palette entry");
    for (i, &a) in record.iter().enumerate() {
        let want = if i == usize::from(ti) { 0 } else { 255 };
        assert_eq!(a, want, "entry {i}");
    }
    assert_eq!(record.iter().filter(|&&a| a == 0).count(), 1);
    assert_eq!(v.palette_rgba(ti).map(|e| e[3]), Some(0));
    // The transparent pixel is the middle one.
    assert_eq!(v.planes[0].data[1], ti);
    assert_eq!(
        palette_from_frame(&v).as_ref(),
        Some(pal),
        "palette + record rebuild the Layer 1 palette exactly"
    );
    // The record matches what the helper derives from the palette.
    assert_eq!(palette_alpha_record(pal).as_deref(), Some(record));
}

#[test]
fn opaque_still_attaches_no_alpha_record() {
    let img = GifImage::from_rgb8(2, 1, vec![1, 2, 3, 9, 8, 7]).unwrap();
    let bytes = encode(&img, &EncodeOptions::default()).unwrap();
    let v = registry_decode(bytes);
    assert!(v.palette().is_some());
    assert_eq!(v.palette_alpha(), None, "opaque table: no record");
    assert_eq!(v.palette_rgba(0).map(|e| e[3]), Some(255));
    let pal = decode(&encode(&img, &EncodeOptions::default()).unwrap())
        .unwrap()
        .palette
        .unwrap();
    assert_eq!(palette_alpha_record(&pal), None);
}

#[test]
fn encoder_maps_alpha_record_to_the_transparency_index() {
    // Palette: 0 opaque red, 1 transparent, 2 partially covered
    // (below the threshold → transparent, folded onto entry 1),
    // 3 partially covered above the threshold → opaque.
    let rgb = vec![255, 0, 0, 1, 1, 1, 2, 2, 2, 0, 0, 255];
    let alpha = vec![255, 0, ALPHA_OPAQUE_THRESHOLD - 1, ALPHA_OPAQUE_THRESHOLD];
    let bytes = registry_encode_pal8(4, 1, vec![0, 1, 2, 3], rgb.clone(), Some(alpha));
    let file = parse(&bytes).unwrap();
    let frame = file.frames().next().unwrap();
    let gce = frame.graphic_control.as_ref().expect("GCE written");
    assert_eq!(
        gce.transparent_index,
        Some(1),
        "first below-threshold entry"
    );
    assert_eq!(
        frame.indices,
        vec![0, 1, 1, 3],
        "entry 2 folds onto the Transparency Index; entry 3 stays"
    );
    let composed = compose(&file).unwrap();
    let px = &composed[0].canvas.pixels;
    assert_eq!(px[3], 255, "entry 0 opaque");
    assert_eq!(px[7], 0, "entry 1 transparent");
    assert_eq!(px[11], 0, "entry 2 transparent");
    assert_eq!(px[15], 255, "entry 3 (alpha ≥ threshold) opaque");
    assert_eq!(&px[12..15], &[0, 0, 255], "entry 3 keeps its colour");

    // Decoding the packet through the registry hands the record back:
    // 0 at the Transparency Index, 255 elsewhere.
    let v = registry_decode(bytes);
    assert_eq!(v.palette_alpha(), Some(&[255u8, 0, 255, 255][..]));
}

#[test]
fn encoder_without_alpha_record_writes_an_opaque_image() {
    let rgb = vec![255, 0, 0, 1, 1, 1];
    let bytes = registry_encode_pal8(2, 1, vec![0, 1], rgb, None);
    let file = parse(&bytes).unwrap();
    assert!(!file.has_transparency(), "no record → no transparency");
    let v = registry_decode(bytes);
    assert_eq!(v.palette_alpha(), None);
}

#[test]
fn short_alpha_record_leaves_uncovered_entries_opaque() {
    // A record shorter than the palette (the framework allows it):
    // entry 0 transparent, entries 1 and 2 uncovered → opaque.
    let rgb = vec![9, 9, 9, 1, 1, 1, 2, 2, 2];
    let bytes = registry_encode_pal8(3, 1, vec![0, 1, 2], rgb, Some(vec![0]));
    let file = parse(&bytes).unwrap();
    let gce = file
        .frames()
        .next()
        .unwrap()
        .graphic_control
        .as_ref()
        .unwrap();
    assert_eq!(gce.transparent_index, Some(0));
    let composed = compose(&file).unwrap();
    let px = &composed[0].canvas.pixels;
    assert_eq!([px[3], px[7], px[11]], [0, 255, 255]);
}

#[test]
fn frame_bridge_round_trips_a_palette_with_alpha() {
    let pal = Palette::new(vec![[9, 8, 7, 255], [1, 2, 3, 0], [4, 5, 6, 200]]);
    let img = GifImage::from_indexed(3, 1, vec![0, 1, 2], pal.clone()).unwrap();
    let frame: VideoFrame = (&img).into();
    assert_eq!(frame.palette_alpha(), Some(&[255u8, 0, 200][..]));
    assert_eq!(frame.palette_rgba(2), Some([4, 5, 6, 200]));
    let params = still_params(3, 1, PixelFormat::Pal8);
    let back = GifImage::from_video_frame(&frame, &params).unwrap();
    assert_eq!(back, img, "alpha survives the bridge both ways");
    assert!(back.has_alpha());
    // An image built by value takes the same path.
    let frame2 = VideoFrame::from(img.clone());
    assert_eq!(frame2.palette_alpha(), frame.palette_alpha());
}
