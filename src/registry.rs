//! `oxideav-core` integration layer for `oxideav-gif`.
//!
//! Gated behind the default-on `registry` feature so image-library
//! consumers can depend on `oxideav-gif` with `default-features = false`
//! and skip the `oxideav-core` dependency entirely.
//!
//! The module exposes:
//! * [`register`] / [`register_codecs`] / [`register_containers`] — the
//!   `CodecRegistry` / `ContainerRegistry` entry points the umbrella
//!   `oxideav` crate calls during framework initialisation.
//! * [`make_decoder`] / [`make_encoder`] — the trait-side factories
//!   that wrap the framework-free decode / encode entry points.
//! * The `From<GifError> for oxideav_core::Error` conversion + the
//!   `Decoder` / `Encoder` trait impls.

use std::collections::VecDeque;

use oxideav_core::Decoder;
use oxideav_core::Encoder;
use oxideav_core::RuntimeContext;
use oxideav_core::{
    CodecCapabilities, CodecId, CodecInfo, CodecParameters, CodecRegistry, ContainerRegistry,
    Error as CoreError, Frame, MediaType, Packet, PixelFormat, TimeBase, VideoFrame, VideoPlane,
};

use crate::container::{decode_frame_payload, extradata_to_palette, palette_to_extradata};
use crate::encoder::DEFAULT_DELAY_CS;
use crate::error::GifError;
use crate::image::GifFrame;
use crate::lzw::Lzw;

/// Codec id for GIF image frames (mirrored from
/// [`crate::container::GIF_CODEC_ID`] for the umbrella `register*`
/// helpers).
pub use crate::container::GIF_CODEC_ID;

/// Convert a [`GifError`] into the framework-shared
/// `oxideav_core::Error` so trait impls in this crate can use `?` on
/// errors returned by the framework-free decode/encode functions.
impl From<GifError> for oxideav_core::Error {
    fn from(e: GifError) -> Self {
        match e {
            GifError::InvalidData(s) => CoreError::InvalidData(s),
            GifError::Unsupported(s) => CoreError::Unsupported(s),
            GifError::Eof => CoreError::Eof,
            GifError::NeedMore => CoreError::NeedMore,
            GifError::Other(s) => CoreError::other(s),
        }
    }
}

// ---- Decoder trait impl + factory ----

/// Factory for the `Decoder` trait impl — registered in the codec
/// registry and called by the framework when a `gif` packet stream
/// needs decoding.
pub fn make_decoder(params: &CodecParameters) -> oxideav_core::Result<Box<dyn Decoder>> {
    let width = params
        .width
        .ok_or_else(|| CoreError::invalid("GIF decoder: missing width"))?;
    let height = params
        .height
        .ok_or_else(|| CoreError::invalid("GIF decoder: missing height"))?;
    let global_palette = extradata_to_palette(&params.extradata);
    Ok(Box::new(GifDecoder {
        codec_id: params.codec_id.clone(),
        width,
        height,
        global_palette,
        canvas: vec![0u8; (width * height) as usize],
        prev_canvas: None,
        pending: Vec::new(),
        eof: false,
    }))
}

/// GIF `Decoder` trait impl: each `send_packet` carries one frame's
/// container payload (produced by the matching `Demuxer`). The
/// `receive_frame` returns the decoded `Pal8` `VideoFrame` sized to
/// the canvas.
struct GifDecoder {
    codec_id: CodecId,
    width: u32,
    height: u32,
    global_palette: Vec<[u8; 4]>,
    canvas: Vec<u8>,
    prev_canvas: Option<Vec<u8>>,
    pending: Vec<Packet>,
    eof: bool,
}

impl Decoder for GifDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> oxideav_core::Result<()> {
        self.pending.push(packet.clone());
        Ok(())
    }

    fn receive_frame(&mut self) -> oxideav_core::Result<Frame> {
        if self.pending.is_empty() {
            return if self.eof {
                Err(CoreError::Eof)
            } else {
                Err(CoreError::NeedMore)
            };
        }
        let pkt = self.pending.remove(0);
        let df = decode_frame_payload(&pkt.data)?;

        // Decode LZW into the frame's sub-rect indices.
        let lzw = Lzw::decoder(df.min_code_size)?;
        let decoded = lzw.read(df.lzw)?;
        let frame_area = (df.w as usize) * (df.h as usize);
        if decoded.len() < frame_area {
            return Err(CoreError::InvalidData(format!(
                "GIF: LZW output {} < expected {}",
                decoded.len(),
                frame_area
            )));
        }

        // Unweave interlacing.
        let indices = if df.interlaced {
            crate::decoder::deinterlace(&decoded, df.w as usize, df.h as usize)
        } else {
            decoded[..frame_area].to_vec()
        };

        // Save snapshot if this frame's disposal is "restore to previous".
        if df.disposal == 3 {
            self.prev_canvas = Some(self.canvas.clone());
        }

        // Composite indices into the canvas, skipping transparent pixels.
        let canvas_w = self.width as usize;
        let canvas_h = self.height as usize;
        let fw = df.w as usize;
        let fh = df.h as usize;
        let fx = df.x as usize;
        let fy = df.y as usize;
        let transp = df.transparent_index;
        let has_transp = df.has_transparent;
        for row in 0..fh {
            let dst_y = fy + row;
            if dst_y >= canvas_h {
                break;
            }
            for col in 0..fw {
                let dst_x = fx + col;
                if dst_x >= canvas_w {
                    break;
                }
                let px = indices[row * fw + col];
                if has_transp && px == transp {
                    continue;
                }
                self.canvas[dst_y * canvas_w + dst_x] = px;
            }
        }

        // Pick the active palette: local override wins over global.
        let palette = if !df.local_palette.is_empty() {
            let n = df.local_palette.len() / 4;
            let mut pal = Vec::with_capacity(n);
            for i in 0..n {
                pal.push([
                    df.local_palette[i * 4],
                    df.local_palette[i * 4 + 1],
                    df.local_palette[i * 4 + 2],
                    df.local_palette[i * 4 + 3],
                ]);
            }
            pal
        } else {
            self.global_palette.clone()
        };

        // Pack the palette into a plane of bytes (RGBA×N, padded to 256).
        let mut palette_plane = Vec::with_capacity(256 * 4);
        for i in 0..256 {
            if i < palette.len() {
                palette_plane.extend_from_slice(&palette[i]);
            } else {
                palette_plane.extend_from_slice(&[0, 0, 0, 0xFF]);
            }
        }

        let planes = vec![
            VideoPlane {
                stride: canvas_w,
                data: self.canvas.clone(),
            },
            VideoPlane {
                stride: 256 * 4,
                data: palette_plane,
            },
        ];

        let out = VideoFrame {
            pts: pkt.pts,
            planes,
        };

        // Apply this frame's disposal to prepare the canvas for the
        // *next* frame.
        match df.disposal {
            2 => {
                let clear_idx = if has_transp { transp } else { 0 };
                for row in 0..fh {
                    let dst_y = fy + row;
                    if dst_y >= canvas_h {
                        break;
                    }
                    for col in 0..fw {
                        let dst_x = fx + col;
                        if dst_x >= canvas_w {
                            break;
                        }
                        self.canvas[dst_y * canvas_w + dst_x] = clear_idx;
                    }
                }
            }
            3 => {
                if let Some(prev) = self.prev_canvas.take() {
                    self.canvas = prev;
                }
            }
            _ => {}
        }

        Ok(Frame::Video(out))
    }

    fn flush(&mut self) -> oxideav_core::Result<()> {
        self.eof = true;
        Ok(())
    }
}

// ---- Encoder trait impl + factory ----

/// Factory for the `Encoder` trait impl — registered in the codec
/// registry and called by the framework when a `gif` encode is
/// requested.
pub fn make_encoder(params: &CodecParameters) -> oxideav_core::Result<Box<dyn Encoder>> {
    Ok(Box::new(GifEncoder::new(params)?))
}

/// A frame that has been LZW-compressed but whose delay is still
/// unknown (we only know how long it's displayed once the NEXT frame
/// arrives with a later pts). We carry everything we need to serialise
/// it to an `OGIF` payload once the delay is resolved.
struct BufferedFrame {
    palette: Vec<[u8; 4]>,
    indices: Vec<u8>,
    pts_cs: i64,
    disposal: u8,
    transparent_index: Option<u8>,
}

/// Concrete GIF encoder.
///
/// Prefer [`make_encoder`] when working through `CodecRegistry`. Use
/// `GifEncoder` directly when you need per-frame disposal or transparency
/// control — the [`Encoder`] trait exposes no side-channel for those, so
/// they have to be set on the concrete type just before `send_frame`.
pub struct GifEncoder {
    output_params: CodecParameters,
    width: u32,
    height: u32,
    time_base: TimeBase,
    pending: VecDeque<Packet>,
    frame_count: u64,
    delay_cs: u16,
    global_palette_set: bool,
    /// Disposal to attach to the next `send_frame` call, then cleared.
    next_disposal: u8,
    /// Transparent index to attach to the next `send_frame` call, then
    /// cleared. `None` means the frame is fully opaque.
    next_transparent: Option<u8>,
    /// Most recently received frame — held until either the next
    /// frame or a `flush()` establishes its display duration.
    buffered: Option<BufferedFrame>,
}

impl GifEncoder {
    /// Build a fresh encoder from `CodecParameters`. Requires `width`,
    /// `height`, and (if set) `pixel_format == Pal8`.
    pub fn new(params: &CodecParameters) -> oxideav_core::Result<Self> {
        let width = params
            .width
            .ok_or_else(|| CoreError::invalid("GIF encoder: missing width"))?;
        let height = params
            .height
            .ok_or_else(|| CoreError::invalid("GIF encoder: missing height"))?;
        let pix = params.pixel_format.unwrap_or(PixelFormat::Pal8);
        if pix != PixelFormat::Pal8 {
            return Err(CoreError::unsupported(format!(
                "GIF encoder: pixel format {:?} not supported — feed Pal8",
                pix
            )));
        }
        let mut output_params = params.clone();
        output_params.media_type = MediaType::Video;
        output_params.codec_id = CodecId::new(GIF_CODEC_ID);
        output_params.pixel_format = Some(PixelFormat::Pal8);
        output_params.width = Some(width);
        output_params.height = Some(height);

        let time_base = TimeBase::new(1, 100);

        Ok(Self {
            output_params,
            width,
            height,
            time_base,
            pending: VecDeque::new(),
            frame_count: 0,
            delay_cs: DEFAULT_DELAY_CS,
            global_palette_set: false,
            next_disposal: 0,
            next_transparent: None,
            buffered: None,
        })
    }

    /// Set the GIF disposal method for the next frame emitted.
    ///
    /// Legal values:
    /// * `0` — unspecified (no special disposal; the implementations in
    ///   the wild treat this the same as 1).
    /// * `1` — keep the rendered pixels on the canvas.
    /// * `2` — restore the frame area to the background after display.
    /// * `3` — restore the canvas to its state before the frame drew.
    ///
    /// The hint is consumed by the next `send_frame` call and then reset
    /// to `0`.
    pub fn set_next_disposal(&mut self, disposal: u8) {
        self.next_disposal = disposal & 0x07;
    }

    /// Set the transparent-colour index for the next frame emitted, or
    /// `None` to emit a fully opaque frame. Consumed by the next
    /// `send_frame` call and then reset to `None`.
    pub fn set_next_transparent_index(&mut self, idx: Option<u8>) {
        self.next_transparent = idx;
    }
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
                let palette = extract_palette(v)?;
                let indices = pack_indices(v, self.width, self.height);

                let pts_cs = v
                    .pts
                    .unwrap_or(self.frame_count as i64 * self.delay_cs as i64);

                if let Some(prev) = self.buffered.take() {
                    let delta = (pts_cs - prev.pts_cs).max(1);
                    let delay = delta.min(u16::MAX as i64) as u16;
                    self.emit(prev, delay);
                }

                let disposal = self.next_disposal;
                let transparent_index = self.next_transparent.take();
                self.next_disposal = 0;
                self.buffered = Some(BufferedFrame {
                    palette,
                    indices,
                    pts_cs,
                    disposal,
                    transparent_index,
                });
                Ok(())
            }
            _ => Err(CoreError::invalid("GIF encoder: video frames only")),
        }
    }

    fn receive_packet(&mut self) -> oxideav_core::Result<Packet> {
        self.pending.pop_front().ok_or(CoreError::NeedMore)
    }

    fn flush(&mut self) -> oxideav_core::Result<()> {
        if let Some(prev) = self.buffered.take() {
            self.emit(prev, self.delay_cs);
        }
        Ok(())
    }
}

impl GifEncoder {
    fn emit(&mut self, bf: BufferedFrame, delay_cs: u16) {
        let frame = GifFrame {
            width: self.width,
            height: self.height,
            indices: bf.indices,
            palette: bf.palette.clone(),
            delay_cs,
        };
        let canvas = (self.width, self.height);
        let data =
            crate::encoder::frame_to_payload(&frame, canvas, bf.disposal, bf.transparent_index);
        if !self.global_palette_set {
            self.output_params.extradata = palette_to_extradata(&bf.palette);
            self.global_palette_set = true;
        }
        let mut pkt = Packet::new(0, self.time_base, data);
        pkt.pts = Some(bf.pts_cs);
        pkt.dts = pkt.pts;
        pkt.duration = Some(delay_cs as i64);
        pkt.flags.keyframe = true;
        self.pending.push_back(pkt);
        self.frame_count += 1;
    }
}

fn extract_palette(v: &VideoFrame) -> oxideav_core::Result<Vec<[u8; 4]>> {
    if v.planes.len() < 2 {
        return Err(CoreError::invalid(
            "GIF encoder: Pal8 frame missing palette plane",
        ));
    }
    let p = &v.planes[1];
    let n = p.data.len() / 4;
    let mut out = Vec::with_capacity(n.min(256));
    for i in 0..n.min(256) {
        out.push([
            p.data[i * 4],
            p.data[i * 4 + 1],
            p.data[i * 4 + 2],
            p.data[i * 4 + 3],
        ]);
    }
    Ok(out)
}

fn pack_indices(v: &VideoFrame, width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let plane = &v.planes[0];
    let stride = plane.stride;
    if stride == w {
        plane.data[..w * h].to_vec()
    } else {
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            out.extend_from_slice(&plane.data[row * stride..row * stride + w]);
        }
        out
    }
}

// ---- Container + registration ----

/// Register the GIF codec (decoder + encoder) into `reg`.
pub fn register_codecs(reg: &mut CodecRegistry) {
    let caps = CodecCapabilities::video("gif_sw")
        .with_lossless(true)
        .with_intra_only(true)
        .with_max_size(65535, 65535)
        .with_pixel_format(PixelFormat::Pal8);
    reg.register(
        CodecInfo::new(CodecId::new(GIF_CODEC_ID))
            .capabilities(caps)
            .decoder(make_decoder)
            .encoder(make_encoder),
    );
}

/// Register the GIF container's demuxer + muxer + probe + extension.
pub fn register_containers(reg: &mut ContainerRegistry) {
    crate::container::register(reg);
}

/// Unified registration entry point — installs the GIF codec into the
/// codec sub-registry and the GIF container into the container
/// sub-registry of the supplied [`RuntimeContext`].
pub fn register(ctx: &mut RuntimeContext) {
    register_codecs(&mut ctx.codecs);
    register_containers(&mut ctx.containers);
}

oxideav_core::register!("gif", register);

#[cfg(test)]
mod register_tests {
    use super::*;

    #[test]
    fn register_via_runtime_context_installs_both_sides() {
        let mut ctx = RuntimeContext::new();
        register(&mut ctx);
        let id = CodecId::new(GIF_CODEC_ID);
        assert!(
            ctx.codecs.has_decoder(&id),
            "GIF decoder factory not installed via RuntimeContext"
        );
        assert!(
            ctx.codecs.has_encoder(&id),
            "GIF encoder factory not installed via RuntimeContext"
        );
        assert_eq!(
            ctx.containers.container_for_extension("gif"),
            Some("gif"),
            "GIF container extension not installed via RuntimeContext"
        );
    }
}
