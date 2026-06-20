//! `oxideav-core` integration layer for `oxideav-gif`.
//!
//! Gated behind the default-on `registry` Cargo feature so image-library
//! consumers can depend on `oxideav-gif` with `default-features = false`
//! and skip the `oxideav-core` dependency entirely.
//!
//! The module exposes:
//! * [`register`] / [`register_codecs`] / [`register_containers`] — the
//!   `CodecRegistry` / `ContainerRegistry` entry points the umbrella
//!   `oxideav` crate calls during framework initialisation.
//! * [`GifDecoder`] / [`GifEncoder`] — `Decoder` / `Encoder` trait
//!   wrappers around the framework-free [`crate::decode`] +
//!   [`crate::encode`] entry points.
//! * The `From<Error> for oxideav_core::Error` conversion that lets
//!   trait impls bubble bitstream errors up through the framework error
//!   type.
//!
//! The `oxideav_core::register!` invocation at the bottom of the file
//! generates the `pub fn __oxideav_entry(ctx)` wrapper that
//! `oxideav-meta`'s `register_all` calls. `lib.rs` re-exports it at the
//! crate root (`oxideav_gif::__oxideav_entry`) which is the symbol meta
//! looks up.

use oxideav_core::{
    frame::VideoPlane, CodecCapabilities, CodecId, CodecInfo, CodecParameters, CodecRegistry,
    ContainerRegistry, Decoder, Encoder, Error as CoreError, Frame as CoreFrame, Packet,
    PixelFormat, Result as CoreResult, RuntimeContext, TimeBase, VideoFrame,
};

use crate::compose::{compose, RgbaCanvas};
use crate::error::Error as GifError;
use crate::image::{Block, Frame as GifFrame, GifImage, Rgb, Version};

/// Canonical codec id for GIF image frames.
pub const CODEC_ID_STR: &str = "gif";

impl From<GifError> for CoreError {
    fn from(e: GifError) -> Self {
        match e {
            GifError::InvalidData(s) => CoreError::InvalidData(s),
            GifError::Unsupported(s) => CoreError::Unsupported(s),
            GifError::UnexpectedEof => {
                CoreError::InvalidData("gif: unexpected end of stream".into())
            }
            GifError::InvalidInput(s) => CoreError::InvalidData(s),
        }
    }
}

/// Register the GIF codec into the supplied [`CodecRegistry`].
pub fn register_codecs(reg: &mut CodecRegistry) {
    let caps = CodecCapabilities::video("gif_sw")
        .with_lossy(false)
        .with_intra_only(true)
        .with_max_size(65535, 65535)
        .with_pixel_formats(vec![PixelFormat::Rgba]);
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_STR))
            .capabilities(caps)
            .decoder(make_decoder)
            .encoder(make_encoder),
    );
}

/// Register the `.gif` file extension so the container registry can
/// resolve the codec identifier from a filename hint.
///
/// GIF is its own container (the data stream IS the file format), so
/// only the extension hook is wired up — no demuxer or muxer is
/// installed.
pub fn register_containers(reg: &mut ContainerRegistry) {
    reg.register_extension("gif", "gif");
}

/// Unified registration entry point — installs the GIF codec into the
/// codec sub-registry and the `.gif` extension hint into the container
/// sub-registry of the supplied [`RuntimeContext`].
///
/// Wired into [`oxideav_meta::register_all`] via the
/// [`oxideav_core::register!`] macro below.
pub fn register(ctx: &mut RuntimeContext) {
    register_codecs(&mut ctx.codecs);
    register_containers(&mut ctx.containers);
}

oxideav_core::register!("gif", register);

fn make_decoder(_params: &CodecParameters) -> CoreResult<Box<dyn Decoder>> {
    Ok(Box::new(GifDecoder::new()))
}

fn make_encoder(params: &CodecParameters) -> CoreResult<Box<dyn Encoder>> {
    Ok(Box::new(GifEncoder::new_from_params(params)))
}

/// `Decoder` trait wrapper around the framework-free
/// [`crate::decode`] + [`crate::compose::compose`] pipeline.
///
/// One input [`Packet`] is treated as a complete `<GIF Data Stream>`
/// (Header → Logical Screen Descriptor → blocks → Trailer). Compose
/// runs the §23 disposal-method state machine and emits one
/// [`oxideav_core::VideoFrame`] per source frame, all in `Rgba`.
pub struct GifDecoder {
    codec_id: CodecId,
    queued: std::collections::VecDeque<CoreFrame>,
}

impl GifDecoder {
    pub fn new() -> Self {
        Self {
            codec_id: CodecId::new(CODEC_ID_STR),
            queued: std::collections::VecDeque::new(),
        }
    }
}

impl Default for GifDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for GifDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> CoreResult<()> {
        let image = crate::decode(&packet.data)?;
        let composed = compose(&image)?;
        for (idx, frame) in composed.into_iter().enumerate() {
            self.queued
                .push_back(canvas_to_videoframe(&frame.canvas, idx as i64));
        }
        Ok(())
    }

    fn receive_frame(&mut self) -> CoreResult<CoreFrame> {
        self.queued
            .pop_front()
            .ok_or_else(|| CoreError::invalid("gif: no frame queued"))
    }

    fn flush(&mut self) -> CoreResult<()> {
        self.queued.clear();
        Ok(())
    }
}

fn canvas_to_videoframe(canvas: &RgbaCanvas, pts: i64) -> CoreFrame {
    let stride = (canvas.width as usize) * 4;
    CoreFrame::Video(VideoFrame {
        pts: Some(pts),
        planes: vec![VideoPlane {
            stride,
            data: canvas.pixels.clone(),
        }],
    })
}

/// `Encoder` trait wrapper that accepts one `Rgba` [`VideoFrame`] per
/// `send_frame` and emits a single-frame GIF byte stream per
/// `receive_packet`.
///
/// The encoder builds a per-frame §19 colour table from the input
/// pixels. A fully-opaque frame using ≤256 distinct RGB triplets keeps
/// its exact colours (lossless); a frame with more than 256 colours is
/// reduced to a 256-entry palette with the median-cut quantiser
/// ([`crate::quantize`]). GIF has no per-pixel alpha — only one
/// §23.c.viii Transparency Index — so transparent pixels (alpha below
/// [`crate::quantize::ALPHA_OPAQUE_THRESHOLD`]) route to a single
/// reserved palette slot and the frame gains a §23 Graphic Control
/// Extension marking that slot transparent.
pub struct GifEncoder {
    output_params: CodecParameters,
    pending: std::collections::VecDeque<Packet>,
}

impl GifEncoder {
    pub fn new_from_params(params: &CodecParameters) -> Self {
        Self {
            output_params: params.clone(),
            pending: std::collections::VecDeque::new(),
        }
    }
}

impl Encoder for GifEncoder {
    fn codec_id(&self) -> &CodecId {
        &self.output_params.codec_id
    }

    fn output_params(&self) -> &CodecParameters {
        &self.output_params
    }

    fn send_frame(&mut self, frame: &CoreFrame) -> CoreResult<()> {
        let video = match frame {
            CoreFrame::Video(v) => v,
            _ => return Err(CoreError::invalid("gif encoder: expected Frame::Video")),
        };
        let width = self
            .output_params
            .width
            .ok_or_else(|| CoreError::invalid("gif encoder: missing width in params"))?;
        let height = self
            .output_params
            .height
            .ok_or_else(|| CoreError::invalid("gif encoder: missing height in params"))?;
        if width == 0 || height == 0 || width > 65535 || height > 65535 {
            return Err(CoreError::invalid(
                "gif encoder: width/height must be in 1..=65535",
            ));
        }
        let plane = video
            .planes
            .first()
            .ok_or_else(|| CoreError::invalid("gif encoder: video frame has no planes"))?;

        let (palette, indices, transparent_index) =
            build_palette_and_indices(&plane.data, plane.stride, width as usize, height as usize)?;

        // Attach a §23 Graphic Control Extension only when the frame
        // carried transparency, so a fully-opaque single-colour-clean
        // input stays a minimal GIF89a image with no GCE.
        let graphic_control = transparent_index.map(|ti| crate::image::GraphicControl {
            disposal: crate::image::DisposalMethod::None,
            user_input: false,
            transparent_index: Some(ti),
            delay_centis: 0,
        });

        let image = GifImage {
            version: Version::Gif89a,
            screen_width: width as u16,
            screen_height: height as u16,
            color_resolution: 7,
            global_palette_sorted: false,
            background_index: 0,
            pixel_aspect_ratio: 0,
            global_palette: Some(palette),
            blocks: vec![Block::Image(GifFrame {
                left: 0,
                top: 0,
                width: width as u16,
                height: height as u16,
                local_palette: None,
                palette_sorted: false,
                interlaced: false,
                indices,
                graphic_control,
            })],
        };

        let bytes = crate::encode(&image)?;
        let pkt = Packet::new(0u32, TimeBase::new(1, 1), bytes);
        self.pending.push_back(pkt);
        Ok(())
    }

    fn receive_packet(&mut self) -> CoreResult<Packet> {
        self.pending
            .pop_front()
            .ok_or_else(|| CoreError::invalid("gif encoder: no packet queued"))
    }

    fn flush(&mut self) -> CoreResult<()> {
        Ok(())
    }
}

/// Quantise an RGBA frame into a GIF palette + index plane, plus the
/// §23.c.viii Transparency Index when the frame carried any transparent
/// pixel.
///
/// Two paths, picked automatically:
///
/// * **Lossless** — when the frame is fully opaque and uses ≤256
///   distinct RGB triplets, the exact set of colours becomes the
///   palette and every pixel keeps its precise colour. This keeps a
///   palette-clean input byte-stable through the encoder, with no
///   quantisation error.
/// * **Median cut** — when the frame uses more than 256 distinct
///   colours, or carries transparent pixels (GIF has no per-pixel
///   alpha; only one §23.c.viii Transparency Index), the frame is
///   reduced with [`crate::quantize::quantize_rgba`] to a conformant
///   ≤256-entry §19 palette. Transparent pixels route to one reserved
///   slot returned as the third tuple element.
fn build_palette_and_indices(
    rgba: &[u8],
    stride: usize,
    width: usize,
    height: usize,
) -> CoreResult<(Vec<Rgb>, Vec<u8>, Option<u8>)> {
    let row_bytes = width
        .checked_mul(4)
        .ok_or_else(|| CoreError::invalid("gif encoder: row size overflow"))?;
    if stride < row_bytes {
        return Err(CoreError::invalid(
            "gif encoder: plane stride smaller than width*4",
        ));
    }
    let needed_total = stride
        .checked_mul(height)
        .ok_or_else(|| CoreError::invalid("gif encoder: plane size overflow"))?;
    if rgba.len() < needed_total {
        return Err(CoreError::invalid(
            "gif encoder: plane data shorter than width*height*4",
        ));
    }

    // Repack to a tight (no-stride) RGBA buffer and decide whether the
    // lossless exact-palette path is reachable: fully opaque + ≤256
    // distinct colours.
    let pixel_count = width * height;
    let mut tight = Vec::with_capacity(pixel_count * 4);
    let mut indices = Vec::with_capacity(pixel_count);
    let mut palette: Vec<Rgb> = Vec::new();
    let mut exact_ok = true;
    let mut any_transparent = false;
    for y in 0..height {
        let row = &rgba[y * stride..y * stride + row_bytes];
        for x in 0..width {
            let p = &row[x * 4..x * 4 + 4];
            tight.extend_from_slice(p);
            if p[3] < crate::quantize::ALPHA_OPAQUE_THRESHOLD {
                any_transparent = true;
                exact_ok = false;
            }
            if exact_ok {
                let rgb = Rgb::new(p[0], p[1], p[2]);
                match palette.iter().position(|c| *c == rgb) {
                    Some(i) => indices.push(i as u8),
                    None if palette.len() < 256 => {
                        palette.push(rgb);
                        indices.push((palette.len() - 1) as u8);
                    }
                    None => exact_ok = false,
                }
            }
        }
    }

    if exact_ok && !any_transparent {
        // Lossless: exact palette + indices, no transparency.
        return Ok((palette, indices, None));
    }

    // Lossy fallback: median-cut quantiser over the tight RGBA buffer.
    let q = crate::quantize::quantize_rgba(&tight, width, height, crate::quantize::MAX_PALETTE)
        .map_err(|e| CoreError::invalid(format!("gif encoder: {e}")))?;
    Ok((q.palette, q.indices, q.transparent_index))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_rgba(w: usize, h: usize, r: u8, g: u8, b: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity(w * h * 4);
        for _ in 0..(w * h) {
            v.extend_from_slice(&[r, g, b, 255]);
        }
        v
    }

    #[test]
    fn register_via_runtime_context_installs_codec_factory() {
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
        assert_eq!(
            ctx.containers.container_for_extension("gif"),
            Some("gif"),
            "GIF container extension not installed via RuntimeContext"
        );
    }

    #[test]
    fn entry_point_dispatches_into_register() {
        // The register! macro should expose __oxideav_entry on the
        // module path; meta's register_all calls the crate-root
        // re-export. Confirm both paths work.
        let mut ctx = RuntimeContext::new();
        super::__oxideav_entry(&mut ctx);
        let id = CodecId::new(CODEC_ID_STR);
        assert!(ctx.codecs.has_decoder(&id));
    }

    #[test]
    fn encode_then_decode_roundtrip_solid_frame() {
        let w = 8usize;
        let h = 4usize;
        let rgba = solid_rgba(w, h, 0x10, 0x80, 0xC0);
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(w as u32);
        params.height = Some(h as u32);
        params.pixel_format = Some(PixelFormat::Rgba);

        let mut enc = make_encoder(&params).unwrap();
        let frame_in = CoreFrame::Video(VideoFrame {
            pts: Some(0),
            planes: vec![VideoPlane {
                stride: w * 4,
                data: rgba.clone(),
            }],
        });
        enc.send_frame(&frame_in).unwrap();
        let pkt = enc.receive_packet().unwrap();
        assert!(pkt.data.starts_with(b"GIF"));

        let mut dec = make_decoder(&params).unwrap();
        dec.send_packet(&pkt).unwrap();
        let frame_out = dec.receive_frame().unwrap();
        let CoreFrame::Video(v) = frame_out else {
            panic!("decoder returned non-video frame");
        };
        assert_eq!(v.planes.len(), 1);
        assert_eq!(v.planes[0].stride, w * 4);
        assert_eq!(v.planes[0].data.len(), w * h * 4);
        // Solid colour input → every pixel decodes back to the same
        // RGB triplet (alpha forced to 255 since the input was opaque
        // and we did not set a transparent index).
        for px in v.planes[0].data.chunks_exact(4) {
            assert_eq!(px, [0x10, 0x80, 0xC0, 0xFF]);
        }
    }

    #[test]
    fn encoder_quantises_more_than_256_colours() {
        // 17×16 = 272 pixels, every one a unique RGB triplet → > 256
        // colours. The encoder used to reject this; it now reduces it
        // to a conformant ≤256-entry §19 palette via median cut and
        // produces a decodable stream.
        let w = 17usize;
        let h = 16usize;
        let mut rgba = Vec::with_capacity(w * h * 4);
        for i in 0..(w * h) {
            rgba.extend_from_slice(&[
                (i & 0xFF) as u8,
                ((i >> 4) & 0xFF) as u8,
                ((i >> 8) & 0xFF) as u8,
                255,
            ]);
        }
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(w as u32);
        params.height = Some(h as u32);
        params.pixel_format = Some(PixelFormat::Rgba);
        let mut enc = make_encoder(&params).unwrap();
        let frame = CoreFrame::Video(VideoFrame {
            pts: Some(0),
            planes: vec![VideoPlane {
                stride: w * 4,
                data: rgba,
            }],
        });
        enc.send_frame(&frame).unwrap();
        let pkt = enc.receive_packet().unwrap();
        assert!(pkt.data.starts_with(b"GIF"));
        // The produced stream decodes, and its §19 palette is within
        // the 256-entry limit.
        let decoded = crate::decode(&pkt.data).unwrap();
        let pal = decoded.global_palette.as_ref().unwrap();
        assert!(pal.len() <= 256);
        assert_eq!(decoded.screen_width as usize, w);
        assert_eq!(decoded.screen_height as usize, h);
    }

    #[test]
    fn encoder_marks_transparency_with_gce() {
        // 2×1: one opaque red pixel + one fully-transparent pixel.
        let w = 2usize;
        let h = 1usize;
        let rgba = vec![255, 0, 0, 255, 9, 9, 9, 0];
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(w as u32);
        params.height = Some(h as u32);
        params.pixel_format = Some(PixelFormat::Rgba);
        let mut enc = make_encoder(&params).unwrap();
        let frame = CoreFrame::Video(VideoFrame {
            pts: Some(0),
            planes: vec![VideoPlane {
                stride: w * 4,
                data: rgba,
            }],
        });
        enc.send_frame(&frame).unwrap();
        let pkt = enc.receive_packet().unwrap();
        let decoded = crate::decode(&pkt.data).unwrap();
        // The frame carries a §23 GCE with a §23.c.viii Transparency
        // Index, and the transparent pixel decodes to alpha 0 through
        // the compositor.
        assert!(decoded.has_transparency());
        let composed = crate::compose(&decoded).unwrap();
        let canvas = &composed[0].canvas;
        // Pixel 1 (the transparent one) composites to fully transparent.
        assert_eq!(canvas.pixels[7], 0, "transparent pixel alpha");
        // Pixel 0 (opaque red) stays opaque.
        assert_eq!(canvas.pixels[3], 0xFF, "opaque pixel alpha");
    }
}
