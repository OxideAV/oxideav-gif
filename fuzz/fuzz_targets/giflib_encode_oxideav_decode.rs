#![no_main]

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use oxideav_core::{CodecRegistry, ContainerRegistry, Frame, NullCodecResolver, ReadSeek};
use oxideav_gif::{register_codecs, register_containers};
use oxideav_gif_fuzz::giflib;

const MAX_WIDTH: usize = 64;
const MAX_PIXELS: usize = 2048;

fuzz_target!(|data: &[u8]| {
    // Skip silently if giflib isn't installed on this host.
    if !giflib::available() {
        return;
    }

    let Some((width, height, indices, palette_rgb)) = image_from_fuzz_input(data) else {
        return;
    };

    // Encode with giflib.
    let Some(gif_bytes) = giflib::encode_indexed(&indices, width, height, &palette_rgb) else {
        // giflib refused (e.g. degenerate palette) — skip silently.
        return;
    };

    // Demux + decode with oxideav-gif.
    let mut codecs = CodecRegistry::new();
    register_codecs(&mut codecs);
    let mut containers = ContainerRegistry::new();
    register_containers(&mut containers);

    let cursor = Cursor::new(gif_bytes);
    let boxed: Box<dyn ReadSeek> = Box::new(cursor);
    let mut demuxer = containers
        .open_demuxer("gif", boxed, &NullCodecResolver)
        .expect("open_demuxer");
    let si = demuxer.streams()[0].clone();
    assert_eq!(si.params.width, Some(width));
    assert_eq!(si.params.height, Some(height));

    let mut decoder = codecs.first_decoder(&si.params).expect("make_decoder");
    let pkt = demuxer.next_packet().expect("next_packet");
    decoder.send_packet(&pkt).expect("send_packet");
    let out_frame = match decoder.receive_frame().expect("receive_frame") {
        Frame::Video(v) => v,
        _ => panic!("non-video frame"),
    };

    // Compare via the input palette: for each pixel, the
    // oxideav-decoded pixel's palette-resolved RGB must equal the
    // input palette[indices[i]] RGB. We deliberately compare RGB only
    // (no alpha) because GIF palettes have no alpha channel.
    let n = (width as usize) * (height as usize);
    assert_eq!(out_frame.planes[0].data.len(), n, "index plane length");
    let pal_out = &out_frame.planes[1].data;
    for i in 0..n {
        let in_rgb = palette_rgb[indices[i] as usize];
        let out_idx = out_frame.planes[0].data[i] as usize;
        let out_rgb = [
            pal_out[out_idx * 4],
            pal_out[out_idx * 4 + 1],
            pal_out[out_idx * 4 + 2],
        ];
        assert_eq!(out_rgb, in_rgb, "pixel {i} RGB mismatch");
    }
});

/// Layout (matches the other two harnesses, but the palette is RGB
/// triples to feed straight into giflib without a [_; 4] -> [_; 3] dance):
///   shape:u8 — width seed
///   pal_n:u8 — palette size hint, mapped to 1..=256
///   palette[pal_n*3] — packed RGB triples
///   tail — index plane bytes, masked to `pal_n`
fn image_from_fuzz_input(data: &[u8]) -> Option<(u32, u32, Vec<u8>, Vec<[u8; 3]>)> {
    if data.len() < 2 {
        return None;
    }
    let shape = data[0];
    let pal_n = (data[1] as usize) + 1;
    let pal_bytes = pal_n.checked_mul(3)?;
    if data.len() < 2 + pal_bytes + 1 {
        return None;
    }
    let mut palette: Vec<[u8; 3]> = Vec::with_capacity(pal_n);
    for i in 0..pal_n {
        let off = 2 + i * 3;
        palette.push([data[off], data[off + 1], data[off + 2]]);
    }
    let tail = &data[2 + pal_bytes..];
    let pixel_count = tail.len().min(MAX_PIXELS);
    if pixel_count == 0 {
        return None;
    }
    let width = ((shape as usize) % MAX_WIDTH) + 1;
    let width = width.min(pixel_count);
    let height = pixel_count / width;
    if height == 0 {
        return None;
    }
    let used_len = width * height;
    let indices: Vec<u8> = tail[..used_len]
        .iter()
        .map(|b| (*b as usize % pal_n) as u8)
        .collect();
    Some((width as u32, height as u32, indices, palette))
}
