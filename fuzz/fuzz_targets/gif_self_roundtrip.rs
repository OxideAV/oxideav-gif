#![no_main]

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use oxideav_core::{
    CodecId, CodecParameters, CodecRegistry, ContainerRegistry, Frame, MediaType,
    NullCodecResolver, PixelFormat, ReadSeek, StreamInfo, TimeBase, VideoFrame, VideoPlane,
    WriteSeek,
};
use oxideav_gif::{register_codecs, register_containers, GIF_CODEC_ID};

const MAX_WIDTH: usize = 64;
const MAX_PIXELS: usize = 2048;

fuzz_target!(|data: &[u8]| {
    let Some((width, height, indices, palette)) = image_from_fuzz_input(data) else {
        return;
    };

    let mut codecs = CodecRegistry::new();
    register_codecs(&mut codecs);
    let mut containers = ContainerRegistry::new();
    register_containers(&mut containers);

    // Encode through the trait API.
    let frame_in = build_pal8_frame(width, height, &indices, &palette);
    let params_enc = build_params(width, height);
    let mut encoder = codecs.first_encoder(&params_enc).expect("make_encoder");
    encoder
        .send_frame(&Frame::Video(frame_in))
        .expect("send_frame");
    encoder.flush().expect("flush");
    let pkt = encoder.receive_packet().expect("receive_packet");
    let encoder_params = encoder.output_params().clone();

    // Mux into a real GIF89a file.
    let (sink, sink_data) = SharedSink::new();
    {
        let boxed: Box<dyn WriteSeek> = Box::new(sink);
        let si = StreamInfo {
            index: 0,
            time_base: TimeBase::new(1, 100),
            duration: None,
            start_time: Some(0),
            params: encoder_params.clone(),
        };
        let mut muxer = containers
            .open_muxer("gif", boxed, std::slice::from_ref(&si))
            .expect("open_muxer");
        muxer.write_header().expect("write_header");
        muxer.write_packet(&pkt).expect("write_packet");
        muxer.write_trailer().expect("write_trailer");
    }
    let gif_bytes: Vec<u8> = sink_data.lock().unwrap().clone();

    // Demux + decode.
    let cursor = Cursor::new(gif_bytes);
    let boxed: Box<dyn ReadSeek> = Box::new(cursor);
    let mut demuxer = containers
        .open_demuxer("gif", boxed, &NullCodecResolver)
        .expect("open_demuxer");
    let si = demuxer.streams()[0].clone();
    assert_eq!(si.params.width, Some(width));
    assert_eq!(si.params.height, Some(height));

    let mut decoder = codecs.first_decoder(&si.params).expect("make_decoder");
    let out_pkt = demuxer.next_packet().expect("next_packet");
    decoder.send_packet(&out_pkt).expect("send_packet");
    let out_frame = match decoder.receive_frame().expect("receive_frame") {
        Frame::Video(v) => v,
        _ => panic!("non-video frame"),
    };

    // Compare indices through the palette: GIF muxer drops palette
    // alpha and pads the GCT to a power of two, which can renumber
    // entries when our palette length isn't a power of two. The robust
    // check is "the RGB the input pixel resolves to == the RGB the
    // decoded pixel resolves to".
    let n_in = (width as usize) * (height as usize);
    assert_eq!(out_frame.planes[0].data.len(), n_in, "index plane length");
    let pal_in_rgb: Vec<[u8; 3]> = palette.iter().map(|p| [p[0], p[1], p[2]]).collect();
    let pal_out = &out_frame.planes[1].data;
    for i in 0..n_in {
        let in_idx = indices[i] as usize;
        let out_idx = out_frame.planes[0].data[i] as usize;
        let in_rgb = pal_in_rgb[in_idx];
        let out_rgb = [
            pal_out[out_idx * 4],
            pal_out[out_idx * 4 + 1],
            pal_out[out_idx * 4 + 2],
        ];
        assert_eq!(in_rgb, out_rgb, "pixel {i} RGB mismatch");
    }
});

/// Carve fuzz bytes into (width, height, indices, palette).
///
/// Layout:
///   shape:u8 — width seed
///   pal_n:u8 — palette size hint (mapped to 1..=256 unique colours)
///   palette[pal_n*3] — packed RGB triples
///   tail — index plane bytes, masked to `pal_n - 1`
fn image_from_fuzz_input(data: &[u8]) -> Option<(u32, u32, Vec<u8>, Vec<[u8; 4]>)> {
    if data.len() < 2 {
        return None;
    }
    let shape = data[0];
    // pal_n in 1..=256: 0 → 1, 255 → 256.
    let pal_n = (data[1] as usize) + 1;
    let pal_bytes = pal_n.checked_mul(3)?;
    if data.len() < 2 + pal_bytes + 1 {
        return None;
    }
    let mut palette: Vec<[u8; 4]> = Vec::with_capacity(pal_n);
    for i in 0..pal_n {
        let off = 2 + i * 3;
        palette.push([data[off], data[off + 1], data[off + 2], 0xFF]);
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
    // Mask each input byte into a legal palette slot. `pal_n` is
    // `1..=256` so a plain modulo is the safest fit (works for non
    // power-of-two palette sizes too).
    let indices: Vec<u8> = tail[..used_len]
        .iter()
        .map(|b| (*b as usize % pal_n) as u8)
        .collect();
    Some((width as u32, height as u32, indices, palette))
}

fn build_pal8_frame(width: u32, height: u32, indices: &[u8], palette: &[[u8; 4]]) -> VideoFrame {
    let mut palette_plane = Vec::with_capacity(256 * 4);
    for i in 0..256 {
        if i < palette.len() {
            palette_plane.extend_from_slice(&palette[i]);
        } else {
            palette_plane.extend_from_slice(&[0, 0, 0, 0xFF]);
        }
    }
    VideoFrame {
        pts: Some(0),
        planes: vec![
            VideoPlane {
                stride: width as usize,
                data: indices.to_vec(),
            },
            VideoPlane {
                stride: 256 * 4,
                data: palette_plane,
            },
        ],
    }
}

fn build_params(width: u32, height: u32) -> CodecParameters {
    let mut p = CodecParameters::video(CodecId::new(GIF_CODEC_ID));
    p.media_type = MediaType::Video;
    p.width = Some(width);
    p.height = Some(height);
    p.pixel_format = Some(PixelFormat::Pal8);
    p
}

// ---- Local SharedSink (mirrors tests/common/mod.rs) -------------------
//
// Cargo-fuzz binaries can't `mod common;` into `tests/common/`, so we
// inline the same sink here. Tracks bytes in an `Arc<Mutex<Vec<u8>>>` so
// we can pull them out after the muxer drops its handle.

use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

struct SharedSink {
    inner: Arc<Mutex<Vec<u8>>>,
    pos: u64,
}

impl SharedSink {
    fn new() -> (Self, Arc<Mutex<Vec<u8>>>) {
        let inner = Arc::new(Mutex::new(Vec::<u8>::new()));
        (
            Self {
                inner: Arc::clone(&inner),
                pos: 0,
            },
            inner,
        )
    }
}

impl Write for SharedSink {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let mut guard = self.inner.lock().unwrap();
        let start = self.pos as usize;
        if start + data.len() > guard.len() {
            guard.resize(start + data.len(), 0);
        }
        guard[start..start + data.len()].copy_from_slice(data);
        self.pos += data.len() as u64;
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Read for SharedSink {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("SharedSink: read unsupported"))
    }
}

impl Seek for SharedSink {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let guard = self.inner.lock().unwrap();
        let len = guard.len() as u64;
        let new_pos = match pos {
            SeekFrom::Start(n) => n,
            SeekFrom::End(n) => (len as i64 + n).max(0) as u64,
            SeekFrom::Current(n) => (self.pos as i64 + n).max(0) as u64,
        };
        self.pos = new_pos;
        Ok(self.pos)
    }
}
