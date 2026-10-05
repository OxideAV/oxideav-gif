//! GIF87a / GIF89a top-level decoder.
//!
//! The implementation walks the byte stream against the grammar in
//! Appendix B and produces the in-memory tree defined in
//! [`crate::image`].

use crate::error::{Error, Result};
use crate::image::{
    Application, Block, DisposalMethod, GifFile, GifFrameData, GraphicControl, PlainText, Rgb,
    Version,
};
use crate::interlace::interlace_row_order;
use crate::lzw;
use crate::options::DecodeOptions;

/// Byte values referenced by the spec.
mod label {
    /// §17 Header byte 0..=2 — `b"GIF"`.
    pub const SIGNATURE: &[u8; 3] = b"GIF";
    /// §20.c.i — Image Descriptor introducer.
    pub const IMAGE_SEPARATOR: u8 = 0x2C;
    /// §27 — Trailer byte.
    pub const TRAILER: u8 = 0x3B;
    /// §23.c.i (and friends) — extension introducer.
    pub const EXTENSION_INTRODUCER: u8 = 0x21;
    /// §23.c.ii — Graphic Control label.
    pub const GRAPHIC_CONTROL: u8 = 0xF9;
    /// §24.c.ii — Comment label.
    pub const COMMENT: u8 = 0xFE;
    /// §25.c.ii — Plain Text label.
    pub const PLAIN_TEXT: u8 = 0x01;
    /// §26.c.ii — Application label.
    pub const APPLICATION: u8 = 0xFF;
}

/// Read a GIF Data Stream from `bytes` and return only the first
/// image-bearing block, packaged as a [`GifFile`] containing exactly
/// one [`Block::Image`].
///
/// Walks the §B grammar header → Logical Screen Descriptor → §15
/// data sub-blocks the same way [`crate::parse`] does, but short-circuits
/// the moment the first §20 Image Descriptor finishes. Trailing
/// §24 Comment / §25 Plain Text / §26 Application Extension blocks
/// (and the §27 Trailer) are intentionally not consumed — the caller
/// gets the cover frame and stops paying for parser work it does not
/// need.
///
/// Returns [`Error::InvalidData`] when the stream contains no image
/// block at all (only extensions, then the trailer).
///
/// Use [`parse`] when you need the full block list (animation
/// frames, comments, application extensions).
///
/// # Why this is faster than `parse().frames().next()`
///
/// `parse()` allocates a [`Vec<Block>`] sized to every block in the
/// stream, walks every comment / application block past the first
/// image, and re-runs the LZW decoder on every animation frame even
/// when the consumer only wants the first one. `parse_first_frame`
/// stops at the first image and never allocates the trailing block
/// list.
pub fn parse_first_frame(bytes: &[u8]) -> Result<GifFile> {
    parse_first_frame_with(bytes, &DecodeOptions::default())
}

/// [`parse_first_frame`] under explicit [`DecodeOptions`] limits. The
/// `strict` / `lenient` flags are ignored here: the fast path is
/// always the structural (non-recovering) parser, and the conformance
/// walk needs the whole block list.
pub fn parse_first_frame_with(bytes: &[u8], opts: &DecodeOptions) -> Result<GifFile> {
    let mut p = Parser::new(bytes, opts);
    let version = p.read_header()?;
    let (screen_width, screen_height, packed, background_index, pixel_aspect_ratio) =
        p.read_logical_screen_descriptor()?;
    let global_table_flag = (packed & 0b1000_0000) != 0;
    let color_resolution = (packed >> 4) & 0b0000_0111;
    let global_palette_sorted = (packed & 0b0000_1000) != 0;
    let global_table_size_bits = packed & 0b0000_0111;
    let global_palette = if global_table_flag {
        Some(p.read_color_table(global_table_size_bits)?)
    } else {
        None
    };

    let mut pending_gce: Option<GraphicControl> = None;
    loop {
        let intro = p.peek_byte()?;
        match intro {
            label::TRAILER => {
                return Err(Error::InvalidData(
                    "parse_first_frame: stream contains no image block".into(),
                ));
            }
            label::IMAGE_SEPARATOR => {
                let frame = p.read_image_descriptor_and_data(pending_gce.take())?;
                return Ok(GifFile {
                    version,
                    screen_width,
                    screen_height,
                    color_resolution,
                    global_palette_sorted,
                    background_index,
                    pixel_aspect_ratio,
                    global_palette,
                    blocks: vec![Block::Image(frame)],
                });
            }
            label::EXTENSION_INTRODUCER => {
                p.advance(1);
                let label = p.read_byte()?;
                match label {
                    label::GRAPHIC_CONTROL => {
                        // Honour §23.d: the GCE attaches to the next
                        // graphic-rendering block, which on the
                        // fast-path is the image we are about to
                        // accept.
                        pending_gce = Some(p.read_graphic_control_extension()?);
                    }
                    label::COMMENT => {
                        // §24.c.iv — payload bytes follow as a
                        // sub-block sequence. Skip them rather than
                        // materialise; the fast-path discards every
                        // non-image block.
                        let _ = p.read_data_sub_blocks()?;
                    }
                    label::PLAIN_TEXT => {
                        // §25 — fixed 12-byte parameter block followed
                        // by sub-blocks.
                        let _ = p.read_plain_text_extension()?;
                    }
                    label::APPLICATION => {
                        // §26 — fixed 11-byte header block followed by
                        // sub-blocks.
                        let _ = p.read_application_extension()?;
                    }
                    other => {
                        return Err(Error::Unsupported(format!(
                            "unknown extension label 0x{other:02X}"
                        )));
                    }
                }
            }
            other => {
                return Err(Error::InvalidData(format!(
                    "expected block introducer, got byte 0x{other:02X}"
                )));
            }
        }
    }
}

/// Read a GIF Data Stream from `bytes` and return it as a [`GifFile`]
/// — every block, every colour table, every §22 index raster — under
/// [`DecodeOptions::default`] (structural parse, 1 GiB byte limit).
///
/// This is the depth entry point; the image-crate contract's
/// [`crate::parse`] composes the first frame of this model onto the
/// Logical Screen.
pub fn parse(bytes: &[u8]) -> Result<GifFile> {
    parse_with(bytes, &DecodeOptions::default())
}

/// [`parse`] under explicit [`DecodeOptions`]: limits are enforced
/// before every allocation, `lenient` selects the recovery parser of
/// [`parse_lenient`], and `strict` runs
/// [`GifFile::validate_strict`] on the parsed stream.
pub fn parse_with(bytes: &[u8], opts: &DecodeOptions) -> Result<GifFile> {
    if opts.strict && opts.lenient {
        return Err(Error::invalid_input(
            "DecodeOptions: strict and lenient are mutually exclusive",
        ));
    }
    let mode = if opts.lenient {
        RecoveryMode::Lenient
    } else {
        RecoveryMode::Strict
    };
    let file = parse_mode(bytes, mode, opts)?;
    if opts.strict {
        // The conformance walk reports encoder-side `InvalidInput`;
        // on the decode path the same departures are `InvalidData`.
        file.validate_strict().map_err(|e| match e {
            Error::InvalidInput(msg) => Error::InvalidData(msg),
            other => other,
        })?;
    }
    Ok(file)
}

/// Read a GIF Data Stream in *lenient* mode — when the parser hits a
/// malformed block past the §17 header / §18 Logical Screen Descriptor
/// / §19 Global Color Table prefix, it skips ahead to the next §20
/// Image Separator or §27 Trailer instead of returning an error.
///
/// Recovered blocks are appended to the resulting [`GifFile`] in
/// source order; corrupted bytes are simply dropped. The header, LSD,
/// and GCT are still required to parse cleanly — a truncated header
/// has no recoverable image data behind it, so the function still
/// returns an [`Error`] in that case.
///
/// # Why this exists
///
/// Real-world streams can be truncated by network resets, corrupted
/// by transcoders that miscount sub-block lengths, or stitched
/// together by tools that produce malformed application extensions.
/// Strict mode (the [`crate::parse`] entry point) refuses these inputs;
/// lenient mode lets a viewer recover whatever image-bearing blocks
/// are still readable behind a broken extension or a malformed prior
/// frame.
///
/// # What is NOT recovered
///
/// * A corrupt §22 LZW payload inside an image block — the spec
///   doesn't define a way to resynchronise mid-LZW. The malformed
///   image is dropped; the parser then resumes at the next §20 / §27
///   introducer it finds.
/// * Header / LSD / GCT corruption — these prefix every block, so
///   without them the stream is effectively unparseable.
///
/// # Why a separate entry point
///
/// Production decoders should default to strict ([`parse`]) so a
/// corrupted stream doesn't silently round-trip to a *different*
/// stream on re-encode. Lenient mode is opt-in for consumers (viewers,
/// thumbnailers, recovery tools) that prefer "show what we can" over
/// "all or nothing". Equivalent to [`parse_with`] with
/// [`DecodeOptions::with_lenient`]`(true)`.
pub fn parse_lenient(bytes: &[u8]) -> Result<GifFile> {
    parse_with(bytes, &DecodeOptions::default().with_lenient(true))
}

// ---- Pre-contract names, kept for one release -------------------------

/// The pre-contract name of [`parse_first_frame`].
#[deprecated(note = "use oxideav_gif::parse_first_frame (IMAGE_CRATE_API)")]
pub fn decode_first_frame(bytes: &[u8]) -> Result<GifFile> {
    parse_first_frame(bytes)
}

/// The pre-contract name of [`parse_lenient`].
#[deprecated(note = "use oxideav_gif::parse_lenient (IMAGE_CRATE_API)")]
pub fn decode_lenient(bytes: &[u8]) -> Result<GifFile> {
    parse_lenient(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryMode {
    Strict,
    Lenient,
}

fn parse_mode(bytes: &[u8], mode: RecoveryMode, opts: &DecodeOptions) -> Result<GifFile> {
    let mut p = Parser::new(bytes, opts);
    let version = p.read_header()?;
    let (screen_width, screen_height, packed, background_index, pixel_aspect_ratio) =
        p.read_logical_screen_descriptor()?;
    // §18.c "<Packed Fields>" — bit layout:
    //  bit 7      : Global Color Table Flag
    //  bits 6..4  : Color Resolution
    //  bit 3      : Sort Flag
    //  bits 2..0  : Size of Global Color Table
    let global_table_flag = (packed & 0b1000_0000) != 0;
    let color_resolution = (packed >> 4) & 0b0000_0111;
    let global_palette_sorted = (packed & 0b0000_1000) != 0;
    let global_table_size_bits = packed & 0b0000_0111;
    let global_palette = if global_table_flag {
        Some(p.read_color_table(global_table_size_bits)?)
    } else {
        None
    };

    let mut blocks = Vec::new();
    // The Graphic Control Extension's scope is "the first graphic
    // rendering block to follow" (§23.d). Buffer the most recent GCE
    // here and attach it to the next image / plain text we see.
    let mut pending_gce: Option<GraphicControl> = None;

    loop {
        let intro = match p.peek_byte() {
            Ok(b) => b,
            Err(e) => match mode {
                RecoveryMode::Strict => return Err(e),
                // Lenient: ran out of bytes without seeing the §27
                // Trailer. Surface whatever blocks we successfully
                // recovered so far.
                RecoveryMode::Lenient => break,
            },
        };
        match intro {
            label::TRAILER => {
                p.advance(1);
                break;
            }
            label::IMAGE_SEPARATOR => {
                match p.read_image_descriptor_and_data(pending_gce.take()) {
                    Ok(frame) => blocks.push(Block::Image(frame)),
                    // A DecodeOptions limit is a caller policy, not a
                    // malformed block: it is never recovered from.
                    Err(e @ Error::LimitExceeded(_)) => return Err(e),
                    Err(e) => match mode {
                        RecoveryMode::Strict => return Err(e),
                        // §22 LZW corruption / image-descriptor
                        // truncation. Drop the partial frame and scan
                        // forward for the next image-or-trailer
                        // introducer.
                        RecoveryMode::Lenient => p.resync_to_image_or_trailer(),
                    },
                }
            }
            label::EXTENSION_INTRODUCER => {
                p.advance(1);
                let label = match p.read_byte() {
                    Ok(b) => b,
                    Err(e) => match mode {
                        RecoveryMode::Strict => return Err(e),
                        RecoveryMode::Lenient => break,
                    },
                };
                match label {
                    label::GRAPHIC_CONTROL => match p.read_graphic_control_extension() {
                        // §23 — at most one GCE may precede a graphic
                        // rendering block. A second consecutive GCE is
                        // invalid; drop the previous one with a warning
                        // by silently overwriting (decoders must be
                        // robust against malformed streams).
                        Ok(gce) => pending_gce = Some(gce),
                        Err(e) => match mode {
                            RecoveryMode::Strict => return Err(e),
                            RecoveryMode::Lenient => {
                                // Corrupted GCE; abandon the pending
                                // attachment and resync.
                                pending_gce = None;
                                p.resync_to_image_or_trailer();
                            }
                        },
                    },
                    label::COMMENT => match p.read_data_sub_blocks() {
                        Ok(data) => blocks.push(Block::Comment(data)),
                        Err(e) => match mode {
                            RecoveryMode::Strict => return Err(e),
                            RecoveryMode::Lenient => p.resync_to_image_or_trailer(),
                        },
                    },
                    label::PLAIN_TEXT => match p.read_plain_text_extension() {
                        Ok(params) => blocks.push(Block::PlainText {
                            params,
                            graphic_control: pending_gce.take(),
                        }),
                        Err(e) => match mode {
                            RecoveryMode::Strict => return Err(e),
                            RecoveryMode::Lenient => {
                                pending_gce = None;
                                p.resync_to_image_or_trailer();
                            }
                        },
                    },
                    label::APPLICATION => match p.read_application_extension() {
                        Ok(app) => blocks.push(Block::Application(app)),
                        Err(e) => match mode {
                            RecoveryMode::Strict => return Err(e),
                            RecoveryMode::Lenient => p.resync_to_image_or_trailer(),
                        },
                    },
                    other => match mode {
                        RecoveryMode::Strict => {
                            return Err(Error::Unsupported(format!(
                                "unknown extension label 0x{other:02X}"
                            )));
                        }
                        RecoveryMode::Lenient => {
                            // Unknown extension label — every defined
                            // extension's payload sits behind a §15
                            // sub-block sequence terminated by a 0
                            // byte. Skip past the sub-blocks; if that
                            // also fails, resync to the next image
                            // separator.
                            if p.skip_data_sub_blocks().is_err() {
                                p.resync_to_image_or_trailer();
                            }
                        }
                    },
                }
            }
            other => match mode {
                RecoveryMode::Strict => {
                    return Err(Error::InvalidData(format!(
                        "expected block introducer, got byte 0x{other:02X}"
                    )));
                }
                RecoveryMode::Lenient => {
                    // Garbage at the block-introducer position. Drop
                    // the byte and keep scanning.
                    p.advance(1);
                }
            },
        }
    }

    Ok(GifFile {
        version,
        screen_width,
        screen_height,
        color_resolution,
        global_palette_sorted,
        background_index,
        pixel_aspect_ratio,
        global_palette,
        blocks,
    })
}

// ---------------------------------------------------------------------
// Header walk for the image-crate contract (`info` / `decode`).
// ---------------------------------------------------------------------

/// What [`scan`] learns from a structural walk of the stream.
// The span / comment / prefix fields are read by the framework
// container only.
#[cfg_attr(not(feature = "registry"), allow(dead_code))]
pub(crate) struct Scan {
    /// A *partial* [`GifFile`]: header / Logical Screen fields, every
    /// §26 Application Extension in source order, and the first §20
    /// image (raster expanded only when asked, otherwise `indices` is
    /// empty). Comments, Plain Text blocks and later images are not
    /// materialised. Never hand this to a caller as a `GifFile`.
    pub(crate) file: GifFile,
    /// Number of §20 image blocks seen.
    pub(crate) image_count: u32,
    /// Number of §25 Plain Text blocks seen.
    pub(crate) plain_text_count: u32,
    /// Byte length of the stream prefix every frame shares: §17 Header +
    /// §18 Logical Screen Descriptor + §19 Global Color Table.
    pub(crate) prefix_len: usize,
    /// One span per graphic-rendering block (§20 image or §25 Plain
    /// Text), in source order. The spans partition the body between
    /// `prefix_len` and the §27 Trailer: span `i` starts where span
    /// `i - 1` ended (span 0 at `prefix_len`), so every §23 Graphic
    /// Control, §24 Comment and §26 Application block travels with the
    /// graphic block that follows it; trailing special-purpose blocks
    /// ride the last span. Empty when the stream has no graphic block.
    pub(crate) spans: Vec<GraphicSpan>,
    /// Every §24 Comment Extension payload, in source order.
    pub(crate) comments: Vec<Vec<u8>>,
    /// The structural fault that ended the walk early (after the first
    /// image), when there was one. `None` for a well-formed stream.
    pub(crate) walk_error: Option<Error>,
}

/// Byte extent of one graphic-rendering block and the blocks that lead
/// it — see [`Scan::spans`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(feature = "registry"), allow(dead_code))]
pub(crate) struct GraphicSpan {
    /// First byte (inclusive) in the stream.
    pub(crate) start: usize,
    /// One past the last byte of the graphic block.
    pub(crate) end: usize,
    /// §23.c.vii Delay Time of the block's Graphic Control Extension;
    /// `None` when the block has no GCE.
    pub(crate) delay_centis: Option<u16>,
}

/// Walk the stream like [`parse`] but without expanding any LZW raster
/// except (when `expand_first`) the first image's. The §17 header, §18
/// Logical Screen Descriptor and §19 Global Color Table must parse;
/// so must every block up to and including the first image. Past that
/// point a structural fault ends the walk and the counts so far are
/// returned — the same success domain as [`parse_first_frame`].
pub(crate) fn scan(bytes: &[u8], opts: &DecodeOptions, expand_first: bool) -> Result<Scan> {
    let mut p = Parser::new(bytes, opts);
    let version = p.read_header()?;
    let (screen_width, screen_height, packed, background_index, pixel_aspect_ratio) =
        p.read_logical_screen_descriptor()?;
    let global_table_flag = (packed & 0b1000_0000) != 0;
    let color_resolution = (packed >> 4) & 0b0000_0111;
    let global_palette_sorted = (packed & 0b0000_1000) != 0;
    let global_table_size_bits = packed & 0b0000_0111;
    let global_palette = if global_table_flag {
        Some(p.read_color_table(global_table_size_bits)?)
    } else {
        None
    };

    let prefix_len = p.pos;
    let mut blocks = Vec::new();
    let mut pending_gce: Option<GraphicControl> = None;
    let mut image_count = 0u32;
    let mut plain_text_count = 0u32;
    let mut spans: Vec<GraphicSpan> = Vec::new();
    let mut comments: Vec<Vec<u8>> = Vec::new();
    let mut walk_error: Option<Error> = None;
    // Where the span of the next graphic block starts: the end of the
    // previous one (the body start for the first).
    let mut span_start = prefix_len;

    // Every structural step after the first image is "best effort": an
    // `Err` ends the walk instead of failing it (the fault is kept in
    // `walk_error` for callers that need the whole stream).
    macro_rules! step {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(e) => {
                    if image_count == 0 {
                        return Err(e);
                    }
                    walk_error = Some(e);
                    break;
                }
            }
        };
    }

    loop {
        let intro = step!(p.peek_byte());
        match intro {
            label::TRAILER => {
                // Trailing special-purpose blocks ride the last span.
                if let Some(last) = spans.last_mut() {
                    last.end = p.pos;
                }
                break;
            }
            label::IMAGE_SEPARATOR => {
                let delay_centis = pending_gce.as_ref().map(|g| g.delay_centis);
                if image_count == 0 && expand_first {
                    let frame = p.read_image_descriptor_and_data(pending_gce.take())?;
                    blocks.push(Block::Image(frame));
                } else {
                    let frame = step!(p.skip_image_descriptor_and_data(pending_gce.take()));
                    if image_count == 0 {
                        blocks.push(Block::Image(frame));
                    }
                }
                image_count += 1;
                spans.push(GraphicSpan {
                    start: span_start,
                    end: p.pos,
                    delay_centis,
                });
                span_start = p.pos;
            }
            label::EXTENSION_INTRODUCER => {
                p.advance(1);
                let label = step!(p.read_byte());
                match label {
                    label::GRAPHIC_CONTROL => {
                        pending_gce = Some(step!(p.read_graphic_control_extension()));
                    }
                    label::COMMENT => {
                        comments.push(step!(p.read_data_sub_blocks()));
                    }
                    label::PLAIN_TEXT => {
                        let _ = step!(p.read_plain_text_extension());
                        let delay_centis = pending_gce.take().map(|g| g.delay_centis);
                        plain_text_count += 1;
                        spans.push(GraphicSpan {
                            start: span_start,
                            end: p.pos,
                            delay_centis,
                        });
                        span_start = p.pos;
                    }
                    label::APPLICATION => {
                        let app = step!(p.read_application_extension());
                        blocks.push(Block::Application(app));
                    }
                    other => {
                        step!(Err(Error::Unsupported(format!(
                            "unknown extension label 0x{other:02X}"
                        ))));
                    }
                }
            }
            other => {
                step!(Err(Error::InvalidData(format!(
                    "expected block introducer, got byte 0x{other:02X}"
                ))));
            }
        }
    }

    Ok(Scan {
        file: GifFile {
            version,
            screen_width,
            screen_height,
            color_resolution,
            global_palette_sorted,
            background_index,
            pixel_aspect_ratio,
            global_palette,
            blocks,
        },
        image_count,
        plain_text_count,
        prefix_len,
        spans,
        comments,
        walk_error,
    })
}

// ---------------------------------------------------------------------
// Parser internals.
// ---------------------------------------------------------------------

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    /// Caller limits, checked before every raster allocation.
    opts: &'a DecodeOptions,
    /// Running total of expanded §22 raster bytes, for `max_bytes`.
    decoded_total: u64,
}

impl<'a> Parser<'a> {
    fn new(src: &'a [u8], opts: &'a DecodeOptions) -> Self {
        Self {
            src,
            pos: 0,
            opts,
            decoded_total: 0,
        }
    }

    fn remaining(&self) -> usize {
        self.src.len() - self.pos
    }

    fn need(&self, n: usize) -> Result<()> {
        if self.remaining() < n {
            Err(Error::UnexpectedEof)
        } else {
            Ok(())
        }
    }

    fn advance(&mut self, n: usize) {
        self.pos += n;
    }

    fn peek_byte(&self) -> Result<u8> {
        self.need(1)?;
        Ok(self.src[self.pos])
    }

    fn read_byte(&mut self) -> Result<u8> {
        self.need(1)?;
        let b = self.src[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn read_u16_le(&mut self) -> Result<u16> {
        // Appendix D: "multi-byte numeric fields are ordered with the
        // Least Significant Byte first."
        self.need(2)?;
        let lo = self.src[self.pos] as u16;
        let hi = self.src[self.pos + 1] as u16;
        self.pos += 2;
        Ok(lo | (hi << 8))
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.need(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(&self.src[self.pos..self.pos + N]);
        self.pos += N;
        Ok(out)
    }

    fn read_slice(&mut self, n: usize) -> Result<&'a [u8]> {
        self.need(n)?;
        let s = &self.src[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn read_header(&mut self) -> Result<Version> {
        // §17.c: 3 signature bytes + 3 version bytes.
        let signature: [u8; 3] = self.read_array()?;
        if &signature != label::SIGNATURE {
            return Err(Error::InvalidData(format!(
                "header signature is not 'GIF': {signature:?}"
            )));
        }
        let version_bytes: [u8; 3] = self.read_array()?;
        match &version_bytes {
            b"87a" => Ok(Version::Gif87a),
            b"89a" => Ok(Version::Gif89a),
            other => Err(Error::Unsupported(format!(
                "unsupported version field {other:?}"
            ))),
        }
    }

    fn read_logical_screen_descriptor(&mut self) -> Result<(u16, u16, u8, u8, u8)> {
        // §18.c: width, height, packed, bg_index, aspect.
        let w = self.read_u16_le()?;
        let h = self.read_u16_le()?;
        let packed = self.read_byte()?;
        let bg = self.read_byte()?;
        let aspect = self.read_byte()?;
        // DecodeOptions: the Logical Screen is the size of every image
        // this crate hands back; refuse before any block is parsed.
        self.opts
            .check_screen(u32::from(w), u32::from(h), u64::from(w) * u64::from(h))?;
        Ok((w, h, packed, bg, aspect))
    }

    fn read_color_table(&mut self, size_bits: u8) -> Result<Vec<Rgb>> {
        // §19 / §21: number of bytes = 3 * 2^(size_bits + 1).
        let count = 1usize << (size_bits as u32 + 1);
        let mut table = Vec::with_capacity(count);
        for _ in 0..count {
            let r = self.read_byte()?;
            let g = self.read_byte()?;
            let b = self.read_byte()?;
            table.push(Rgb::new(r, g, b));
        }
        Ok(table)
    }

    fn read_data_sub_blocks(&mut self) -> Result<Vec<u8>> {
        // §15: each sub-block is `len_byte | data*len`. Sequence is
        // terminated by the §16 zero-length Block Terminator.
        let mut out = Vec::new();
        loop {
            let n = self.read_byte()?;
            if n == 0 {
                return Ok(out);
            }
            let chunk = self.read_slice(n as usize)?;
            out.extend_from_slice(chunk);
        }
    }

    /// Walk a §15 sub-block sequence without buffering the payload.
    /// Used by lenient-mode unknown-extension skipping.
    fn skip_data_sub_blocks(&mut self) -> Result<()> {
        loop {
            let n = self.read_byte()?;
            if n == 0 {
                return Ok(());
            }
            self.need(n as usize)?;
            self.pos += n as usize;
        }
    }

    /// Forward-scan the byte stream for the next §20 Image Separator
    /// (`0x2C`) or §27 Trailer (`0x3B`), stopping the cursor on it.
    /// Used by lenient mode to recover after a malformed block.
    ///
    /// The scan is byte-by-byte; there is no risk of false-matching
    /// because the parser is only invoked here after deciding the
    /// current cursor sits in garbage. If neither byte is found, the
    /// cursor is left at end-of-stream and the outer loop terminates
    /// the parse on the next `peek_byte`.
    fn resync_to_image_or_trailer(&mut self) {
        while self.pos < self.src.len() {
            let b = self.src[self.pos];
            if b == label::IMAGE_SEPARATOR || b == label::TRAILER {
                return;
            }
            self.pos += 1;
        }
    }

    /// Parse a §20 Image Descriptor (+ §21 Local Color Table) and walk
    /// past its §22 data sub-blocks **without** expanding the LZW
    /// stream. The returned record has an empty `indices` raster; it is
    /// only used by the contract's header walk ([`scan`]).
    fn skip_image_descriptor_and_data(
        &mut self,
        graphic_control: Option<GraphicControl>,
    ) -> Result<GifFrameData> {
        let sep = self.read_byte()?;
        debug_assert_eq!(sep, label::IMAGE_SEPARATOR);
        let left = self.read_u16_le()?;
        let top = self.read_u16_le()?;
        let width = self.read_u16_le()?;
        let height = self.read_u16_le()?;
        let packed = self.read_byte()?;
        let local_flag = (packed & 0b1000_0000) != 0;
        let interlaced = (packed & 0b0100_0000) != 0;
        let palette_sorted = (packed & 0b0010_0000) != 0;
        let local_size_bits = packed & 0b0000_0111;
        let local_palette = if local_flag {
            Some(self.read_color_table(local_size_bits)?)
        } else {
            None
        };
        // §22 — LZW Minimum Code Size byte, then the sub-block chain.
        let _min_code_size = self.read_byte()?;
        self.skip_data_sub_blocks()?;
        Ok(GifFrameData {
            left,
            top,
            width,
            height,
            local_palette,
            palette_sorted,
            interlaced,
            indices: Vec::new(),
            graphic_control,
        })
    }

    fn read_image_descriptor_and_data(
        &mut self,
        graphic_control: Option<GraphicControl>,
    ) -> Result<GifFrameData> {
        // Consume the image separator.
        let sep = self.read_byte()?;
        debug_assert_eq!(sep, label::IMAGE_SEPARATOR);

        // §20.c: left, top, width, height, packed.
        let left = self.read_u16_le()?;
        let top = self.read_u16_le()?;
        let width = self.read_u16_le()?;
        let height = self.read_u16_le()?;
        let packed = self.read_byte()?;

        // §20.c "<Packed Fields>" — bit layout:
        //  bit 7      : Local Color Table Flag
        //  bit 6      : Interlace Flag
        //  bit 5      : Sort Flag
        //  bits 4..3  : Reserved
        //  bits 2..0  : Size of Local Color Table
        let local_flag = (packed & 0b1000_0000) != 0;
        let interlaced = (packed & 0b0100_0000) != 0;
        let palette_sorted = (packed & 0b0010_0000) != 0;
        let local_size_bits = packed & 0b0000_0111;

        let local_palette = if local_flag {
            Some(self.read_color_table(local_size_bits)?)
        } else {
            None
        };

        // §22 — LZW Minimum Code Size byte, then sub-block sequence.
        let min_code_size = self.read_byte()?;
        let raw = self.read_data_sub_blocks()?;

        let pixels_expected = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| Error::InvalidData("image width × height overflows".into()))?;

        // DecodeOptions: bound the raster about to be allocated, and
        // the running total across the stream, before the LZW
        // expansion commits memory.
        self.opts
            .check_pixels(u32::from(width), u32::from(height))?;
        self.opts.check_bytes(pixels_expected as u64)?;
        self.decoded_total = self.decoded_total.saturating_add(pixels_expected as u64);
        self.opts.check_bytes(self.decoded_total)?;

        let raw_indices = lzw::decode(min_code_size, &raw, pixels_expected)?;
        if raw_indices.len() != pixels_expected {
            // Spec gives no recovery contract: pad with the background
            // (index 0) is risky because index 0 may not exist in the
            // active palette. Refuse instead.
            return Err(Error::InvalidData(format!(
                "LZW produced {} pixels, expected {}",
                raw_indices.len(),
                pixels_expected
            )));
        }

        let indices = if interlaced {
            // Appendix E — re-shuffle decoded rows into top-to-bottom
            // order before exposing them.
            let mut out = vec![0u8; pixels_expected];
            let row_order = interlace_row_order(height);
            let row_bytes = width as usize;
            for (storage_row, &target_row) in row_order.iter().enumerate() {
                let src_off = storage_row * row_bytes;
                let dst_off = (target_row as usize) * row_bytes;
                out[dst_off..dst_off + row_bytes]
                    .copy_from_slice(&raw_indices[src_off..src_off + row_bytes]);
            }
            out
        } else {
            raw_indices
        };

        Ok(GifFrameData {
            left,
            top,
            width,
            height,
            local_palette,
            palette_sorted,
            interlaced,
            indices,
            graphic_control,
        })
    }

    fn read_graphic_control_extension(&mut self) -> Result<GraphicControl> {
        // §23.c: block_size byte (must be 4) + packed + delay (LE u16)
        // + transparent index + zero block terminator.
        let block_size = self.read_byte()?;
        if block_size != 4 {
            return Err(Error::InvalidData(format!(
                "Graphic Control Extension block size {block_size} != 4"
            )));
        }
        let packed = self.read_byte()?;
        let delay = self.read_u16_le()?;
        let transparent_index = self.read_byte()?;
        let term = self.read_byte()?;
        if term != 0 {
            return Err(Error::InvalidData(format!(
                "Graphic Control Extension missing block terminator (got 0x{term:02X})"
            )));
        }
        // §23.c "<Packed Fields>" — bit layout:
        //  bits 7..5 : Reserved
        //  bits 4..2 : Disposal Method
        //  bit 1     : User Input Flag
        //  bit 0     : Transparency Flag
        let disposal_bits = (packed >> 2) & 0b0000_0111;
        let user_input = (packed & 0b0000_0010) != 0;
        let transparency_flag = (packed & 0b0000_0001) != 0;
        Ok(GraphicControl {
            disposal: DisposalMethod::from_bits(disposal_bits),
            user_input,
            transparent_index: if transparency_flag {
                Some(transparent_index)
            } else {
                None
            },
            delay_centis: delay,
        })
    }

    fn read_plain_text_extension(&mut self) -> Result<PlainText> {
        // §25.c: block_size (12), then 12 bytes of fixed parameters,
        // then sub-block payload, then terminator.
        let block_size = self.read_byte()?;
        if block_size != 12 {
            return Err(Error::InvalidData(format!(
                "Plain Text Extension block size {block_size} != 12"
            )));
        }
        let left = self.read_u16_le()?;
        let top = self.read_u16_le()?;
        let width = self.read_u16_le()?;
        let height = self.read_u16_le()?;
        let cell_w = self.read_byte()?;
        let cell_h = self.read_byte()?;
        let fg = self.read_byte()?;
        let bg = self.read_byte()?;
        let text = self.read_data_sub_blocks()?;
        Ok(PlainText {
            left,
            top,
            width,
            height,
            cell_width: cell_w,
            cell_height: cell_h,
            fg_color_index: fg,
            bg_color_index: bg,
            text,
        })
    }

    fn read_application_extension(&mut self) -> Result<Application> {
        // §26.c: block_size (11), 8-byte identifier, 3-byte auth code,
        // sub-block payload, terminator.
        let block_size = self.read_byte()?;
        if block_size != 11 {
            return Err(Error::InvalidData(format!(
                "Application Extension block size {block_size} != 11"
            )));
        }
        let identifier: [u8; 8] = self.read_array()?;
        let auth_code: [u8; 3] = self.read_array()?;
        let data = self.read_data_sub_blocks()?;
        Ok(Application {
            identifier,
            auth_code,
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_ext::{ExifMetadata, XmpPacket};
    use crate::encoder::encode_file as encode;
    use crate::image::{Block, GifFile, GifFrameData as GifFrame, Rgb, Version};

    fn one_frame_image_no_extensions() -> GifFile {
        GifFile {
            version: Version::Gif89a,
            screen_width: 2,
            screen_height: 2,
            color_resolution: 1,
            global_palette_sorted: false,
            background_index: 0,
            pixel_aspect_ratio: 0,
            global_palette: Some(vec![
                Rgb::new(0, 0, 0),
                Rgb::new(0xFF, 0xFF, 0xFF),
                Rgb::new(0xFF, 0, 0),
                Rgb::new(0, 0xFF, 0),
            ]),
            blocks: vec![Block::Image(GifFrame {
                left: 0,
                top: 0,
                width: 2,
                height: 2,
                local_palette: None,
                palette_sorted: false,
                interlaced: false,
                indices: vec![0, 1, 2, 3],
                graphic_control: None,
            })],
        }
    }

    /// Fast-path on a one-image stream returns exactly that image.
    #[test]
    fn fast_path_returns_single_image_block() {
        let bytes = encode(&one_frame_image_no_extensions()).unwrap();
        let img = parse_first_frame(&bytes).unwrap();
        assert_eq!(img.blocks.len(), 1);
        let f = img.frames().next().unwrap();
        assert_eq!(f.indices, vec![0, 1, 2, 3]);
    }

    /// Fast-path skips Comment / Application / Plain Text blocks that
    /// sit between the LSD and the first image. The skipped blocks
    /// must NOT appear in the returned `GifFile`.
    #[test]
    fn fast_path_skips_extensions_before_image() {
        let mut img = one_frame_image_no_extensions();
        let exif = ExifMetadata::new(b"II*\0\x08\x00\x00\x00".to_vec());
        let xmp = XmpPacket {
            bytes: b"<x:xmpmeta/>".to_vec(),
        };
        // Re-order so extensions sit BEFORE the image block.
        let frame_block = img.blocks.remove(0);
        img.blocks = vec![
            Block::Comment(b"hi".to_vec()),
            Block::Application(exif.to_application()),
            Block::Application(xmp.to_application()),
            frame_block,
        ];
        let bytes = encode(&img).unwrap();
        let cover = parse_first_frame(&bytes).unwrap();
        // Only the image block survives.
        assert_eq!(cover.blocks.len(), 1);
        assert!(matches!(cover.blocks[0], Block::Image(_)));
        // Trailing extensions / blocks after the image are NOT
        // consumed — that is the whole point of the fast-path.
    }

    /// Fast-path attaches the most recent Graphic Control Extension to
    /// the image (§23.d "scope is the next graphic-rendering block").
    #[test]
    fn fast_path_attaches_pending_gce_to_image() {
        let mut img = one_frame_image_no_extensions();
        if let Block::Image(f) = &mut img.blocks[0] {
            f.graphic_control = Some(GraphicControl {
                disposal: DisposalMethod::RestoreBackground,
                user_input: false,
                transparent_index: Some(2),
                delay_centis: 25,
            });
        }
        let bytes = encode(&img).unwrap();
        let cover = parse_first_frame(&bytes).unwrap();
        let f = cover.frames().next().unwrap();
        let gce = f.graphic_control.as_ref().unwrap();
        assert_eq!(gce.disposal, DisposalMethod::RestoreBackground);
        assert_eq!(gce.transparent_index, Some(2));
        assert_eq!(gce.delay_centis, 25);
    }

    /// Fast-path on a stream with zero image blocks (only the trailer
    /// after the LSD) reports `InvalidData` rather than producing an
    /// empty `GifFile`.
    #[test]
    fn fast_path_errors_on_image_free_stream() {
        // Hand-rolled minimal stream: header + LSD + trailer, no GCT.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"GIF89a");
        bytes.extend_from_slice(&[1, 0, 1, 0, 0, 0, 0]); // 1×1, no GCT
        bytes.push(0x3B); // Trailer
        let err = parse_first_frame(&bytes).unwrap_err();
        match err {
            Error::InvalidData(s) => assert!(s.contains("no image block")),
            _ => panic!("unexpected error: {err:?}"),
        }
    }

    /// Lenient mode accepts a well-formed stream byte-for-byte the
    /// same way strict mode does.
    #[test]
    fn lenient_matches_strict_on_well_formed_stream() {
        let img = one_frame_image_no_extensions();
        let bytes = encode(&img).unwrap();
        let strict = parse(&bytes).unwrap();
        let lenient = parse_lenient(&bytes).unwrap();
        assert_eq!(strict, lenient);
    }

    /// Lenient mode recovers a still-readable second frame after
    /// catching a corrupted LZW payload in the first one. Strict
    /// mode aborts on the same input.
    #[test]
    fn lenient_recovers_frame_after_corrupted_lzw_payload() {
        // Build a well-formed two-frame stream, then surgically corrupt
        // the first frame's LZW sub-block payload — keep enough framing
        // so the parser identifies the §20 Image Separator and reaches
        // the §22 minimum-code-size byte, but corrupt the LZW bytes so
        // they fail to decode to the expected number of pixels.
        let mut img = one_frame_image_no_extensions();
        img.blocks.push(Block::Image(GifFrame {
            left: 0,
            top: 0,
            width: 2,
            height: 2,
            local_palette: None,
            palette_sorted: false,
            interlaced: false,
            indices: vec![3, 2, 1, 0],
            graphic_control: None,
        }));
        let mut bytes = encode(&img).unwrap();
        // Locate the first §20 Image Separator (0x2C) past the LSD/GCT.
        // Skip 6-byte header + 7-byte LSD + GCT bytes.
        let lsd_packed = bytes[10];
        let gct_size = if (lsd_packed & 0x80) != 0 {
            3 * (1 << ((lsd_packed & 0x07) as u32 + 1))
        } else {
            0
        };
        let first_sep = 6 + 7 + gct_size;
        assert_eq!(bytes[first_sep], 0x2C, "expected Image Separator");
        // Corrupt the LZW Minimum Code Size byte. Image Descriptor
        // (§20.c) is 10 bytes: 1 separator + 2+2+2+2 LE u16 +
        // 1 packed = 10. No LCT in this frame, so the §22 min code
        // size byte sits at `first_sep + 10`. Set it to an illegal
        // value (9 — above the spec ceiling of 8). The decoder
        // rejects this with InvalidData *before* it touches the LZW
        // payload, so strict aborts but lenient skips ahead to the
        // next §20 separator.
        let min_code_size_off = first_sep + 10;
        bytes[min_code_size_off] = 9;

        // Strict decode should fail.
        let strict = parse(&bytes);
        assert!(strict.is_err(), "strict should reject");

        // Lenient decode should recover the second frame.
        let lenient = parse_lenient(&bytes).unwrap();
        let frames: Vec<_> = lenient.frames().collect();
        assert_eq!(frames.len(), 1, "expected to recover the second frame");
        assert_eq!(frames[0].indices, vec![3, 2, 1, 0]);
    }

    /// Lenient mode handles a truncated stream — missing §27 Trailer —
    /// by returning whatever has been recovered so far.
    #[test]
    fn lenient_tolerates_missing_trailer() {
        let img = one_frame_image_no_extensions();
        let bytes = encode(&img).unwrap();
        let truncated = &bytes[..bytes.len() - 1]; // drop the 0x3B Trailer
                                                   // Strict still works if the §22 sub-block terminator + GCE
                                                   // already ended the parse before the trailer — but it more
                                                   // often won't, depending on the stream. Lenient must always
                                                   // succeed.
        let lenient = parse_lenient(truncated).unwrap();
        assert_eq!(lenient.blocks.len(), 1);
    }

    /// Lenient mode skips garbage bytes between blocks and resumes at
    /// the next §20 Image Separator.
    #[test]
    fn lenient_skips_garbage_between_blocks() {
        let mut img = one_frame_image_no_extensions();
        img.blocks.push(Block::Image(GifFrame {
            left: 0,
            top: 0,
            width: 2,
            height: 2,
            local_palette: None,
            palette_sorted: false,
            interlaced: false,
            indices: vec![1, 1, 1, 1],
            graphic_control: None,
        }));
        let mut bytes = encode(&img).unwrap();
        // Splice garbage right after the LSD/GCT, before the first
        // Image Separator. The strict parser would reject the
        // garbage immediately; the lenient parser drops it byte by
        // byte until it finds 0x2C.
        let lsd_packed = bytes[10];
        let gct_size = if (lsd_packed & 0x80) != 0 {
            3 * (1 << ((lsd_packed & 0x07) as u32 + 1))
        } else {
            0
        };
        let insertion_point = 6 + 7 + gct_size;
        let garbage = [0x55u8, 0x66, 0x77];
        let mut spliced = bytes[..insertion_point].to_vec();
        spliced.extend_from_slice(&garbage);
        spliced.extend_from_slice(&bytes[insertion_point..]);
        bytes = spliced;

        let lenient = parse_lenient(&bytes).unwrap();
        let frames: Vec<_> = lenient.frames().collect();
        // Both frames recovered despite the garbage in between.
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].indices, vec![0, 1, 2, 3]);
        assert_eq!(frames[1].indices, vec![1, 1, 1, 1]);
    }

    /// Header / LSD corruption is NOT recoverable: the prefix
    /// applies to every block, so lenient mode still errors there.
    #[test]
    fn lenient_still_errors_on_header_corruption() {
        let mut bytes = b"NOT".to_vec();
        bytes.extend_from_slice(b"89a"); // garbage signature
        bytes.extend_from_slice(&[1, 0, 1, 0, 0, 0, 0]);
        bytes.push(0x3B);
        assert!(parse_lenient(&bytes).is_err());
    }

    /// Fast-path agrees with the full decoder on the first frame's
    /// pixels for streams whose first block IS an image.
    #[test]
    fn fast_path_agrees_with_full_decode_on_first_frame() {
        let mut img = one_frame_image_no_extensions();
        // Add a trailing comment + a second image to make sure full
        // decode gets more blocks but fast-path stops early.
        img.blocks.push(Block::Comment(b"trailing".to_vec()));
        img.blocks.push(Block::Image(GifFrame {
            left: 0,
            top: 0,
            width: 2,
            height: 2,
            local_palette: None,
            palette_sorted: false,
            interlaced: false,
            indices: vec![3, 2, 1, 0],
            graphic_control: None,
        }));
        let bytes = encode(&img).unwrap();
        let full = parse(&bytes).unwrap();
        let fast = parse_first_frame(&bytes).unwrap();
        assert_eq!(fast.blocks.len(), 1);
        assert!(full.blocks.len() > 1);
        // Same first frame content.
        assert_eq!(full.frames().next().unwrap().indices, vec![0, 1, 2, 3]);
        assert_eq!(fast.frames().next().unwrap().indices, vec![0, 1, 2, 3]);
        // Same screen-level metadata.
        assert_eq!(fast.screen_width, full.screen_width);
        assert_eq!(fast.screen_height, full.screen_height);
        assert_eq!(fast.global_palette, full.global_palette);
    }
}
