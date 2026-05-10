#![no_main]
//! Encode an arbitrary GIF (built from fuzzed dimensions + palette
//! + indices) and confirm it round-trips through the decoder cleanly.

use libfuzzer_sys::fuzz_target;
use oxideav_gif::{decode_gif, encode_gif, GifBlock, GifFrame, GifImage, GifVersion};

fuzz_target!(|data: &[u8]| {
    if data.len() < 6 {
        return;
    }
    // First two bytes pick width and height in 1..=16.
    let w = ((data[0] % 16) as u16) + 1;
    let h = ((data[1] % 16) as u16) + 1;
    let pixels = (w as usize) * (h as usize);
    let palette_entries = ((data[2] % 8) + 1) as usize; // 1..=8 entries
    let palette_bytes = palette_entries * 3;
    if data.len() < 3 + palette_bytes + pixels {
        return;
    }
    let palette = data[3..3 + palette_bytes].to_vec();
    let raw_indices = &data[3 + palette_bytes..3 + palette_bytes + pixels];
    let indices: Vec<u8> = raw_indices
        .iter()
        .map(|&b| ((b as usize) % palette_entries) as u8)
        .collect();
    let img = GifImage {
        version: GifVersion::Gif87a,
        width: w,
        height: h,
        color_resolution: 7,
        global_palette_sorted: false,
        background_color_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(palette),
        blocks: vec![GifBlock::Frame(GifFrame {
            left: 0,
            top: 0,
            width: w,
            height: h,
            local_palette: None,
            local_palette_sorted: false,
            interlaced: false,
            indices: indices.clone(),
            control: None,
        })],
    };
    let bytes = match encode_gif(&img) {
        Ok(b) => b,
        Err(_) => return, // encoder may legitimately reject some inputs
    };
    let decoded = decode_gif(&bytes).expect("self-encoded GIF must decode");
    let frame = decoded.frames().next().expect("must have at least one frame");
    assert_eq!(frame.indices, indices, "round-trip indices mismatch");
});
