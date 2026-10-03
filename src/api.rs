//! The root vocabulary of the image-crate contract (`IMAGE_CRATE_API`):
//! `probe` / `info` / `decode*` / `encode*`, all framework-free, built
//! on the GIF Data Stream model ([`GifFile`]) and the §23 compositor
//! ([`crate::compose()`]).

use core::time::Duration;
use std::io::{Read, Write};

use crate::app_ext::{ExifMetadata, IccProfile, XmpPacket};
use crate::compose::compose;
use crate::decoder::{parse_with, scan};
use crate::encoder::{encode_file_with, EncodeOptions};
use crate::error::{GifError as Error, Result};
use crate::image::{Block, DisposalMethod, GifFile, GifFrameData, GraphicControl, Rgb, Version};
use crate::options::DecodeOptions;
use crate::types::{
    Frame, GifImage, ImageInfo, Metadata, Palette, PixelFormat, RgbImage, RgbaImage,
};

/// `true` when `bytes` starts with a §17 Header — `GIF87a` or `GIF89a`.
/// Allocation-free; `false` on short input.
pub fn probe(bytes: &[u8]) -> bool {
    bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a")
}

/// Describe a GIF from its header walk without expanding any LZW
/// raster: Logical Screen dimensions, the layout [`decode`] would
/// return, the number of graphic-rendering blocks ([`decode_all`]'s
/// length), alpha, metadata presence, version, loop count.
///
/// The §17 header, §18 Logical Screen Descriptor, §19 Global Color
/// Table and every block up to the first image must be well-formed;
/// a structural fault after that point ends the walk (the counts
/// cover what was readable), matching the success domain of
/// [`decode`].
pub fn info(bytes: &[u8]) -> Result<ImageInfo> {
    let s = scan(bytes, &DecodeOptions::default().unlimited(), false)?;
    let file = &s.file;
    let first = file.frames().next();
    let layout = first.map(|f| FirstFrameLayout::of(file, f));
    let format = match &layout {
        Some(FirstFrameLayout::Rgba) => PixelFormat::Rgba,
        _ => PixelFormat::Pal8,
    };
    let mut out = ImageInfo::new(
        u32::from(file.screen_width),
        u32::from(file.screen_height),
        format,
    );
    out.frames = s.image_count + s.plain_text_count;
    out.image_count = s.image_count;
    out.has_alpha = match (&layout, first) {
        (Some(FirstFrameLayout::Rgba), _) => true,
        (Some(FirstFrameLayout::Pal8 { transparent, .. }), _) => transparent.is_some(),
        (None, _) => false,
    };
    out.has_icc = file.icc_profile().is_some();
    out.has_exif = file.exif().is_some();
    out.has_xmp = file.xmp_packet().is_some();
    out.version = file.version;
    out.has_global_palette = file.global_palette.is_some();
    out.palette_entries = first
        .and_then(|f| {
            f.local_palette
                .as_deref()
                .or(file.global_palette.as_deref())
        })
        .map(|p| p.len() as u32)
        .unwrap_or(0);
    out.interlaced = first.is_some_and(|f| f.interlaced);
    out.loop_count = file.loop_count();
    out.pixel_aspect_ratio = file.pixel_aspect_ratio;
    Ok(out)
}

/// Decode the first §20 image, composed onto the §18 Logical Screen,
/// in its native layout with [`DecodeOptions::default`]. See
/// [`GifImage`] for the `Pal8` / `Rgba` rule and the crate docs for
/// the compositing semantics.
pub fn decode(bytes: &[u8]) -> Result<GifImage> {
    decode_with(bytes, &DecodeOptions::default())
}

/// [`decode`] under explicit limits / strictness.
///
/// Only the first image's LZW raster is expanded (the remaining blocks
/// are walked structurally for the ICC / Exif / XMP extensions), except
/// under `strict`, where the whole stream is parsed and run through
/// [`GifFile::validate_strict`] first. `lenient` selects the recovery
/// parser for the whole stream.
pub fn decode_with(bytes: &[u8], opts: &DecodeOptions) -> Result<GifImage> {
    if opts.strict && opts.lenient {
        return Err(Error::invalid_input(
            "DecodeOptions: strict and lenient are mutually exclusive",
        ));
    }
    if opts.strict || opts.lenient {
        let file = parse_with(bytes, opts)?;
        return first_frame_image(&file, opts);
    }
    // Fast path: the first image's raster plus a structural walk of
    // the rest for the metadata extensions.
    let s = scan(bytes, opts, true)?;
    first_frame_image(&s.file, opts)
}

/// Decode straight to tightly packed 8-bit RGB (alpha dropped).
pub fn decode_rgb8(bytes: &[u8]) -> Result<RgbImage> {
    let img = decode(bytes)?;
    Ok(RgbImage::new(img.width, img.height, img.to_rgb8()))
}

/// Decode straight to tightly packed 8-bit RGBA (transparent pixels at
/// alpha `0`, everything else `255`).
pub fn decode_rgba8(bytes: &[u8]) -> Result<RgbaImage> {
    let img = decode(bytes)?;
    Ok(RgbaImage::new(img.width, img.height, img.to_rgba8()))
}

/// Every graphic-rendering block of the stream as a composited
/// logical-screen canvas (`Rgba`), per the §23 disposal-method state
/// machine of [`crate::compose()`] — what a viewer shows after each
/// frame — with the block's §23 delay / disposal / user-input flag.
/// Uses [`DecodeOptions::default`].
///
/// A stream with no graphic-rendering block is [`Error::InvalidData`],
/// like [`decode`].
pub fn decode_all(bytes: &[u8]) -> Result<Vec<Frame>> {
    decode_all_with(bytes, &DecodeOptions::default())
}

/// [`decode_all`] under explicit limits / strictness. `max_bytes` also
/// bounds `frames × 4 × width × height`, the size of the result.
pub fn decode_all_with(bytes: &[u8], opts: &DecodeOptions) -> Result<Vec<Frame>> {
    let file = parse_with(bytes, opts)?;
    frames_of(&file, opts)
}

/// Read `r` to its end and [`decode`] the bytes.
pub fn decode_from<R: Read>(mut r: R) -> Result<GifImage> {
    let mut buf = Vec::new();
    r.read_to_end(&mut buf)?;
    decode(&buf)
}

/// Encode `image` as a single-image GIF.
///
/// * `Pal8` — the palette is written as the §19 Global Color Table
///   exactly as given (RGB); the first entry whose alpha is below
///   [`crate::quantize::ALPHA_OPAQUE_THRESHOLD`] becomes the §23.c.viii
///   Transparency Index, and pixels at any *other* such entry are
///   re-pointed to it (GIF has one transparent index per image).
///   Indices past the palette are [`Error::InvalidInput`].
/// * `Rgb24` / `Rgba` — reduced to a colour table with
///   [`EncodeOptions::quantize`] exactly as [`encode_rgb8`] /
///   [`encode_rgba8`] do (documented conversion, not a silent one:
///   GIF has no truecolour layout).
///
/// Dimensions above 65 535 are [`Error::Unsupported`]. With
/// [`EncodeOptions::embed_metadata`], `metadata.icc` / `exif` / `xmp`
/// become the de-facto Application Extensions; the version is `GIF89a`
/// when any extension is written, `GIF87a` otherwise.
pub fn encode(image: &GifImage, opts: &EncodeOptions) -> Result<Vec<u8>> {
    let file = file_from_image(image, opts)?;
    encode_file_with(&file, opts)
}

/// Encode tightly packed 8-bit RGB (`3 × width × height` bytes),
/// quantised to a ≤256-entry colour table with
/// [`EncodeOptions::quantize`] (median cut, deterministic; lossless
/// when the input has at most that many distinct colours).
pub fn encode_rgb8(width: u32, height: u32, rgb: &[u8], opts: &EncodeOptions) -> Result<Vec<u8>> {
    let need = check_raw_len(width, height, 3, rgb.len())?;
    encode(
        &GifImage::from_rgb8(width, height, rgb[..need].to_vec())?,
        opts,
    )
}

/// Encode tightly packed 8-bit RGBA (`4 × width × height` bytes),
/// quantised like [`encode_rgb8`]; every pixel with alpha below
/// [`crate::quantize::ALPHA_OPAQUE_THRESHOLD`] (128) maps to the one
/// §23.c.viii Transparency Index (GIF has no per-pixel alpha), the
/// rest are opaque.
pub fn encode_rgba8(width: u32, height: u32, rgba: &[u8], opts: &EncodeOptions) -> Result<Vec<u8>> {
    let need = check_raw_len(width, height, 4, rgba.len())?;
    encode(
        &GifImage::from_rgba8(width, height, rgba[..need].to_vec())?,
        opts,
    )
}

/// [`encode`] straight into a writer.
pub fn encode_to<W: Write>(image: &GifImage, opts: &EncodeOptions, mut w: W) -> Result<()> {
    let bytes = encode(image, opts)?;
    w.write_all(&bytes)?;
    Ok(())
}

/// Encode an animation from full-canvas frames (the shape
/// [`decode_all`] returns): every `frame.image` must share one
/// geometry (≤ 65 535 per side) and is quantised independently to its
/// own §21 Local Color Table with [`EncodeOptions::quantize`]
/// (transparent pixels as in [`encode_rgba8`]); `frame.delay`
/// (rounded to §23.c.vii centiseconds, `None` = 0) and
/// `frame.disposal` go into each frame's Graphic Control Extension;
/// [`EncodeOptions::loop_count`] writes the NETSCAPE2.0 extension;
/// [`EncodeOptions::interlace`] sets every frame's Interlace Flag; the
/// first frame's metadata is embedded when
/// [`EncodeOptions::embed_metadata`].
///
/// For anything finer (shared palettes, sub-rectangle frames, Plain
/// Text, comments) build a [`GifFile`] — see [`crate::AnimationBuilder`]
/// and [`GifFile::from_rgba_frames_shared_palette`].
pub fn encode_animation(frames: &[Frame], opts: &EncodeOptions) -> Result<Vec<u8>> {
    let first = frames
        .first()
        .ok_or_else(|| Error::invalid_input("encode_animation: at least one frame is required"))?;
    let (width, height) = (first.image.width, first.image.height);
    let (w16, h16) = wire_dims(width, height)?;
    let mut rgba_frames: Vec<Vec<u8>> = Vec::with_capacity(frames.len());
    for (i, f) in frames.iter().enumerate() {
        if f.image.width != width || f.image.height != height {
            return Err(Error::invalid_input(format!(
                "encode_animation: frame {i} is {}x{} but frame 0 is {width}x{height}",
                f.image.width, f.image.height
            )));
        }
        rgba_frames.push(f.image.to_rgba8());
    }
    let spec: Vec<(&[u8], u16, DisposalMethod)> = frames
        .iter()
        .zip(&rgba_frames)
        .map(|(f, rgba)| (rgba.as_slice(), delay_centis(f.delay), f.disposal))
        .collect();
    let mut file =
        GifFile::from_rgba_frames_with_options(&spec, w16, h16, opts.quantize, opts.loop_count)?;
    if opts.interlace {
        file.set_frames_interlaced(true);
    }
    if opts.embed_metadata {
        let extra = metadata_blocks(&first.image.metadata);
        if !extra.is_empty() {
            // Metadata extensions lead the stream, after any NETSCAPE2.0
            // block the builder emitted.
            let at = file
                .blocks
                .iter()
                .position(|b| matches!(b, Block::Image(_)))
                .unwrap_or(file.blocks.len());
            file.blocks.splice(at..at, extra);
        }
    }
    file.upgrade_version_if_needed();
    encode_file_with(&file, opts)
}

impl GifFile {
    /// Parse a GIF Data Stream — see [`crate::parse`].
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        crate::decoder::parse(bytes)
    }

    /// Parse under explicit [`DecodeOptions`] — see [`crate::parse_with`].
    pub fn parse_with(bytes: &[u8], opts: &DecodeOptions) -> Result<Self> {
        parse_with(bytes, opts)
    }

    /// Serialise with default options — see [`crate::encode_file`].
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        crate::encoder::encode_file(self)
    }

    /// Serialise with explicit options — see [`crate::encode_file_with`].
    pub fn to_bytes_with(&self, opts: &EncodeOptions) -> Result<Vec<u8>> {
        encode_file_with(self, opts)
    }

    /// The first §20 image composed onto the Logical Screen — what
    /// [`crate::decode`] returns for this stream's bytes. Transparent
    /// pixels (outside the image rectangle or at the §23.c.viii
    /// Transparency Index) follow the [`GifImage`] layout rule.
    pub fn first_image(&self) -> Result<GifImage> {
        first_frame_image(self, &DecodeOptions::default().unlimited())
    }

    /// Every graphic-rendering block composited — what
    /// [`crate::decode_all`] returns for this stream's bytes.
    pub fn frames_composited(&self) -> Result<Vec<Frame>> {
        frames_of(self, &DecodeOptions::default().unlimited())
    }

    /// The contract [`Metadata`] view of the ICC / Exif / XMP
    /// Application Extensions.
    pub fn metadata(&self) -> Metadata {
        Metadata::new()
            .with_icc(self.icc_profile().map(<[u8]>::to_vec))
            .with_exif(self.exif().map(<[u8]>::to_vec))
            .with_xmp(self.xmp_packet().map(<[u8]>::to_vec))
    }
}

// ---- Internals -----------------------------------------------------------

/// How the first image's composition is represented.
enum FirstFrameLayout {
    /// One colour table describes the canvas. `transparent` is the
    /// index every transparent canvas pixel (outside the image
    /// rectangle, or at the Transparency Index) takes; `synthetic` is
    /// `true` when that index is an extra `[0, 0, 0, 0]` entry appended
    /// past the file's table.
    Pal8 {
        transparent: Option<u8>,
        synthetic: bool,
    },
    /// 256 opaque entries and transparent pixels to show: expand.
    Rgba,
}

impl FirstFrameLayout {
    fn of(file: &GifFile, f: &GifFrameData) -> Self {
        let palette_len = f
            .local_palette
            .as_deref()
            .or(file.global_palette.as_deref())
            .map_or(0, <[Rgb]>::len);
        let covers = f.left == 0
            && f.top == 0
            && f.width == file.screen_width
            && f.height == file.screen_height;
        let ti = f.graphic_control.as_ref().and_then(|g| g.transparent_index);
        let ti_in_range = ti.filter(|&t| usize::from(t) < palette_len);
        if let Some(t) = ti_in_range {
            return Self::Pal8 {
                transparent: Some(t),
                synthetic: false,
            };
        }
        // No usable transparent entry. Does anything need one?
        if covers && ti.is_none() {
            return Self::Pal8 {
                transparent: None,
                synthetic: false,
            };
        }
        if palette_len < 256 {
            return Self::Pal8 {
                transparent: Some(palette_len as u8),
                synthetic: true,
            };
        }
        Self::Rgba
    }
}

/// Compose the first §20 image of `file` onto the Logical Screen.
fn first_frame_image(file: &GifFile, opts: &DecodeOptions) -> Result<GifImage> {
    let f = file
        .frames()
        .next()
        .ok_or_else(|| Error::invalid("GIF: stream contains no image block"))?;
    let table = f
        .local_palette
        .as_deref()
        .or(file.global_palette.as_deref())
        .ok_or_else(|| {
            Error::invalid("frame has no local palette and stream has no global palette")
        })?;
    let (sw, sh) = (
        usize::from(file.screen_width),
        usize::from(file.screen_height),
    );
    let (fl, ft, fw, fh) = (
        usize::from(f.left),
        usize::from(f.top),
        usize::from(f.width),
        usize::from(f.height),
    );
    if fl + fw > sw || ft + fh > sh {
        return Err(Error::invalid(format!(
            "block placement ({},{},{}×{}) escapes logical screen ({}×{})",
            f.left, f.top, f.width, f.height, file.screen_width, file.screen_height
        )));
    }
    if f.indices.len() != fw * fh {
        return Err(Error::invalid(format!(
            "frame raster holds {} indices, {}×{} needs {}",
            f.indices.len(),
            f.width,
            f.height,
            fw * fh
        )));
    }
    let ti = f.graphic_control.as_ref().and_then(|g| g.transparent_index);
    if let Some(bad) = f
        .indices
        .iter()
        .find(|&&i| Some(i) != ti && usize::from(i) >= table.len())
    {
        return Err(Error::invalid(format!(
            "frame pixel index {bad} out of range for palette of {} entries",
            table.len()
        )));
    }
    let metadata = file.metadata();
    let (width, height) = (u32::from(file.screen_width), u32::from(file.screen_height));

    match FirstFrameLayout::of(file, f) {
        FirstFrameLayout::Pal8 {
            transparent,
            synthetic,
        } => {
            opts.check_bytes((sw * sh) as u64)?;
            let mut palette = Palette::from_color_table(table, transparent);
            if synthetic {
                // The transparent slot goes right past the file's table,
                // then the table is padded to the next power of two with
                // opaque black — the shape a §19 / §21 colour table takes
                // on the wire — so `decode(encode(img)) == img` holds for
                // this image too.
                palette.entries.push([0, 0, 0, 0]);
                let padded = palette.len().next_power_of_two().clamp(2, 256);
                palette.entries.resize(padded, [0, 0, 0, 255]);
            }
            let fill = transparent.unwrap_or(0);
            let mut indices = vec![fill; sw * sh];
            for y in 0..fh {
                let dst = &mut indices[(ft + y) * sw + fl..(ft + y) * sw + fl + fw];
                let src = &f.indices[y * fw..(y + 1) * fw];
                if let Some(t) = transparent {
                    // Pixels at the file's Transparency Index (which may
                    // be past the table when the slot is synthetic) all
                    // take the canvas's transparent index.
                    for (d, &s) in dst.iter_mut().zip(src) {
                        *d = if Some(s) == ti { t } else { s };
                    }
                } else {
                    dst.copy_from_slice(src);
                }
            }
            let img = GifImage::packed(width, height, PixelFormat::Pal8, sw, indices)?;
            Ok(img.with_palette(palette).with_metadata(metadata))
        }
        FirstFrameLayout::Rgba => {
            opts.check_bytes((sw * sh * 4) as u64)?;
            let mut rgba = vec![0u8; sw * sh * 4];
            for y in 0..fh {
                let row = &f.indices[y * fw..(y + 1) * fw];
                let base = ((ft + y) * sw + fl) * 4;
                for (x, &i) in row.iter().enumerate() {
                    if Some(i) == ti {
                        continue;
                    }
                    let Rgb { r, g, b } = table[usize::from(i)];
                    rgba[base + x * 4..base + x * 4 + 4].copy_from_slice(&[r, g, b, 255]);
                }
            }
            let img = GifImage::from_rgba8(width, height, rgba)?;
            Ok(img.with_metadata(metadata))
        }
    }
}

/// Every graphic-rendering block of `file`, composited.
fn frames_of(file: &GifFile, opts: &DecodeOptions) -> Result<Vec<Frame>> {
    let n = file.graphic_rendering_block_count();
    if n == 0 {
        return Err(Error::invalid(
            "GIF: stream contains no graphic-rendering block",
        ));
    }
    let canvas = u64::from(file.screen_width) * u64::from(file.screen_height) * 4;
    opts.check_bytes(canvas.saturating_mul(n as u64))?;
    let composed = compose(file)?;
    let metadata = file.metadata();
    let (width, height) = (u32::from(file.screen_width), u32::from(file.screen_height));
    let gces = file.blocks.iter().filter_map(|b| match b {
        Block::Image(f) => Some(f.graphic_control.as_ref()),
        Block::PlainText {
            graphic_control, ..
        } => Some(graphic_control.as_ref()),
        _ => None,
    });
    composed
        .into_iter()
        .zip(gces)
        .map(|(cf, gce)| {
            let img = GifImage::from_rgba8(width, height, cf.canvas.pixels)?
                .with_metadata(metadata.clone());
            let mut frame = Frame::new(
                img,
                gce.map(|g| Duration::from_millis(u64::from(g.delay_centis) * 10)),
            );
            if let Some(g) = gce {
                frame.disposal = g.disposal;
                frame.user_input = g.user_input;
            }
            Ok(frame)
        })
        .collect()
}

/// `width` / `height` as the 16-bit wire fields.
fn wire_dims(width: u32, height: u32) -> Result<(u16, u16)> {
    match (u16::try_from(width), u16::try_from(height)) {
        (Ok(w), Ok(h)) => Ok((w, h)),
        _ => Err(Error::unsupported(format!(
            "GIF: {width}x{height} exceeds the 65535x65535 wire limit"
        ))),
    }
}

/// `Option<Duration>` → §23.c.vii centiseconds (rounded to nearest,
/// saturating).
fn delay_centis(d: Option<Duration>) -> u16 {
    let Some(d) = d else {
        return 0;
    };
    let centis = (d.as_millis() + 5) / 10;
    u16::try_from(centis).unwrap_or(u16::MAX)
}

/// The de-facto Application Extensions for `metadata`.
fn metadata_blocks(metadata: &Metadata) -> Vec<Block> {
    let mut out = Vec::new();
    if let Some(icc) = &metadata.icc {
        out.push(Block::Application(
            IccProfile { bytes: icc.clone() }.to_application(),
        ));
    }
    if let Some(exif) = &metadata.exif {
        out.push(Block::Application(
            ExifMetadata::new(exif.clone()).to_application(),
        ));
    }
    if let Some(xmp) = &metadata.xmp {
        out.push(Block::Application(
            XmpPacket { bytes: xmp.clone() }.to_application(),
        ));
    }
    out
}

/// Build the single-image [`GifFile`] that [`encode`] serialises.
fn file_from_image(image: &GifImage, opts: &EncodeOptions) -> Result<GifFile> {
    let (w16, h16) = wire_dims(image.width, image.height)?;
    let (w, h) = (image.width as usize, image.height as usize);
    let stride = image.stride();
    let data = image.as_bytes().unwrap_or(&[]);
    let row = w * image.bytes_per_pixel();
    if stride < row || data.len() < stride.saturating_mul(h) {
        return Err(Error::invalid_input(format!(
            "GIF: plane of {} bytes (stride {stride}) cannot hold {w}x{h} {:?}",
            data.len(),
            image.format
        )));
    }

    let (table, indices, transparent): (Vec<Rgb>, Vec<u8>, Option<u8>) = match image.format {
        PixelFormat::Pal8 => {
            let palette = image
                .palette
                .as_ref()
                .ok_or_else(|| Error::invalid_input("GIF: Pal8 image without a palette"))?;
            if palette.is_empty() || palette.len() > 256 {
                return Err(Error::invalid_input(format!(
                    "GIF: palette must hold 1..=256 entries, got {}",
                    palette.len()
                )));
            }
            let ti = palette.transparent_index();
            let mut indices = Vec::with_capacity(w * h);
            for y in 0..h {
                let src = &data[y * stride..y * stride + w];
                for &i in src {
                    let e = palette.get(i).ok_or_else(|| {
                        Error::invalid_input(format!(
                            "GIF: index {i} is past the {}-entry palette",
                            palette.len()
                        ))
                    })?;
                    // One Transparency Index per image: fold every
                    // transparent entry onto the first one.
                    indices.push(match ti {
                        Some(t) if e[3] < crate::quantize::ALPHA_OPAQUE_THRESHOLD => t,
                        _ => i,
                    });
                }
            }
            (palette.to_color_table(), indices, ti)
        }
        PixelFormat::Rgb24 => {
            let q =
                crate::quantize::quantize_rgb_with_options(&image.to_rgb8(), w, h, opts.quantize)?;
            (q.palette, q.indices, None)
        }
        PixelFormat::Rgba => {
            let q = crate::quantize::quantize_rgba_with_options(
                &image.to_rgba8(),
                w,
                h,
                opts.quantize,
            )?;
            (q.palette, q.indices, q.transparent_index)
        }
    };

    let graphic_control = transparent.map(|t| GraphicControl {
        disposal: DisposalMethod::None,
        user_input: false,
        transparent_index: Some(t),
        delay_centis: 0,
    });
    let mut blocks = Vec::new();
    if opts.embed_metadata {
        blocks.extend(metadata_blocks(&image.metadata));
    }
    blocks.push(Block::Image(GifFrameData {
        left: 0,
        top: 0,
        width: w16,
        height: h16,
        local_palette: None,
        palette_sorted: false,
        interlaced: opts.interlace,
        indices,
        graphic_control,
    }));
    let mut file = GifFile {
        version: Version::Gif87a,
        screen_width: w16,
        screen_height: h16,
        color_resolution: color_resolution_for(table.len()),
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(table),
        blocks,
    };
    file.upgrade_version_if_needed();
    Ok(file)
}

/// §18.c.iv Color Resolution for a table of `n` entries (bits per
/// primary minus one; 8-bit primaries for any table over 128 entries).
fn color_resolution_for(n: usize) -> u8 {
    let mut bits = 1u8;
    while (1usize << bits) < n.max(1) && bits < 8 {
        bits += 1;
    }
    bits - 1
}

fn check_raw_len(width: u32, height: u32, bpp: usize, len: usize) -> Result<usize> {
    let need = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(bpp))
        .ok_or_else(|| Error::invalid_input("GIF encoder: dimensions overflow"))?;
    if len < need {
        return Err(Error::invalid_input(format!(
            "GIF encoder: {width}x{height} at {bpp} bytes/pixel needs {need} bytes, got {len}"
        )));
    }
    Ok(need)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::compose;
    use crate::encoder::encode_file;

    fn two_colour() -> Vec<Rgb> {
        vec![Rgb::new(255, 0, 0), Rgb::new(0, 0, 255)]
    }

    fn single(
        screen: (u16, u16),
        rect: (u16, u16, u16, u16),
        gce: Option<GraphicControl>,
    ) -> GifFile {
        let (l, t, w, h) = rect;
        let indices: Vec<u8> = (0..usize::from(w) * usize::from(h))
            .map(|i| (i % 2) as u8)
            .collect();
        GifFile {
            version: Version::Gif89a,
            screen_width: screen.0,
            screen_height: screen.1,
            color_resolution: 0,
            global_palette_sorted: false,
            background_index: 0,
            pixel_aspect_ratio: 0,
            global_palette: Some(two_colour()),
            blocks: vec![Block::Image(GifFrameData {
                left: l,
                top: t,
                width: w,
                height: h,
                local_palette: None,
                palette_sorted: false,
                interlaced: false,
                indices,
                graphic_control: gce,
            })],
        }
    }

    /// `to_rgba8()` of the contract image equals the compositor's
    /// first canvas wherever alpha is 255, and has alpha 0 exactly
    /// where the compositor left the canvas transparent.
    fn assert_matches_compose(bytes: &[u8]) {
        let img = decode(bytes).unwrap();
        let file = crate::parse(bytes).unwrap();
        let canvas = &compose(&file).unwrap()[0].canvas;
        let rgba = img.to_rgba8();
        assert_eq!(rgba.len(), canvas.pixels.len());
        for (a, b) in rgba.chunks_exact(4).zip(canvas.pixels.chunks_exact(4)) {
            assert_eq!(a[3], b[3], "alpha differs");
            if a[3] == 255 {
                assert_eq!(a, b);
            }
        }
    }

    #[test]
    fn probe_sniffs_both_versions() {
        assert!(probe(b"GIF87a\0\0"));
        assert!(probe(b"GIF89a"));
        assert!(!probe(b"GIF88a"));
        assert!(!probe(b"GIF8"));
        assert!(!probe(b""));
    }

    #[test]
    fn full_cover_frame_is_pal8_without_transparency() {
        let bytes = encode_file(&single((4, 2), (0, 0, 4, 2), None)).unwrap();
        let img = decode(&bytes).unwrap();
        assert_eq!(img.format, PixelFormat::Pal8);
        assert_eq!((img.width, img.height), (4, 2));
        assert!(!img.has_alpha());
        assert_eq!(img.palette.as_ref().unwrap().len(), 2);
        assert_eq!(img.as_bytes().unwrap(), &[0, 1, 0, 1, 0, 1, 0, 1]);
        assert_matches_compose(&bytes);
        let i = info(&bytes).unwrap();
        assert_eq!(i.format, PixelFormat::Pal8);
        assert!(!i.has_alpha);
        assert_eq!(i.frames, 1);
        assert_eq!(i.palette_entries, 2);
    }

    #[test]
    fn partial_frame_gets_synthetic_transparent_entry() {
        let bytes = encode_file(&single((4, 3), (1, 1, 2, 1), None)).unwrap();
        let img = decode(&bytes).unwrap();
        assert_eq!(img.format, PixelFormat::Pal8);
        let pal = img.palette.as_ref().unwrap();
        assert_eq!(pal.len(), 4, "2 entries + synthetic slot, padded to 4");
        assert_eq!(pal.entries[2], [0, 0, 0, 0]);
        assert_eq!(pal.entries[3], [0, 0, 0, 255]);
        assert_eq!(
            img.as_bytes().unwrap(),
            &[2, 2, 2, 2, 2, 0, 1, 2, 2, 2, 2, 2]
        );
        assert!(img.has_alpha());
        assert_matches_compose(&bytes);
        assert!(info(&bytes).unwrap().has_alpha);
    }

    #[test]
    fn transparent_index_is_reused_for_uncovered_pixels() {
        let gce = GraphicControl {
            transparent_index: Some(1),
            ..Default::default()
        };
        let bytes = encode_file(&single((3, 1), (0, 0, 2, 1), Some(gce))).unwrap();
        let img = decode(&bytes).unwrap();
        assert_eq!(img.format, PixelFormat::Pal8);
        let pal = img.palette.as_ref().unwrap();
        assert_eq!(pal.len(), 2);
        assert_eq!(pal.entries[1][3], 0);
        assert_eq!(img.as_bytes().unwrap(), &[0, 1, 1]);
        assert_matches_compose(&bytes);
    }

    #[test]
    fn full_opaque_table_with_partial_frame_falls_back_to_rgba() {
        let table: Vec<Rgb> = (0..=255u8).map(|i| Rgb::new(i, i, i)).collect();
        let mut file = single((3, 1), (0, 0, 2, 1), None);
        file.global_palette = Some(table);
        file.color_resolution = 7;
        let bytes = encode_file(&file).unwrap();
        assert_eq!(info(&bytes).unwrap().format, PixelFormat::Rgba);
        let img = decode(&bytes).unwrap();
        assert_eq!(img.format, PixelFormat::Rgba);
        assert_eq!(
            img.as_bytes().unwrap(),
            &[0, 0, 0, 255, 1, 1, 1, 255, 0, 0, 0, 0]
        );
        assert_matches_compose(&bytes);
    }

    #[test]
    fn decode_all_matches_compose_and_carries_gce() {
        let gce = GraphicControl {
            disposal: DisposalMethod::RestoreBackground,
            user_input: true,
            transparent_index: None,
            delay_centis: 7,
        };
        let mut file = single((2, 1), (0, 0, 2, 1), Some(gce));
        let second = GifFrameData {
            left: 1,
            top: 0,
            width: 1,
            height: 1,
            local_palette: None,
            palette_sorted: false,
            interlaced: false,
            indices: vec![1],
            graphic_control: None,
        };
        file.blocks.push(Block::Image(second));
        let bytes = encode_file(&file).unwrap();
        let frames = decode_all(&bytes).unwrap();
        let composed = compose(&file).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(info(&bytes).unwrap().frames, 2);
        for (f, c) in frames.iter().zip(&composed) {
            assert_eq!(f.image.format, PixelFormat::Rgba);
            assert_eq!(f.image.as_bytes().unwrap(), c.canvas.pixels.as_slice());
        }
        assert_eq!(frames[0].delay, Some(Duration::from_millis(70)));
        assert_eq!(frames[0].disposal, DisposalMethod::RestoreBackground);
        assert!(frames[0].user_input);
        assert_eq!(frames[1].delay, None);
        assert_eq!(frames[1].disposal, DisposalMethod::None);
    }

    #[test]
    fn pal8_round_trip_is_lossless() {
        let gce = GraphicControl {
            transparent_index: Some(1),
            ..Default::default()
        };
        let bytes = encode_file(&single((4, 2), (0, 0, 4, 2), Some(gce))).unwrap();
        let img = decode(&bytes).unwrap();
        let again = decode(&encode(&img, &EncodeOptions::default()).unwrap()).unwrap();
        assert_eq!(again, img);
        // And the raw paths agree with the image kernels.
        assert_eq!(decode_rgba8(&bytes).unwrap().data, img.to_rgba8());
        assert_eq!(decode_rgb8(&bytes).unwrap().data, img.to_rgb8());
    }

    #[test]
    fn rgb8_round_trip_is_exact_under_256_colours() {
        let w = 16u32;
        let h = 16u32;
        let rgb: Vec<u8> = (0..w * h)
            .flat_map(|i| [(i % 16) as u8 * 16, (i / 16) as u8 * 16, 7])
            .collect();
        let bytes = encode_rgb8(w, h, &rgb, &EncodeOptions::default()).unwrap();
        assert_eq!(decode_rgb8(&bytes).unwrap().data, rgb);
        let img = decode(&bytes).unwrap();
        assert_eq!(img.format, PixelFormat::Pal8);
        assert_eq!(img.palette.as_ref().unwrap().len(), 256);
    }

    #[test]
    fn rgba8_alpha_threshold_becomes_transparent_index() {
        let rgba = vec![255, 0, 0, 255, 0, 255, 0, 127, 0, 0, 255, 128];
        let bytes = encode_rgba8(3, 1, &rgba, &EncodeOptions::default()).unwrap();
        let out = decode_rgba8(&bytes).unwrap().data;
        assert_eq!(&out[0..4], &[255, 0, 0, 255]);
        assert_eq!(out[7], 0, "alpha 127 is transparent");
        assert_eq!(&out[8..12], &[0, 0, 255, 255], "alpha 128 is opaque");
        assert!(info(&bytes).unwrap().has_alpha);
    }

    #[test]
    fn encode_folds_multiple_transparent_entries() {
        let pal = Palette::new(vec![[1, 1, 1, 255], [2, 2, 2, 0], [3, 3, 3, 10]]);
        let img = GifImage::from_indexed(3, 1, vec![0, 1, 2], pal).unwrap();
        let bytes = encode(&img, &EncodeOptions::default()).unwrap();
        let back = decode(&bytes).unwrap();
        assert_eq!(back.as_bytes().unwrap(), &[0, 1, 1]);
        assert_eq!(back.palette.as_ref().unwrap().entries[2], [3, 3, 3, 255]);
    }

    #[test]
    fn metadata_round_trips_through_application_extensions() {
        let md = Metadata::new()
            .with_icc(vec![1, 2, 3])
            .with_exif(b"II*\0rest".to_vec())
            .with_xmp(b"<x:xmpmeta/>".to_vec());
        let img = GifImage::from_rgb8(1, 1, vec![9, 9, 9])
            .unwrap()
            .with_metadata(md.clone());
        let bytes = encode(&img, &EncodeOptions::default()).unwrap();
        let i = info(&bytes).unwrap();
        assert!(i.has_icc && i.has_exif && i.has_xmp);
        assert_eq!(i.version, Version::Gif89a);
        let back = decode(&bytes).unwrap();
        assert_eq!(back.metadata, md);
        let stripped = encode(&img, &EncodeOptions::default().with_embed_metadata(false)).unwrap();
        assert!(decode(&stripped).unwrap().metadata.is_empty());
        assert_eq!(info(&stripped).unwrap().version, Version::Gif87a);
    }

    #[test]
    fn limits_fire_before_allocation() {
        let bytes = encode_file(&single((4, 2), (0, 0, 4, 2), None)).unwrap();
        let tight = DecodeOptions::default().with_max_pixels(7u64);
        assert!(matches!(
            decode_with(&bytes, &tight),
            Err(Error::LimitExceeded(_))
        ));
        assert!(matches!(
            decode_all_with(&bytes, &DecodeOptions::default().with_max_bytes(31u64)),
            Err(Error::LimitExceeded(_))
        ));
        assert!(matches!(
            decode_with(&bytes, &DecodeOptions::default().with_max_width(3u32)),
            Err(Error::LimitExceeded(_))
        ));
        assert!(decode_with(&bytes, &DecodeOptions::default().with_max_pixels(8u64)).is_ok());
    }

    #[test]
    fn strict_and_lenient_modes() {
        let mut bytes = encode_file(&single((4, 2), (0, 0, 4, 2), None)).unwrap();
        // Strict rejects a background index past the table.
        let mut bad = single((4, 2), (0, 0, 4, 2), None);
        bad.background_index = 9;
        let bad_bytes = encode_file(&bad).unwrap();
        assert!(decode(&bad_bytes).is_ok());
        assert!(matches!(
            decode_with(&bad_bytes, &DecodeOptions::default().with_strict(true)),
            Err(Error::InvalidData(_))
        ));
        // Lenient accepts a stream without its §27 Trailer.
        bytes.pop();
        assert!(matches!(decode_all(&bytes), Err(Error::UnexpectedEof)));
        assert!(decode(&bytes).is_ok(), "the first image is intact");
        assert_eq!(
            decode_all_with(&bytes, &DecodeOptions::default().with_lenient(true))
                .unwrap()
                .len(),
            1
        );
        assert!(matches!(
            decode_with(
                &bytes,
                &DecodeOptions::default()
                    .with_lenient(true)
                    .with_strict(true)
            ),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn encode_animation_round_trips_frames() {
        let a = GifImage::from_rgba8(2, 1, vec![255, 0, 0, 255, 0, 0, 0, 0]).unwrap();
        let b = GifImage::from_rgba8(2, 1, vec![0, 255, 0, 255, 0, 0, 255, 255]).unwrap();
        let frames = vec![
            Frame::new(a, Some(Duration::from_millis(100)))
                .with_disposal(DisposalMethod::RestoreBackground),
            Frame::new(b, Some(Duration::from_millis(250))),
        ];
        let opts = EncodeOptions::default().with_loop_count(3u16);
        let bytes = encode_animation(&frames, &opts).unwrap();
        let i = info(&bytes).unwrap();
        assert_eq!(i.frames, 2);
        assert_eq!(i.loop_count, Some(3));
        let back = decode_all(&bytes).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].delay, Some(Duration::from_millis(100)));
        assert_eq!(back[0].disposal, DisposalMethod::RestoreBackground);
        assert_eq!(back[1].delay, Some(Duration::from_millis(250)));
        assert_eq!(back[0].image.as_bytes().unwrap()[..4], [255, 0, 0, 255]);
        assert_eq!(back[0].image.as_bytes().unwrap()[7], 0);
        // Frame 1 restored the background (transparent, no in-range
        // background colour) before frame 2 painted over everything.
        assert_eq!(
            back[1].image.as_bytes().unwrap(),
            &[0, 255, 0, 255, 0, 0, 255, 255]
        );
        assert!(matches!(
            encode_animation(&[], &opts),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn oversize_and_short_inputs_are_refused() {
        assert!(matches!(
            encode_rgb8(2, 2, &[0; 11], &EncodeOptions::default()),
            Err(Error::InvalidInput(_))
        ));
        let img =
            GifImage::from_indexed(1, 1, vec![0], Palette::new(vec![[0, 0, 0, 255]])).unwrap();
        let mut too_wide = img.clone();
        too_wide.width = 70_000;
        assert!(matches!(
            encode(&too_wide, &EncodeOptions::default()),
            Err(Error::Unsupported(_))
        ));
        let mut no_pal = img;
        no_pal.palette = None;
        assert!(matches!(
            encode(&no_pal, &EncodeOptions::default()),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(decode(b"GIF89a"), Err(Error::UnexpectedEof)));
        assert!(matches!(info(b"PNG"), Err(Error::InvalidData(_))));
    }

    #[test]
    fn decode_from_and_encode_to_stream() {
        let bytes = encode_file(&single((2, 1), (0, 0, 2, 1), None)).unwrap();
        let img = decode_from(&bytes[..]).unwrap();
        let mut out = Vec::new();
        encode_to(&img, &EncodeOptions::default(), &mut out).unwrap();
        assert_eq!(decode(&out).unwrap(), img);
    }
}
