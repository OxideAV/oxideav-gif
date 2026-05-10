//! `oxideav-core` integration layer for `oxideav-gif`.
//!
//! Gated behind the default-on `registry` feature so image-library
//! consumers can depend on `oxideav-gif` with `default-features = false`
//! and skip the `oxideav-core` dependency entirely.

use std::collections::VecDeque;

use oxideav_core::{
    CodecCapabilities, CodecId, CodecInfo, CodecParameters, CodecRegistry, Decoder, Encoder, Frame,
    MediaType, Packet, PixelFormat, RuntimeContext, TimeBase, VideoFrame, VideoPlane,
};

use crate::decoder::{decode_gif, CODEC_ID_STR};
use crate::encoder::encode_gif;
use crate::error::GifError;
use crate::image::{GifBlock, GifFrame, GifImage, GifVersion};

/// Convert a [`GifError`] into the framework-shared `oxideav_core::Error`
/// so trait impls in this crate can use `?` on errors returned by the
/// framework-free decode/encode functions.
impl From<GifError> for oxideav_core::Error {
    fn from(e: GifError) -> Self {
        match e {
            GifError::InvalidData(s) => oxideav_core::Error::InvalidData(s),
            GifError::Unsupported(s) => oxideav_core::Error::Unsupported(s),
        }
    }
}

/// `Decoder` impl: each `send_packet` carries one full GIF Data Stream
/// and the matching `receive_frame` returns the *first* frame composited
/// onto the logical screen as RGBA. (Multi-frame GIFs surface their
/// remaining frames through subsequent `receive_frame` calls until the
/// stream is exhausted.)
pub struct GifDecoder {
    codec_id: CodecId,
    pending: Option<GifImage>,
    /// Index of the next frame to emit out of `pending.frames()`.
    next_frame: usize,
    /// Running framebuffer for compositing successive frames.
    canvas: Vec<u8>,
    eof: bool,
}

impl Decoder for GifDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> oxideav_core::Result<()> {
        if self.pending.is_some() {
            return Err(oxideav_core::Error::other(
                "GIF decoder: receive_frame must be called before sending another packet",
            ));
        }
        let img = decode_gif(&packet.data)?;
        self.canvas = vec![0u8; (img.width as usize) * (img.height as usize) * 4];
        self.next_frame = 0;
        self.pending = Some(img);
        Ok(())
    }

    fn receive_frame(&mut self) -> oxideav_core::Result<Frame> {
        let img = match &self.pending {
            Some(i) => i,
            None => {
                return if self.eof {
                    Err(oxideav_core::Error::Eof)
                } else {
                    Err(oxideav_core::Error::NeedMore)
                };
            }
        };
        let nframes = img.frames().count();
        if self.next_frame >= nframes {
            self.pending = None;
            return if self.eof {
                Err(oxideav_core::Error::Eof)
            } else {
                Err(oxideav_core::Error::NeedMore)
            };
        }
        let new_canvas = img.composite_frame_rgba(self.next_frame, &self.canvas)?;
        self.canvas = new_canvas;
        self.next_frame += 1;
        let vf = VideoFrame {
            pts: None,
            planes: vec![VideoPlane {
                stride: (img.width as usize) * 4,
                data: self.canvas.clone(),
            }],
        };
        Ok(Frame::Video(vf))
    }

    fn flush(&mut self) -> oxideav_core::Result<()> {
        self.eof = true;
        Ok(())
    }
}

fn make_decoder(params: &CodecParameters) -> oxideav_core::Result<Box<dyn Decoder>> {
    Ok(Box::new(GifDecoder {
        codec_id: params.codec_id.clone(),
        pending: None,
        next_frame: 0,
        canvas: Vec::new(),
        eof: false,
    }))
}

/// `Encoder` impl: buffers RGBA frames, then on `flush` quantises each
/// one to a 256-color palette and emits a single GIF (or animated GIF
/// if more than one frame was supplied).
///
/// Quantisation is intentionally trivial in this round — the encoder
/// down-samples each pixel into a fixed 6×7×6 = 252-entry RGB cube
/// so the output is deterministic and round-trippable. Better
/// quantisers (median cut, octree) are tracked as a follow-up.
pub struct GifEncoder {
    output_params: CodecParameters,
    width: u32,
    height: u32,
    time_base: TimeBase,
    frames: Vec<VideoFrame>,
    pending_out: VecDeque<Packet>,
    eof: bool,
}

impl Encoder for GifEncoder {
    fn codec_id(&self) -> &CodecId {
        &self.output_params.codec_id
    }

    fn output_params(&self) -> &CodecParameters {
        &self.output_params
    }

    fn send_frame(&mut self, frame: &Frame) -> oxideav_core::Result<()> {
        match frame {
            Frame::Video(v) => {
                self.frames.push(v.clone());
                Ok(())
            }
            _ => Err(oxideav_core::Error::invalid(
                "GIF encoder: video frames only",
            )),
        }
    }

    fn receive_packet(&mut self) -> oxideav_core::Result<Packet> {
        if let Some(p) = self.pending_out.pop_front() {
            return Ok(p);
        }
        if self.eof {
            if !self.frames.is_empty() {
                self.finalize()?;
                if let Some(p) = self.pending_out.pop_front() {
                    return Ok(p);
                }
            }
            return Err(oxideav_core::Error::Eof);
        }
        Err(oxideav_core::Error::NeedMore)
    }

    fn flush(&mut self) -> oxideav_core::Result<()> {
        self.eof = true;
        if !self.frames.is_empty() && self.pending_out.is_empty() {
            self.finalize()?;
        }
        Ok(())
    }
}

impl GifEncoder {
    fn finalize(&mut self) -> oxideav_core::Result<()> {
        let palette = build_default_palette();
        let mut blocks: Vec<GifBlock> = Vec::with_capacity(self.frames.len());
        for f in &self.frames {
            let plane = f
                .planes
                .first()
                .ok_or_else(|| oxideav_core::Error::invalid("GIF encoder: empty frame"))?;
            let indices = quantise_rgba(&plane.data, self.width as usize, self.height as usize)?;
            blocks.push(GifBlock::Frame(GifFrame {
                left: 0,
                top: 0,
                width: self.width as u16,
                height: self.height as u16,
                local_palette: None,
                local_palette_sorted: false,
                interlaced: false,
                indices,
                control: None,
            }));
        }
        let img = GifImage {
            version: if blocks.len() > 1 {
                GifVersion::Gif89a
            } else {
                GifVersion::Gif87a
            },
            width: self.width as u16,
            height: self.height as u16,
            color_resolution: 7,
            global_palette_sorted: false,
            background_color_index: 0,
            pixel_aspect_ratio: 0,
            global_palette: Some(palette),
            blocks,
        };
        let bytes = encode_gif(&img)?;
        let mut pkt = Packet::new(0, self.time_base, bytes);
        pkt.pts = self.frames[0].pts;
        pkt.dts = pkt.pts;
        pkt.flags.keyframe = true;
        self.pending_out.push_back(pkt);
        self.frames.clear();
        Ok(())
    }
}

fn make_encoder(params: &CodecParameters) -> oxideav_core::Result<Box<dyn Encoder>> {
    let width = params
        .width
        .ok_or_else(|| oxideav_core::Error::invalid("GIF encoder: missing width"))?;
    let height = params
        .height
        .ok_or_else(|| oxideav_core::Error::invalid("GIF encoder: missing height"))?;
    let mut output_params = params.clone();
    output_params.media_type = MediaType::Video;
    output_params.codec_id = CodecId::new(CODEC_ID_STR);
    output_params.width = Some(width);
    output_params.height = Some(height);
    output_params.pixel_format = Some(PixelFormat::Pal8);
    let time_base = TimeBase::new(1, 100);
    Ok(Box::new(GifEncoder {
        output_params,
        width,
        height,
        time_base,
        frames: Vec::new(),
        pending_out: VecDeque::new(),
        eof: false,
    }))
}

/// Build a deterministic 252-entry uniform RGB cube + 4 grayscale
/// entries. Total = 256 entries = `Size of Global Color Table = 7`
/// per §18.c.vi.
fn build_default_palette() -> Vec<u8> {
    let mut p = Vec::with_capacity(256 * 3);
    // 6 R x 7 G x 6 B cube.
    for r in 0..6u32 {
        for g in 0..7u32 {
            for b in 0..6u32 {
                p.push((r * 51) as u8);
                p.push((g * 42) as u8);
                p.push((b * 51) as u8);
            }
        }
    }
    // Pad with 4 grayscale entries to hit 256.
    for k in 0..4u32 {
        let v = (64 + k * 64) as u8;
        p.push(v);
        p.push(v);
        p.push(v);
    }
    debug_assert_eq!(p.len(), 256 * 3);
    p
}

/// Quantise an RGBA buffer to indices into the 252+4 palette built by
/// [`build_default_palette`]. Rejects buffers whose length doesn't
/// match `width * height * 4`.
fn quantise_rgba(rgba: &[u8], width: usize, height: usize) -> oxideav_core::Result<Vec<u8>> {
    if rgba.len() != width * height * 4 {
        return Err(oxideav_core::Error::invalid(format!(
            "GIF encoder: RGBA buffer is {} bytes, expected {} ({}x{}x4)",
            rgba.len(),
            width * height * 4,
            width,
            height
        )));
    }
    let mut out = Vec::with_capacity(width * height);
    for px in rgba.chunks_exact(4) {
        let r = px[0] as u32;
        let g = px[1] as u32;
        let b = px[2] as u32;
        // Map to the 6x7x6 cube.
        let ri = (r * 5 / 255) as u8; // 0..=5
        let gi = (g * 6 / 255) as u8; // 0..=6
        let bi = (b * 5 / 255) as u8; // 0..=5
        let idx = (ri as u16) * 42 + (gi as u16) * 6 + (bi as u16);
        out.push(idx as u8);
    }
    Ok(out)
}

/// Register the GIF codec into `reg`.
pub fn register_codecs(reg: &mut CodecRegistry) {
    let caps = CodecCapabilities::video("gif")
        .with_intra_only(true)
        .with_lossless(true)
        .with_max_size(65535, 65535)
        .with_pixel_formats(vec![PixelFormat::Rgba]);
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_STR))
            .capabilities(caps)
            .decoder(make_decoder)
            .encoder(make_encoder),
    );
}

/// Unified registration entry point.
pub fn register(ctx: &mut RuntimeContext) {
    register_codecs(&mut ctx.codecs);
}

oxideav_core::register!("gif", register);

#[cfg(test)]
mod register_tests {
    use super::*;

    #[test]
    fn register_via_runtime_context_installs_codec() {
        let mut ctx = RuntimeContext::new();
        register(&mut ctx);
        let id = CodecId::new(CODEC_ID_STR);
        assert!(
            ctx.codecs.has_decoder(&id),
            "GIF decoder factory not installed via RuntimeContext"
        );
        assert!(
            ctx.codecs.has_encoder(&id),
            "GIF encoder factory not installed via RuntimeContext"
        );
    }
}
