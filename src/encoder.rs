//! GIF87a / GIF89a top-level encoder.

use crate::error::{Error, Result};
use crate::image::{Application, Block, Frame, GifImage, GraphicControl, PlainText, Rgb, Version};
use crate::interlace::interlace_row_order;
use crate::lzw;

/// The Appendix-F table-full strategy the encoder uses when an image's
/// LZW dictionary fills its 4096-entry ceiling.
///
/// Both strategies are permitted by Appendix F's cover sheet and decode
/// to identical pixels — [`crate::lzw::decode`] honours a mid-stream
/// Clear (§F.1) regardless. They emit *byte-identical* compressed output
/// for any frame whose raster never fills the table; the choice only
/// affects frames large and varied enough to reach 4096 entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LzwStrategy {
    /// Freeze the full dictionary and keep emitting 12-bit codes against
    /// it until end-of-image (the §F cover-sheet *deferred clear* rule).
    /// This is the historical default and keeps [`encode`]'s output
    /// byte-stable.
    #[default]
    DeferredClear,
    /// Emit a Clear code and rebuild the dictionary the instant the table
    /// fills, so it re-adapts to later content instead of coding it
    /// against a frozen prefix set. Typically produces a smaller stream
    /// on a large raster whose later content differs from its early
    /// content.
    ClearOnFull,
}

/// Encoder tuning knobs. Construct with [`EncodeOptions::default`] (which
/// reproduces [`encode`] exactly) and override individual fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EncodeOptions {
    /// Which Appendix-F table-full strategy to apply to every image
    /// frame's LZW stream. Defaults to [`LzwStrategy::DeferredClear`].
    pub lzw_strategy: LzwStrategy,
}

/// Serialise a [`GifImage`] into a byte stream conforming to the
/// `<GIF Data Stream>` grammar in Appendix B.
///
/// Uses the default [`EncodeOptions`] (deferred-clear LZW). Call
/// [`encode_with_options`] to select a different Appendix-F table-full
/// strategy.
pub fn encode(image: &GifImage) -> Result<Vec<u8>> {
    encode_with_options(image, EncodeOptions::default())
}

/// Serialise a [`GifImage`] like [`encode`], but with caller-chosen
/// [`EncodeOptions`].
///
/// The output is byte-identical to [`encode`] whenever
/// `options == EncodeOptions::default()` *and* whenever no image frame's
/// LZW dictionary reaches its 4096-entry ceiling — the
/// [`LzwStrategy`] choice only changes the bytes for table-filling
/// frames. Every produced stream decodes to the same pixels.
pub fn encode_with_options(image: &GifImage, options: EncodeOptions) -> Result<Vec<u8>> {
    validate(image)?;

    let mut out = Vec::new();
    write_header(&mut out, image.version);
    write_logical_screen_descriptor(&mut out, image)?;
    if let Some(palette) = &image.global_palette {
        write_color_table(&mut out, palette)?;
    }

    for block in &image.blocks {
        match block {
            Block::Image(frame) => {
                if let Some(gce) = &frame.graphic_control {
                    write_graphic_control_extension(&mut out, gce);
                }
                write_image_block(
                    &mut out,
                    frame,
                    image.global_palette.as_deref(),
                    options.lzw_strategy,
                )?;
            }
            Block::PlainText {
                params,
                graphic_control,
            } => {
                if let Some(gce) = graphic_control {
                    write_graphic_control_extension(&mut out, gce);
                }
                write_plain_text_extension(&mut out, params)?;
            }
            Block::Comment(data) => {
                write_comment_extension(&mut out, data)?;
            }
            Block::Application(app) => {
                write_application_extension(&mut out, app)?;
            }
        }
    }

    // §27 — Trailer.
    out.push(0x3B);
    Ok(out)
}

// ---------------------------------------------------------------------
// Validation.
// ---------------------------------------------------------------------

fn validate(image: &GifImage) -> Result<()> {
    if image.color_resolution > 7 {
        return Err(Error::InvalidInput(format!(
            "color_resolution {} exceeds 3-bit field range",
            image.color_resolution
        )));
    }
    if let Some(palette) = &image.global_palette {
        validate_palette(palette, "global")?;
    }
    // §7 ("Version Numbers") — an encoder is required to label the
    // stream with the earliest version that covers every block. If the
    // caller declared `Version::Gif87a` but their block list contains an
    // 89a-only block (any extension, or an image with an attached §23
    // GCE), we cannot honour the request: the resulting stream would be
    // structurally invalid (a `GIF87a` header followed by a 89a-only
    // block payload). Refuse with `InvalidInput` so the caller fixes the
    // mismatch — either by calling [`crate::GifImage::upgrade_version_if_needed`]
    // first or by removing the offending block.
    let required = image.required_version();
    if required > image.version {
        let offender = image
            .blocks
            .iter()
            .find(|b| b.required_version() > image.version)
            .map(block_label)
            .unwrap_or("unknown");
        return Err(Error::InvalidInput(format!(
            "stream declared as GIF87a but contains a {offender} block (Required Version: GIF89a). \
             Call GifImage::upgrade_version_if_needed() or remove the block."
        )));
    }
    for block in &image.blocks {
        if let Block::Image(frame) = block {
            if let Some(palette) = &frame.local_palette {
                validate_palette(palette, "local")?;
            }
            let pixels_expected = (frame.width as usize)
                .checked_mul(frame.height as usize)
                .ok_or_else(|| Error::InvalidInput("frame width × height overflows".into()))?;
            if frame.indices.len() != pixels_expected {
                return Err(Error::InvalidInput(format!(
                    "frame indices length {} != width*height {}",
                    frame.indices.len(),
                    pixels_expected
                )));
            }
        }
    }
    Ok(())
}

/// Short label for an offending block kind, used in the §7 version
/// mismatch error message.
fn block_label(block: &Block) -> &'static str {
    match block {
        Block::Image(_) => "Image (with Graphic Control Extension)",
        Block::PlainText { .. } => "Plain Text Extension",
        Block::Comment(_) => "Comment Extension",
        Block::Application(_) => "Application Extension",
    }
}

fn validate_palette(palette: &[Rgb], scope: &str) -> Result<()> {
    if palette.is_empty() {
        return Err(Error::InvalidInput(format!(
            "{scope} colour table must contain at least one entry"
        )));
    }
    if palette.len() > 256 {
        return Err(Error::InvalidInput(format!(
            "{scope} colour table has {} entries; max is 256",
            palette.len()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Block writers.
// ---------------------------------------------------------------------

fn write_header(out: &mut Vec<u8>, version: Version) {
    out.extend_from_slice(b"GIF");
    out.extend_from_slice(&version.ascii());
}

fn write_logical_screen_descriptor(out: &mut Vec<u8>, image: &GifImage) -> Result<()> {
    write_u16_le(out, image.screen_width);
    write_u16_le(out, image.screen_height);

    let mut packed: u8 = 0;
    if let Some(palette) = &image.global_palette {
        packed |= 0b1000_0000;
        let bits = size_bits_for_palette(palette.len())?;
        packed |= bits;
    }
    packed |= (image.color_resolution & 0b0000_0111) << 4;
    if image.global_palette_sorted {
        packed |= 0b0000_1000;
    }
    out.push(packed);

    out.push(image.background_index);
    out.push(image.pixel_aspect_ratio);
    Ok(())
}

fn write_color_table(out: &mut Vec<u8>, palette: &[Rgb]) -> Result<()> {
    let bits = size_bits_for_palette(palette.len())?;
    let on_disk_count = 1usize << (bits as u32 + 1);
    for entry in palette {
        out.push(entry.r);
        out.push(entry.g);
        out.push(entry.b);
    }
    // §19 / §21: the on-disk size is the next power of two ≥ entry
    // count (range 2..=256). Pad unused slots with black.
    for _ in palette.len()..on_disk_count {
        out.push(0);
        out.push(0);
        out.push(0);
    }
    Ok(())
}

fn write_image_block(
    out: &mut Vec<u8>,
    frame: &Frame,
    global_palette: Option<&[Rgb]>,
    lzw_strategy: LzwStrategy,
) -> Result<()> {
    out.push(0x2C); // Image Separator (§20.c.i)
    write_u16_le(out, frame.left);
    write_u16_le(out, frame.top);
    write_u16_le(out, frame.width);
    write_u16_le(out, frame.height);

    let mut packed: u8 = 0;
    if let Some(palette) = &frame.local_palette {
        packed |= 0b1000_0000;
        let bits = size_bits_for_palette(palette.len())?;
        packed |= bits;
    }
    if frame.interlaced {
        packed |= 0b0100_0000;
    }
    if frame.palette_sorted {
        packed |= 0b0010_0000;
    }
    out.push(packed);

    if let Some(palette) = &frame.local_palette {
        write_color_table(out, palette)?;
    }

    // §22.c.i — LZW Minimum Code Size derived from active palette
    // size, with a floor of 2 ("ESTABLISH CODE SIZE" black-and-white
    // clause).
    let active_palette_len = frame
        .local_palette
        .as_deref()
        .or(global_palette)
        .map(|p| p.len())
        .ok_or_else(|| {
            Error::InvalidInput(
                "frame has no local palette and no global palette is present".into(),
            )
        })?;
    let raw_bits = bits_required_for(active_palette_len);
    let min_code_size = raw_bits.max(2);

    // Validate every index fits in the active palette.
    for &idx in &frame.indices {
        if (idx as usize) >= active_palette_len {
            return Err(Error::InvalidInput(format!(
                "pixel index {idx} >= active palette size {active_palette_len}"
            )));
        }
    }

    // Re-shuffle into interlaced storage order if requested (Appendix E).
    let pixels_for_lzw: Vec<u8> = if frame.interlaced {
        let row_order = interlace_row_order(frame.height);
        let row_bytes = frame.width as usize;
        let mut buf = Vec::with_capacity(frame.indices.len());
        for &target_row in &row_order {
            let dst_off = (target_row as usize) * row_bytes;
            buf.extend_from_slice(&frame.indices[dst_off..dst_off + row_bytes]);
        }
        buf
    } else {
        frame.indices.clone()
    };

    let compressed = match lzw_strategy {
        LzwStrategy::DeferredClear => lzw::encode(min_code_size, &pixels_for_lzw)?,
        LzwStrategy::ClearOnFull => lzw::encode_with_clear_on_full(min_code_size, &pixels_for_lzw)?,
    };
    out.push(min_code_size);
    write_data_sub_blocks(out, &compressed);
    Ok(())
}

fn write_graphic_control_extension(out: &mut Vec<u8>, gce: &GraphicControl) {
    out.push(0x21); // Extension Introducer
    out.push(0xF9); // Graphic Control Label
    out.push(4); // Block Size — fixed 4 (§23.c.iii)
    let mut packed: u8 = 0;
    packed |= (gce.disposal.as_bits() & 0b0000_0111) << 2;
    if gce.user_input {
        packed |= 0b0000_0010;
    }
    if gce.transparent_index.is_some() {
        packed |= 0b0000_0001;
    }
    out.push(packed);
    write_u16_le(out, gce.delay_centis);
    out.push(gce.transparent_index.unwrap_or(0));
    out.push(0); // Block Terminator (§23.c.ix)
}

fn write_plain_text_extension(out: &mut Vec<u8>, params: &PlainText) -> Result<()> {
    out.push(0x21);
    out.push(0x01);
    out.push(12); // Block Size — fixed 12 (§25.c.iii)
    write_u16_le(out, params.left);
    write_u16_le(out, params.top);
    write_u16_le(out, params.width);
    write_u16_le(out, params.height);
    out.push(params.cell_width);
    out.push(params.cell_height);
    out.push(params.fg_color_index);
    out.push(params.bg_color_index);
    write_data_sub_blocks(out, &params.text);
    Ok(())
}

fn write_comment_extension(out: &mut Vec<u8>, data: &[u8]) -> Result<()> {
    out.push(0x21);
    out.push(0xFE);
    write_data_sub_blocks(out, data);
    Ok(())
}

fn write_application_extension(out: &mut Vec<u8>, app: &Application) -> Result<()> {
    out.push(0x21);
    out.push(0xFF);
    out.push(11); // Block Size — fixed 11 (§26.c.iii)
    out.extend_from_slice(&app.identifier);
    out.extend_from_slice(&app.auth_code);
    write_data_sub_blocks(out, &app.data);
    Ok(())
}

// ---------------------------------------------------------------------
// Sub-block packaging — §15 (Data Sub-blocks) and §16 (Block
// Terminator). A zero-length sub-block ends the sequence even when no
// data was written, so an empty payload still emits a single 0x00.
// ---------------------------------------------------------------------

fn write_data_sub_blocks(out: &mut Vec<u8>, data: &[u8]) {
    let mut i = 0;
    while i < data.len() {
        let chunk_len = (data.len() - i).min(255);
        out.push(chunk_len as u8);
        out.extend_from_slice(&data[i..i + chunk_len]);
        i += chunk_len;
    }
    out.push(0); // Block Terminator
}

// ---------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------

fn write_u16_le(out: &mut Vec<u8>, v: u16) {
    out.push((v & 0xFF) as u8);
    out.push(((v >> 8) & 0xFF) as u8);
}

/// Map an entry count (1..=256) to the 3-bit "Size" field used by
/// §18.c.vi and §20.c.ix: actual entries = `2^(field + 1)`.
fn size_bits_for_palette(n: usize) -> Result<u8> {
    if n == 0 || n > 256 {
        return Err(Error::InvalidInput(format!(
            "palette size {n} outside 1..=256"
        )));
    }
    // Smallest k in 0..=7 with 2^(k+1) >= n.
    for k in 0u8..=7 {
        if (1usize << (k as u32 + 1)) >= n {
            return Ok(k);
        }
    }
    unreachable!()
}

/// Bits needed to address `n` distinct values. 0 → 0 bits; 1 → 0 bits;
/// 2 → 1 bit; 3..=4 → 2 bits; ...
fn bits_required_for(n: usize) -> u8 {
    if n <= 1 {
        0
    } else {
        // Minimum k such that 2^k >= n.
        let mut k = 0u8;
        while (1usize << k as u32) < n {
            k += 1;
        }
        k
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{Application, DisposalMethod, Frame as GifFrame, GraphicControl, PlainText};

    fn one_pixel_image(version: Version, blocks: Vec<Block>) -> GifImage {
        GifImage {
            version,
            screen_width: 1,
            screen_height: 1,
            color_resolution: 1,
            global_palette_sorted: false,
            background_index: 0,
            pixel_aspect_ratio: 0,
            global_palette: Some(vec![Rgb::new(0, 0, 0), Rgb::new(0xFF, 0xFF, 0xFF)]),
            blocks,
        }
    }

    fn one_frame_no_gce() -> Block {
        Block::Image(GifFrame {
            left: 0,
            top: 0,
            width: 1,
            height: 1,
            local_palette: None,
            palette_sorted: false,
            interlaced: false,
            indices: vec![0],
            graphic_control: None,
        })
    }

    fn one_frame_with_gce() -> Block {
        Block::Image(GifFrame {
            left: 0,
            top: 0,
            width: 1,
            height: 1,
            local_palette: None,
            palette_sorted: false,
            interlaced: false,
            indices: vec![0],
            graphic_control: Some(GraphicControl {
                disposal: DisposalMethod::Keep,
                user_input: false,
                transparent_index: None,
                delay_centis: 10,
            }),
        })
    }

    /// §7 — pure 87a-compatible content (image, no extensions) encodes
    /// under `Gif87a` without complaint.
    #[test]
    fn encode_pure_image_under_gif87a_works() {
        let img = one_pixel_image(Version::Gif87a, vec![one_frame_no_gce()]);
        let out = encode(&img).unwrap();
        // Header must be `GIF87a`.
        assert_eq!(&out[..6], b"GIF87a");
    }

    /// §23 GCE requires 89a — encoding under `Gif87a` must be rejected
    /// because the resulting stream would be malformed (a `GIF87a`
    /// header preceding a 89a-only block).
    #[test]
    fn encode_gce_under_gif87a_rejected() {
        let img = one_pixel_image(Version::Gif87a, vec![one_frame_with_gce()]);
        let err = encode(&img).unwrap_err();
        match err {
            Error::InvalidInput(s) => {
                assert!(s.contains("GIF87a"), "{s}");
                assert!(s.contains("GIF89a"), "{s}");
                assert!(s.contains("Image"), "{s}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// §24 Comment Extension requires 89a.
    #[test]
    fn encode_comment_under_gif87a_rejected() {
        let img = one_pixel_image(
            Version::Gif87a,
            vec![Block::Comment(b"hi".to_vec()), one_frame_no_gce()],
        );
        let err = encode(&img).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(s) if s.contains("Comment")));
    }

    /// §25 Plain Text Extension requires 89a.
    #[test]
    fn encode_plain_text_under_gif87a_rejected() {
        let pt = PlainText {
            left: 0,
            top: 0,
            width: 1,
            height: 1,
            cell_width: 8,
            cell_height: 8,
            fg_color_index: 1,
            bg_color_index: 0,
            text: b"X".to_vec(),
        };
        let img = one_pixel_image(
            Version::Gif87a,
            vec![
                Block::PlainText {
                    params: pt,
                    graphic_control: None,
                },
                one_frame_no_gce(),
            ],
        );
        let err = encode(&img).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(s) if s.contains("Plain Text")));
    }

    /// §26 Application Extension requires 89a.
    #[test]
    fn encode_application_under_gif87a_rejected() {
        let app = Application {
            identifier: *b"NETSCAPE",
            auth_code: *b"2.0",
            data: vec![0x01, 0x00, 0x00],
        };
        let img = one_pixel_image(
            Version::Gif87a,
            vec![Block::Application(app), one_frame_no_gce()],
        );
        let err = encode(&img).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(s) if s.contains("Application")));
    }

    /// `upgrade_version_if_needed()` followed by `encode()` succeeds on
    /// what would otherwise be a §7 mismatch — this is the documented
    /// recovery path.
    #[test]
    fn upgrade_then_encode_passes_with_gif89a_header() {
        let mut img = one_pixel_image(Version::Gif87a, vec![one_frame_with_gce()]);
        assert!(img.upgrade_version_if_needed(), "expected a version bump");
        let out = encode(&img).unwrap();
        assert_eq!(&out[..6], b"GIF89a");
    }

    /// `Gif89a` is preserved when the blocks would only require 87a —
    /// `upgrade_version_if_needed` must NOT *downgrade*. An explicit
    /// `Gif89a` declaration is a caller choice we honour.
    #[test]
    fn upgrade_does_not_downgrade() {
        let mut img = one_pixel_image(Version::Gif89a, vec![one_frame_no_gce()]);
        assert!(
            !img.upgrade_version_if_needed(),
            "expected no version change"
        );
        assert_eq!(img.version, Version::Gif89a);
        let out = encode(&img).unwrap();
        assert_eq!(&out[..6], b"GIF89a");
    }

    #[test]
    fn size_bits_table() {
        // 2^(k+1) entries → k.
        assert_eq!(size_bits_for_palette(2).unwrap(), 0);
        assert_eq!(size_bits_for_palette(3).unwrap(), 1);
        assert_eq!(size_bits_for_palette(4).unwrap(), 1);
        assert_eq!(size_bits_for_palette(5).unwrap(), 2);
        assert_eq!(size_bits_for_palette(8).unwrap(), 2);
        assert_eq!(size_bits_for_palette(16).unwrap(), 3);
        assert_eq!(size_bits_for_palette(256).unwrap(), 7);
        assert!(size_bits_for_palette(0).is_err());
        assert!(size_bits_for_palette(257).is_err());
    }

    #[test]
    fn bits_required_table() {
        assert_eq!(bits_required_for(0), 0);
        assert_eq!(bits_required_for(1), 0);
        assert_eq!(bits_required_for(2), 1);
        assert_eq!(bits_required_for(3), 2);
        assert_eq!(bits_required_for(4), 2);
        assert_eq!(bits_required_for(5), 3);
        assert_eq!(bits_required_for(16), 4);
        assert_eq!(bits_required_for(256), 8);
    }

    // -----------------------------------------------------------------
    // LzwStrategy selection (encode_with_options).
    // -----------------------------------------------------------------

    /// A 256-colour image whose raster changes "regime" partway through:
    /// the first half repeats a small set of indices (long LZW runs that
    /// fill the 4096-entry table quickly), the second half is a different
    /// pseudo-random byte sequence the early dictionary cannot code well.
    /// Large enough (192×192 = 36 864 px) to fill the dictionary, so the
    /// deferred-clear vs clear-on-full split is exercised.
    fn regime_change_image() -> GifImage {
        let palette: Vec<Rgb> = (0..256u32)
            .map(|i| Rgb::new(i as u8, (i ^ 0x5A) as u8, (i.wrapping_mul(7)) as u8))
            .collect();
        let w: u16 = 192;
        let h: u16 = 192;
        let total = w as usize * h as usize;
        let mut indices = Vec::with_capacity(total);
        let half = total / 2;
        // First regime: low-entropy ramp over a small alphabet.
        for i in 0..half {
            indices.push((i % 7) as u8);
        }
        // Second regime: a wholly different pseudo-random stream.
        let mut state: u32 = 0x1234_5678;
        for _ in half..total {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            indices.push((state >> 16) as u8);
        }
        GifImage {
            version: Version::Gif89a,
            screen_width: w,
            screen_height: h,
            color_resolution: 7,
            global_palette_sorted: false,
            background_index: 0,
            pixel_aspect_ratio: 0,
            global_palette: Some(palette),
            blocks: vec![Block::Image(GifFrame {
                left: 0,
                top: 0,
                width: w,
                height: h,
                local_palette: None,
                palette_sorted: false,
                interlaced: false,
                indices,
                graphic_control: None,
            })],
        }
    }

    /// `encode_with_options(EncodeOptions::default())` is byte-identical
    /// to `encode` — the default knobs must not change the output.
    #[test]
    fn encode_with_default_options_matches_encode() {
        let img = regime_change_image();
        let baseline = encode(&img).unwrap();
        let via_options = encode_with_options(&img, EncodeOptions::default()).unwrap();
        assert_eq!(baseline, via_options);
    }

    /// Both strategies decode to the same `GifImage` (identical pixels).
    /// The choice is purely a compressed-size trade-off; pixel output is
    /// invariant because `lzw::decode` honours a mid-stream Clear.
    #[test]
    fn both_strategies_decode_identically() {
        let img = regime_change_image();
        let deferred = encode_with_options(
            &img,
            EncodeOptions {
                lzw_strategy: LzwStrategy::DeferredClear,
            },
        )
        .unwrap();
        let clear_on_full = encode_with_options(
            &img,
            EncodeOptions {
                lzw_strategy: LzwStrategy::ClearOnFull,
            },
        )
        .unwrap();

        let a = crate::decoder::decode(&deferred).unwrap();
        let b = crate::decoder::decode(&clear_on_full).unwrap();
        // Same decoded structure: one frame, identical index payloads.
        let fa: Vec<_> = a.frames().map(|f| f.indices.clone()).collect();
        let fb: Vec<_> = b.frames().map(|f| f.indices.clone()).collect();
        assert_eq!(fa, fb);
        // And those indices match the source raster.
        let src: Vec<_> = img.frames().map(|f| f.indices.clone()).collect();
        assert_eq!(fa, src);
    }

    /// On a large regime-changing raster, the clear-on-full strategy
    /// re-adapts its dictionary and produces a strictly smaller stream
    /// than the frozen-table deferred-clear default.
    #[test]
    fn clear_on_full_is_smaller_on_regime_change() {
        let img = regime_change_image();
        let deferred = encode_with_options(
            &img,
            EncodeOptions {
                lzw_strategy: LzwStrategy::DeferredClear,
            },
        )
        .unwrap();
        let clear_on_full = encode_with_options(
            &img,
            EncodeOptions {
                lzw_strategy: LzwStrategy::ClearOnFull,
            },
        )
        .unwrap();
        assert!(
            clear_on_full.len() < deferred.len(),
            "clear-on-full ({}) should beat deferred-clear ({}) on a \
             regime-changing raster",
            clear_on_full.len(),
            deferred.len()
        );
    }

    /// A small raster that never fills the LZW table emits byte-identical
    /// output under both strategies (the cover-sheet guarantee: the
    /// strategies only diverge once the dictionary reaches 4096 entries).
    #[test]
    fn strategies_agree_below_table_full() {
        let img = one_pixel_image(Version::Gif89a, vec![one_frame_no_gce()]);
        let deferred = encode_with_options(
            &img,
            EncodeOptions {
                lzw_strategy: LzwStrategy::DeferredClear,
            },
        )
        .unwrap();
        let clear_on_full = encode_with_options(
            &img,
            EncodeOptions {
                lzw_strategy: LzwStrategy::ClearOnFull,
            },
        )
        .unwrap();
        assert_eq!(deferred, clear_on_full);
    }
}
