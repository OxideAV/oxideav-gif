#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_core::{
    CodecId, CodecParameters, CodecRegistry, ContainerRegistry, Frame, MediaType, PixelFormat,
    StreamInfo, TimeBase, VideoFrame, VideoPlane, WriteSeek,
};
use oxideav_gif::{register_codecs, register_containers, GIF_CODEC_ID};
use oxideav_gif_fuzz::giflib;

const MAX_WIDTH: usize = 64;
const MAX_PIXELS: usize = 2048;

fuzz_target!(|data: &[u8]| {
    // Skip silently if giflib isn't installed on this host.
    if !giflib::available() {
        return;
    }

    let Some((width, height, indices, palette)) = image_from_fuzz_input(data) else {
        return;
    };

    // Encode + mux through oxideav-gif.
    let mut codecs = CodecRegistry::new();
    register_codecs(&mut codecs);
    let mut containers = ContainerRegistry::new();
    register_containers(&mut containers);

    let frame_in = build_pal8_frame(width, height, &indices, &palette);
    let params_enc = build_params(width, height);
    let mut encoder = codecs.first_encoder(&params_enc).expect("make_encoder");
    encoder
        .send_frame(&Frame::Video(frame_in))
        .expect("send_frame");
    encoder.flush().expect("flush");
    let pkt = encoder.receive_packet().expect("receive_packet");
    let encoder_params = encoder.output_params().clone();

    let (sink, sink_data) = SharedSink::new();
    {
        let boxed: Box<dyn WriteSeek> = Box::new(sink);
        let si = StreamInfo {
            index: 0,
            time_base: TimeBase::new(1, 100),
            duration: None,
            start_time: Some(0),
            params: encoder_params,
        };
        let mut muxer = containers
            .open_muxer("gif", boxed, std::slice::from_ref(&si))
            .expect("open_muxer");
        muxer.write_header().expect("write_header");
        muxer.write_packet(&pkt).expect("write_packet");
        muxer.write_trailer().expect("write_trailer");
    }
    let gif_bytes: Vec<u8> = sink_data.lock().unwrap().clone();

    // Decode with giflib.
    let decoded = giflib::decode_to_rgba(&gif_bytes).expect("giflib decode");
    assert_eq!(decoded.width, width);
    assert_eq!(decoded.height, height);

    // Compare via the input palette: for each pixel, the giflib-decoded
    // RGB must equal the RGB of `palette[indices[i]]`. We compare RGB
    // only because GIF palettes have no alpha channel.
    let n = (width as usize) * (height as usize);
    assert_eq!(decoded.rgba.len(), n * 4);
    for i in 0..n {
        let in_rgb = &palette[indices[i] as usize][..3];
        let off = i * 4;
        let out_rgb = &decoded.rgba[off..off + 3];
        assert_eq!(out_rgb, in_rgb, "pixel {i} RGB mismatch");
        assert_eq!(decoded.rgba[off + 3], 0xFF, "alpha must be opaque");
    }
});

/// Same shape as the self-roundtrip harness — two-byte header, packed
/// RGB palette, then a tail of palette indices masked to size.
fn image_from_fuzz_input(data: &[u8]) -> Option<(u32, u32, Vec<u8>, Vec<[u8; 4]>)> {
    if data.len() < 2 {
        return None;
    }
    let shape = data[0];
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
