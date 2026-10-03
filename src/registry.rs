//! `oxideav-core` integration layer for `oxideav-gif`.
//!
//! Gated behind the default-on `registry` Cargo feature so image-library
//! consumers can depend on `oxideav-gif` with `default-features = false`
//! and skip the `oxideav-core` dependency entirely.
//!
//! The module exposes:
//! * [`register`] / [`register_codecs`] / [`register_containers`] — the
//!   `RuntimeContext` / `CodecRegistry` / `ContainerRegistry` entry
//!   points the umbrella `oxideav` crate calls during framework
//!   initialisation.
//! * [`make_decoder`] / [`make_encoder`] — the codec factories, also
//!   installed in the registry.
//! * [`GifDecoder`] / [`GifEncoder`] — `Decoder` / `Encoder` trait
//!   wrappers that call the framework-free [`crate::decode_all`] and
//!   [`crate::encode`] (one implementation; the registry path is a thin
//!   adapter).
//! * `From<GifImage> for VideoFrame`, [`GifImage::from_video_frame`] and
//!   `TryFrom<(&VideoFrame, &CodecParameters)>` — the frame bridge, plus
//!   [`to_core_pixel_format`] / `TryFrom<PixelFormat>` for the
//!   pixel-format enums (1:1 by name) and [`to_color_signal`] /
//!   [`from_color_signal`].
//! * The `From<GifError> for oxideav_core::Error` conversion that lets
//!   trait impls bubble bitstream errors up through the framework error
//!   type.
//!
//! The `oxideav_core::register!` invocation at the bottom of the file
//! generates the `pub fn __oxideav_entry(ctx)` wrapper that
//! `oxideav-meta`'s `register_all` calls. `lib.rs` re-exports it at the
//! crate root (`oxideav_gif::__oxideav_entry`) which is the symbol meta
//! looks up.

use oxideav_core::{
    frame::VideoPlane, parse_options, CodecCapabilities, CodecId, CodecInfo, CodecOptionsStruct,
    CodecParameters, CodecRegistry, ColorPrimaries, ColorSignal, ContainerRegistry, Decoder,
    Encoder, Error as CoreError, Frame as CoreFrame, MatrixCoefficients, OptionField, OptionKind,
    OptionValue, Packet, PixelFormat, Result as CoreResult, RuntimeContext, TimeBase,
    TransferCharacteristics, VideoFrame,
};

use crate::encoder::{EncodeOptions, LzwStrategy};
use crate::error::GifError;
use crate::quantize::Dither;
use crate::types::{ColorInfo, ColorRange, GifImage, GifPixelFormat, Palette};

/// Canonical codec id for GIF image frames.
pub const CODEC_ID_STR: &str = "gif";

impl From<GifError> for CoreError {
    fn from(e: GifError) -> Self {
        match e {
            GifError::InvalidData(s) => CoreError::InvalidData(s),
            GifError::Unsupported(s) => CoreError::Unsupported(s),
            GifError::LimitExceeded(s) => {
                CoreError::InvalidData(format!("gif: limit exceeded: {s}"))
            }
            GifError::Io(e) => CoreError::Io(e),
            GifError::UnexpectedEof => {
                CoreError::InvalidData("gif: unexpected end of stream".into())
            }
            GifError::InvalidInput(s) => CoreError::InvalidData(s),
        }
    }
}

// ---- Pixel-format / colour bridges ------------------------------------------

/// [`GifPixelFormat`] → the framework's [`PixelFormat`] (1:1 by name).
pub fn to_core_pixel_format(pf: GifPixelFormat) -> PixelFormat {
    match pf {
        GifPixelFormat::Pal8 => PixelFormat::Pal8,
        GifPixelFormat::Rgb24 => PixelFormat::Rgb24,
        GifPixelFormat::Rgba => PixelFormat::Rgba,
    }
}

fn from_core_pixel_format(pf: PixelFormat) -> CoreResult<GifPixelFormat> {
    Ok(match pf {
        PixelFormat::Pal8 => GifPixelFormat::Pal8,
        PixelFormat::Rgb24 => GifPixelFormat::Rgb24,
        PixelFormat::Rgba => GifPixelFormat::Rgba,
        other => {
            return Err(CoreError::unsupported(format!(
                "GIF: pixel format {other:?} not supported (Pal8 / Rgb24 / Rgba)"
            )))
        }
    })
}

impl From<GifPixelFormat> for PixelFormat {
    fn from(pf: GifPixelFormat) -> Self {
        to_core_pixel_format(pf)
    }
}

impl TryFrom<PixelFormat> for GifPixelFormat {
    type Error = CoreError;
    fn try_from(pf: PixelFormat) -> CoreResult<Self> {
        from_core_pixel_format(pf)
    }
}

/// [`ColorInfo`] as the framework's [`ColorSignal`] (code points map
/// 1:1; `Unspecified` range stays unspecified).
pub fn to_color_signal(c: &ColorInfo) -> ColorSignal {
    let range = match c.range {
        ColorRange::Unspecified => oxideav_core::ColorRange::Unspecified,
        ColorRange::Limited => oxideav_core::ColorRange::Limited,
        ColorRange::Full => oxideav_core::ColorRange::Full,
    };
    ColorSignal::new(
        range,
        ColorPrimaries(c.primaries),
        TransferCharacteristics(c.transfer),
        MatrixCoefficients(c.matrix),
    )
}

/// The framework's [`ColorSignal`] as a [`ColorInfo`].
pub fn from_color_signal(s: &ColorSignal) -> ColorInfo {
    let range = match s.range {
        oxideav_core::ColorRange::Limited => ColorRange::Limited,
        oxideav_core::ColorRange::Full => ColorRange::Full,
        _ => ColorRange::Unspecified,
    };
    ColorInfo::new(range, s.primaries.0, s.transfer.0, s.matrix.0)
}

// ---- Frame bridge -----------------------------------------------------------

fn image_to_video_frame(image: &GifImage, pts: Option<i64>) -> VideoFrame {
    let mut frame = VideoFrame {
        pts,
        planes: vec![VideoPlane {
            stride: image.stride(),
            data: image.data().to_vec(),
        }],
    };
    stamp_frame_side_channels(&mut frame, image);
    frame
}

fn image_into_video_frame(mut image: GifImage, pts: Option<i64>) -> VideoFrame {
    let stride = image.stride();
    let data = if image.planes.is_empty() {
        Vec::new()
    } else {
        std::mem::take(&mut image.planes[0].data)
    };
    let mut frame = VideoFrame {
        pts,
        planes: vec![VideoPlane { stride, data }],
    };
    stamp_frame_side_channels(&mut frame, &image);
    frame
}

/// The palette side-channel for `Pal8` (RGB triplets — the framework
/// palette carries no alpha, so a transparent entry's RGB is kept and
/// its transparency is lost; use [`crate::decode_rgba8`] when alpha
/// matters), and the colour-signal side-channel whenever the image
/// signals more than GIF's default.
fn stamp_frame_side_channels(frame: &mut VideoFrame, image: &GifImage) {
    if let (GifPixelFormat::Pal8, Some(p)) = (image.format, &image.palette) {
        frame.set_palette(p.entries.iter().flat_map(|e| [e[0], e[1], e[2]]).collect());
    }
    let c = image.color;
    if c.primaries != ColorInfo::UNSPECIFIED
        || c.transfer != ColorInfo::UNSPECIFIED
        || c.range == ColorRange::Limited
    {
        frame.set_color_signal(to_color_signal(&c));
    }
}

impl From<GifImage> for VideoFrame {
    /// The pixel plane (`pts` `None`), plus the palette side-channel
    /// for `Pal8` and the colour-signal side-channel when the image
    /// signals a colour space.
    fn from(image: GifImage) -> Self {
        image_into_video_frame(image, None)
    }
}

impl From<&GifImage> for VideoFrame {
    fn from(image: &GifImage) -> Self {
        image_to_video_frame(image, None)
    }
}

impl GifImage {
    /// Build a [`GifImage`] from a framework [`VideoFrame`]: dimensions
    /// and pixel format come from `params` (`Rgba` when unset), the
    /// first image plane is the pixel plane, and for `Pal8` the
    /// frame's palette side-channel (RGB, all opaque) becomes
    /// [`GifImage::palette`]; the colour-signal side-channel, when
    /// attached, becomes [`GifImage::color`].
    pub fn from_video_frame(frame: &VideoFrame, params: &CodecParameters) -> CoreResult<Self> {
        let width = params
            .width
            .ok_or_else(|| CoreError::invalid("GIF: missing width"))?;
        let height = params
            .height
            .ok_or_else(|| CoreError::invalid("GIF: missing height"))?;
        let pix = from_core_pixel_format(params.pixel_format.unwrap_or(PixelFormat::Rgba))?;
        let plane = frame
            .image_planes()
            .first()
            .ok_or_else(|| CoreError::invalid("GIF: frame has no planes"))?;
        // The contract constructor requires `data.len() == stride ×
        // height`; a framework plane may carry trailing bytes, so trim
        // (after checking it is at least that long).
        let need = plane
            .stride
            .checked_mul(height as usize)
            .ok_or_else(|| CoreError::invalid("GIF: plane size overflow"))?;
        if plane.data.len() < need {
            return Err(CoreError::invalid(format!(
                "GIF: plane holds {} bytes, stride {} x height {height} needs {need}",
                plane.data.len(),
                plane.stride
            )));
        }
        let mut img = GifImage::packed(
            width,
            height,
            pix,
            plane.stride,
            plane.data[..need].to_vec(),
        )?;
        if pix == GifPixelFormat::Pal8 {
            img.palette = frame.palette().map(|rgb| {
                Palette::new(
                    rgb.chunks_exact(3)
                        .map(|c| [c[0], c[1], c[2], 255])
                        .collect(),
                )
            });
        }
        if let Some(sig) = frame.color_signal() {
            img.color = from_color_signal(&sig);
        }
        Ok(img)
    }
}

impl TryFrom<(&VideoFrame, &CodecParameters)> for GifImage {
    type Error = CoreError;
    fn try_from((frame, params): (&VideoFrame, &CodecParameters)) -> CoreResult<Self> {
        GifImage::from_video_frame(frame, params)
    }
}

// ---- CodecOptionsStruct (registry-only schema for EncodeOptions) ----------

impl CodecOptionsStruct for EncodeOptions {
    const SCHEMA: &'static [OptionField] = &[
        OptionField {
            name: "lzw_strategy",
            kind: OptionKind::Enum(&["deferred_clear", "clear_on_full"]),
            default: OptionValue::String(String::new()),
            help: "Appendix-F table-full strategy: `deferred_clear` (default; \
                   keep coding against the frozen 4096-entry table) or \
                   `clear_on_full` (emit a Clear code and rebuild).",
        },
        OptionField {
            name: "interlace",
            kind: OptionKind::Bool,
            default: OptionValue::Bool(false),
            help: "Store rows in the four-pass Appendix E interlace order \
                   (§20.c.vii Interlace Flag).",
        },
        OptionField {
            name: "max_colors",
            kind: OptionKind::U32,
            default: OptionValue::U32(256),
            help: "Colour budget for truecolour input (1..=256); the \
                   median-cut quantiser reduces to at most this many \
                   colour-table entries (one is reserved for transparency \
                   when the frame has transparent pixels).",
        },
        OptionField {
            name: "dither",
            kind: OptionKind::Enum(&[
                "none",
                "floyd_steinberg",
                "jarvis_judice_ninke",
                "stucki",
                "burkes",
                "sierra",
                "atkinson",
                "ordered_bayer8x8",
            ]),
            default: OptionValue::String(String::new()),
            help: "Index-plane assignment for truecolour input: `none` \
                   (default, nearest entry), an error-diffusion kernel, or \
                   `ordered_bayer8x8`.",
        },
        OptionField {
            name: "loop_count",
            kind: OptionKind::I32,
            default: OptionValue::I32(0),
            help: "NETSCAPE2.0 loop count for encode_animation: 0 loops \
                   forever, n repeats n times, -1 plays once (no extension).",
        },
        OptionField {
            name: "embed_metadata",
            kind: OptionKind::Bool,
            default: OptionValue::Bool(true),
            help: "Write the ICC / Exif / XMP Application Extensions from \
                   the image's metadata.",
        },
    ];
    fn apply(&mut self, key: &str, v: &OptionValue) -> CoreResult<()> {
        match key {
            "lzw_strategy" => {
                self.lzw_strategy = match v.as_str()? {
                    "" | "deferred_clear" => LzwStrategy::DeferredClear,
                    "clear_on_full" => LzwStrategy::ClearOnFull,
                    _ => unreachable!("guarded by SCHEMA"),
                }
            }
            "interlace" => self.interlace = v.as_bool()?,
            "max_colors" => {
                let n = v.as_u32()?;
                if !(1..=256).contains(&n) {
                    return Err(CoreError::invalid(format!(
                        "GIF encoder: option `max_colors` got {n}; expected 1..=256"
                    )));
                }
                self.quantize.max_colors = n as usize;
            }
            "dither" => {
                self.quantize.dither = match v.as_str()? {
                    "" | "none" => Dither::None,
                    "floyd_steinberg" => Dither::FloydSteinberg,
                    "jarvis_judice_ninke" => Dither::JarvisJudiceNinke,
                    "stucki" => Dither::Stucki,
                    "burkes" => Dither::Burkes,
                    "sierra" => Dither::Sierra,
                    "atkinson" => Dither::Atkinson,
                    "ordered_bayer8x8" => Dither::OrderedBayer8x8,
                    _ => unreachable!("guarded by SCHEMA"),
                }
            }
            "loop_count" => {
                let n = v.as_i32()?;
                self.loop_count = match n {
                    n if n < 0 => None,
                    n => Some(u16::try_from(n).map_err(|_| {
                        CoreError::invalid(format!(
                            "GIF encoder: option `loop_count` got {n}; expected -1..=65535"
                        ))
                    })?),
                };
            }
            "embed_metadata" => self.embed_metadata = v.as_bool()?,
            _ => unreachable!("guarded by SCHEMA"),
        }
        Ok(())
    }
}

// ---- Registration -----------------------------------------------------------

/// Register the GIF codec into the supplied [`CodecRegistry`].
pub fn register_codecs(reg: &mut CodecRegistry) {
    let caps = CodecCapabilities::video("gif_sw")
        .with_lossy(false)
        .with_intra_only(true)
        .with_max_size(65535, 65535)
        .with_pixel_formats(vec![
            PixelFormat::Rgba,
            PixelFormat::Rgb24,
            PixelFormat::Pal8,
        ]);
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
/// Wired into `oxideav_meta::register_all` via the
/// [`oxideav_core::register!`] macro below.
pub fn register(ctx: &mut RuntimeContext) {
    register_codecs(&mut ctx.codecs);
    register_containers(&mut ctx.containers);
}

oxideav_core::register!("gif", register);

/// Factory for the `Decoder` trait impl — registered in the codec
/// registry and called by the framework when a `gif` packet stream
/// needs decoding.
pub fn make_decoder(_params: &CodecParameters) -> CoreResult<Box<dyn Decoder>> {
    Ok(Box::new(GifDecoder::new()))
}

/// Factory for the `Encoder` trait impl. `params.width` / `height`
/// are required at `send_frame`; `params.pixel_format` selects the
/// input layout (`Rgba` default, `Rgb24`, or `Pal8` with the frame's
/// palette side-channel); `params.options` is parsed into
/// [`EncodeOptions`] (see its `CodecOptionsStruct` schema).
pub fn make_encoder(params: &CodecParameters) -> CoreResult<Box<dyn Encoder>> {
    let opts = parse_options::<EncodeOptions>(&params.options)?;
    Ok(Box::new(GifEncoder {
        output_params: params.clone(),
        opts,
        pending: std::collections::VecDeque::new(),
    }))
}

// ---- Decoder --------------------------------------------------------------

/// `Decoder` trait wrapper around the framework-free
/// [`crate::decode_all`].
///
/// One input [`Packet`] is treated as a complete `<GIF Data Stream>`
/// (Header → Logical Screen Descriptor → blocks → Trailer). Every
/// graphic-rendering block becomes one composited `Rgba`
/// [`VideoFrame`] (the §23 disposal-method state machine of
/// [`crate::compose()`]), `pts` = frame ordinal.
pub struct GifDecoder {
    codec_id: CodecId,
    queued: std::collections::VecDeque<CoreFrame>,
}

impl GifDecoder {
    /// A decoder with an empty frame queue.
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
        let frames = crate::decode_all(&packet.data)?;
        for (idx, frame) in frames.into_iter().enumerate() {
            self.queued
                .push_back(CoreFrame::Video(image_into_video_frame(
                    frame.image,
                    Some(idx as i64),
                )));
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

// ---- Encoder --------------------------------------------------------------

/// `Encoder` trait wrapper around the framework-free [`crate::encode`]:
/// accepts one [`VideoFrame`] per `send_frame` (layout per
/// `params.pixel_format`) and emits a single-image GIF byte stream per
/// `receive_packet`.
///
/// Truecolour frames are reduced to a colour table by the deterministic
/// median-cut quantiser ([`crate::quantize`]); an opaque frame using
/// ≤ 256 distinct colours keeps its exact colours. GIF has no per-pixel
/// alpha — pixels with alpha below
/// [`crate::quantize::ALPHA_OPAQUE_THRESHOLD`] route to the one
/// §23.c.viii Transparency Index. `Pal8` frames are written with their
/// palette side-channel as the Global Color Table.
pub struct GifEncoder {
    output_params: CodecParameters,
    opts: EncodeOptions,
    pending: std::collections::VecDeque<Packet>,
}

impl GifEncoder {
    /// Build an encoder from codec parameters (options parsed per the
    /// [`EncodeOptions`] schema; invalid options fall back to defaults —
    /// use [`make_encoder`] to surface them as errors).
    pub fn new_from_params(params: &CodecParameters) -> Self {
        Self {
            output_params: params.clone(),
            opts: parse_options::<EncodeOptions>(&params.options).unwrap_or_default(),
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
        let image = GifImage::from_video_frame(video, &self.output_params)?;
        if image.width == 0 || image.height == 0 {
            return Err(CoreError::invalid(
                "gif encoder: width/height must be in 1..=65535",
            ));
        }
        let bytes = crate::encode(&image, &self.opts)?;
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
        let decoded = crate::parse(&pkt.data).unwrap();
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
        let decoded = crate::parse(&pkt.data).unwrap();
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

    #[test]
    fn frame_bridge_round_trips_pal8_and_rgba() {
        let pal = Palette::new(vec![[9, 8, 7, 255], [1, 2, 3, 255]]);
        let img = GifImage::from_indexed(2, 1, vec![0, 1], pal).unwrap();
        let frame: VideoFrame = (&img).into();
        assert_eq!(frame.image_planes().len(), 1);
        assert_eq!(frame.palette(), Some(&[9u8, 8, 7, 1, 2, 3][..]));
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(2);
        params.height = Some(1);
        params.pixel_format = Some(PixelFormat::Pal8);
        let back = GifImage::try_from((&frame, &params)).unwrap();
        assert_eq!(back, img);

        let rgba = GifImage::from_rgba8(1, 1, vec![1, 2, 3, 4]).unwrap();
        let frame = VideoFrame::from(rgba.clone());
        params.width = Some(1);
        params.pixel_format = Some(PixelFormat::Rgba);
        assert_eq!(GifImage::from_video_frame(&frame, &params).unwrap(), rgba);
        params.pixel_format = Some(PixelFormat::Yuv420P);
        assert!(GifImage::from_video_frame(&frame, &params).is_err());
        assert_eq!(
            to_core_pixel_format(GifPixelFormat::Pal8),
            PixelFormat::Pal8
        );
        assert!(GifPixelFormat::try_from(PixelFormat::Gray8).is_err());
    }

    #[test]
    fn encoder_accepts_pal8_frames_with_palette_side_channel() {
        let pal = Palette::new(vec![[9, 8, 7, 255], [1, 2, 3, 255]]);
        let img = GifImage::from_indexed(2, 1, vec![0, 1], pal).unwrap();
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(2);
        params.height = Some(1);
        params.pixel_format = Some(PixelFormat::Pal8);
        let mut enc = make_encoder(&params).unwrap();
        enc.send_frame(&CoreFrame::Video(VideoFrame::from(&img)))
            .unwrap();
        let pkt = enc.receive_packet().unwrap();
        let back = crate::decode(&pkt.data).unwrap();
        assert_eq!(back, img);
    }

    #[test]
    fn encoder_options_schema_applies() {
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(1);
        params.height = Some(1);
        params.options.insert("interlace", "true");
        params.options.insert("max_colors", "16");
        params.options.insert("dither", "floyd_steinberg");
        params.options.insert("lzw_strategy", "clear_on_full");
        params.options.insert("loop_count", "-1");
        params.options.insert("embed_metadata", "false");
        let opts = parse_options::<EncodeOptions>(&params.options).unwrap();
        assert!(opts.interlace);
        assert_eq!(opts.quantize.max_colors, 16);
        assert_eq!(opts.quantize.dither, Dither::FloydSteinberg);
        assert_eq!(opts.lzw_strategy, LzwStrategy::ClearOnFull);
        assert_eq!(opts.loop_count, None);
        assert!(!opts.embed_metadata);
        params.options.insert("max_colors", "0");
        assert!(make_encoder(&params).is_err());
        params.options.insert("max_colors", "2");
        params.options.insert("dither", "bogus");
        assert!(make_encoder(&params).is_err());
    }
}
