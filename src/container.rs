//! GIF container: demuxer + muxer behind [`crate::register_containers`].
//!
//! GIF is its own container — the §B grammar `<GIF Data Stream>` *is*
//! the file — so the demuxer's job is to publish the stream layout the
//! registered `gif` decoder will emit and to cut an animation into one
//! packet per frame. Every packet this module produces or accepts is a
//! complete, standalone `<GIF Data Stream>` (Header → Logical Screen
//! Descriptor → blocks → Trailer), so Layer 1 [`crate::decode`] reads any
//! packet on its own and the `gif` encoder's output (one single-image
//! stream per frame) muxes without translation.
//!
//! # Stream layout
//!
//! One video stream, `codec_id = "gif"`, `width` / `height` = the §18
//! Logical Screen, time base [`TIME_BASE`] = 1/100 s (GIF's own
//! §23.c.vii Delay Time tick).
//!
//! * **Still** (exactly one graphic-rendering block, a §20 image): one
//!   packet holding the whole file; `pixel_format` is the native layout
//!   [`crate::decode`] returns for the file (`Pal8`, or `Rgba` when the
//!   256-entry table leaves no transparent slot — see [`crate::GifImage`])
//!   and, for `Pal8`, the palette rides `extradata` (RGB triplets, the
//!   same bytes the decoded frame's palette side-channel carries).
//! * **Animation** (two or more graphic-rendering blocks, or a §25 Plain
//!   Text block): one packet per block in file order, `pixel_format =
//!   Rgba` (the composited canvas, as [`crate::decode_all`] returns),
//!   `pts` cumulative from `0` and `duration` = the block's §23.c.vii
//!   Delay Time (`None` without a Graphic Control Extension). Packet
//!   `i` is the file's Header + Logical Screen Descriptor + Global Color
//!   Table followed by every block from the end of block `i − 1` up to
//!   and including graphic block `i` (so a §23 GCE, §24 Comment or §26
//!   Application Extension travels with the graphic block it precedes;
//!   trailing blocks ride the last packet) and a §27 Trailer. Every
//!   byte of the file lands in exactly one packet.
//!
//! [`Demuxer::metadata`] carries `("loop_count", n)` from the
//! NETSCAPE2.0 / ANIMEXTS1.0 *Looping* sub-block and one
//! `("comment", text)` per §24 Comment Extension (lossy UTF-8).
//!
//! # `extradata`
//!
//! The demuxer tells the decoder which of the two layouts above a
//! stream uses through `CodecParameters::extradata`:
//!
//! ```text
//! byte 0   EXTRADATA_VERSION (1)
//! byte 1   EXTRADATA_STILL (0) | EXTRADATA_ANIMATION (1)
//! 2..      still, Pal8 only: the palette as RGB triplets (3 × N bytes)
//! ```
//!
//! [`is_animation_stream`] / [`extradata_palette`] read it. A decoder
//! built from parameters without this record (any other producer of
//! `gif` packets) keeps the whole-stream behaviour: every packet is a
//! complete file, stills come out native and animations composited.
//!
//! # Muxer
//!
//! [`open_muxer`] takes one `gif` video stream. A single packet is
//! written verbatim (it is already a complete file). Two or more packets
//! are merged into one animated GIF: the first packet's Header / Logical
//! Screen / Global Color Table lead, every packet's blocks follow in
//! order (an image that used its own packet's Global Color Table gets it
//! as a §21 Local Color Table when that table differs from the shared
//! one), each packet's `duration` — rescaled from its `time_base` to
//! centiseconds — becomes its frame's §23.c.vii Delay Time (a packet
//! without a duration keeps the delay its own GCE carries), and a
//! NETSCAPE2.0 *Looping* block is added when none of the packets has
//! one, from the stream's `loop_count` option (the `gif` encoder's
//! schema; `Some(0)` = loop forever by default, `-1` writes none). The
//! disposal method of each frame is whatever its packet's GCE says —
//! the `gif` encoder writes none, so full-canvas opaque frames compose
//! back exactly ([`crate::encode_all`] semantics).
//!
//! Whole module gated behind the `registry` feature — the container
//! surface is framework-side only.

use std::io::{Read, SeekFrom, Write};

use oxideav_core::{
    parse_options, CodecId, CodecParameters, CodecResolver, ContainerRegistry, Demuxer,
    Error as CoreError, MediaType, Muxer, Packet, PixelFormat, ProbeData, ProbeScore, ReadSeek,
    Result as CoreResult, Rounding, StreamInfo, TimeBase, WriteSeek, MAX_PROBE_SCORE,
    PROBE_SCORE_EXTENSION,
};

use crate::api::first_frame_native;
use crate::app_ext::{AnimextsLoopControl, LoopControl};
use crate::decoder::scan;
use crate::encoder::{encode_file_with, EncodeOptions};
use crate::image::{Block, DisposalMethod, GifFile, GraphicControl};
use crate::options::DecodeOptions;
use crate::registry::to_core_pixel_format;
use crate::types::Palette;

/// GIF's own clock: the §23.c.vii Delay Time counts hundredths of a
/// second. Every packet and the stream use this time base.
pub const TIME_BASE: TimeBase = TimeBase::new(1, 100);

/// `extradata[0]`: layout version of the record this module writes.
pub const EXTRADATA_VERSION: u8 = 1;
/// `extradata[1]` for a still (one packet, native layout).
pub const EXTRADATA_STILL: u8 = 0;
/// `extradata[1]` for an animation (one packet per frame, `Rgba`).
pub const EXTRADATA_ANIMATION: u8 = 1;

/// Container name registered for GIF (demuxer, muxer, probe, extension).
pub const CONTAINER_NAME: &str = "gif";

/// Register the GIF container: demuxer + muxer + `.gif` extension + probe.
pub fn register(reg: &mut ContainerRegistry) {
    reg.register_demuxer(CONTAINER_NAME, open_demuxer);
    reg.register_muxer(CONTAINER_NAME, open_muxer);
    reg.register_extension("gif", CONTAINER_NAME);
    reg.register_probe(CONTAINER_NAME, probe);
}

/// Content probe: the §17 Header (`GIF87a` / `GIF89a`) is unambiguous;
/// the `.gif` extension alone scores [`PROBE_SCORE_EXTENSION`].
pub fn probe(data: &ProbeData) -> ProbeScore {
    if crate::probe(data.buf) {
        return MAX_PROBE_SCORE;
    }
    if data.ext == Some("gif") {
        PROBE_SCORE_EXTENSION
    } else {
        0
    }
}

/// `true` when `params.extradata` carries this module's record and marks
/// the stream as an animation (one single-frame packet per graphic
/// block, to be composited across packets).
pub fn is_animation_stream(params: &CodecParameters) -> bool {
    matches!(
        params.extradata.as_slice(),
        [EXTRADATA_VERSION, EXTRADATA_ANIMATION, ..]
    )
}

/// The palette (RGB triplets) a still `Pal8` stream declares in its
/// `extradata`; `None` when the record is absent, marks an animation,
/// or carries no palette (an `Rgba` still).
pub fn extradata_palette(params: &CodecParameters) -> Option<&[u8]> {
    match params.extradata.as_slice() {
        [EXTRADATA_VERSION, EXTRADATA_STILL, rest @ ..]
            if !rest.is_empty() && rest.len() % 3 == 0 =>
        {
            Some(rest)
        }
        _ => None,
    }
}

fn still_extradata(palette: Option<&Palette>) -> Vec<u8> {
    let mut out = vec![EXTRADATA_VERSION, EXTRADATA_STILL];
    if let Some(p) = palette {
        out.extend(p.entries.iter().flat_map(|e| [e[0], e[1], e[2]]));
    }
    out
}

fn animation_extradata() -> Vec<u8> {
    vec![EXTRADATA_VERSION, EXTRADATA_ANIMATION]
}

// ---- Demuxer ------------------------------------------------------------

/// Open a GIF file as a one-stream container (see the module docs for
/// the still / animation packetisation).
pub fn open_demuxer(
    mut input: Box<dyn ReadSeek>,
    _codecs: &dyn CodecResolver,
) -> CoreResult<Box<dyn Demuxer>> {
    input.seek(SeekFrom::Start(0))?;
    let mut buf = Vec::new();
    input.read_to_end(&mut buf)?;
    drop(input);

    if !crate::probe(&buf) {
        return Err(CoreError::invalid(
            "GIF: bad magic (expected GIF87a or GIF89a header)",
        ));
    }
    // The header walk (no LZW raster is expanded): Logical Screen,
    // Global Color Table, first image, every graphic block's extent.
    let s = scan(&buf, &DecodeOptions::default().unlimited(), false)?;
    if s.spans.is_empty() {
        return Err(CoreError::invalid(
            "GIF: stream contains no graphic-rendering block",
        ));
    }
    let file = &s.file;
    let (width, height) = (u32::from(file.screen_width), u32::from(file.screen_height));

    let mut params = CodecParameters::video(CodecId::new(crate::CODEC_ID_STR));
    params.width = Some(width);
    params.height = Some(height);

    let still = s.image_count == 1 && s.spans.len() == 1;
    let mut packets = Vec::with_capacity(s.spans.len());
    if still {
        // What `decode` returns for this file: `Pal8` + palette, or `Rgba`.
        let (fmt, palette) = first_frame_native(file)?;
        params.pixel_format = Some(to_core_pixel_format(fmt));
        params.extradata = still_extradata(palette.as_ref());
        let mut pkt = Packet::new(0, TIME_BASE, buf);
        pkt.pts = Some(0);
        pkt.dts = Some(0);
        pkt.duration = s.spans[0].delay_centis.map(i64::from);
        pkt.flags.keyframe = true;
        packets.push(pkt);
    } else {
        // `decode_all` parses the whole stream strictly; hold the
        // demuxer to the same standard instead of silently cutting a
        // truncated animation short.
        if let Some(e) = s.walk_error {
            return Err(e.into());
        }
        params.pixel_format = Some(PixelFormat::Rgba);
        params.extradata = animation_extradata();
        let prefix = &buf[..s.prefix_len];
        let mut pts: i64 = 0;
        for (i, span) in s.spans.iter().enumerate() {
            let body = &buf[span.start..span.end];
            let mut data = Vec::with_capacity(prefix.len() + body.len() + 1);
            data.extend_from_slice(prefix);
            data.extend_from_slice(body);
            data.push(0x3B); // §27 Trailer
            let mut pkt = Packet::new(0, TIME_BASE, data);
            pkt.pts = Some(pts);
            pkt.dts = Some(pts);
            pkt.duration = span.delay_centis.map(i64::from);
            // Later frames compose over the previous canvas (§23
            // disposal), so only the first is a random-access point.
            pkt.flags.keyframe = i == 0;
            pts += i64::from(span.delay_centis.unwrap_or(0));
            packets.push(pkt);
        }
    }

    let total: i64 = packets.iter().filter_map(|p| p.duration).sum();
    let stream = StreamInfo {
        index: 0,
        time_base: TIME_BASE,
        duration: if still { None } else { Some(total) },
        start_time: Some(0),
        params,
    };

    let mut metadata: Vec<(String, String)> = Vec::new();
    if let Some(n) = file.loop_count() {
        metadata.push(("loop_count".into(), n.to_string()));
    }
    for c in &s.comments {
        metadata.push(("comment".into(), String::from_utf8_lossy(c).into_owned()));
    }

    Ok(Box::new(GifDemuxer {
        stream,
        packets,
        pos: 0,
        metadata,
    }))
}

struct GifDemuxer {
    stream: StreamInfo,
    packets: Vec<Packet>,
    pos: usize,
    metadata: Vec<(String, String)>,
}

impl Demuxer for GifDemuxer {
    fn format_name(&self) -> &str {
        CONTAINER_NAME
    }

    fn streams(&self) -> &[StreamInfo] {
        std::slice::from_ref(&self.stream)
    }

    fn next_packet(&mut self) -> CoreResult<Packet> {
        let Some(pkt) = self.packets.get(self.pos) else {
            return Err(CoreError::Eof);
        };
        self.pos += 1;
        Ok(pkt.clone())
    }

    fn metadata(&self) -> &[(String, String)] {
        &self.metadata
    }

    fn duration_micros(&self) -> Option<i64> {
        self.stream.duration.map(|d| d.saturating_mul(10_000))
    }
}

// ---- Muxer --------------------------------------------------------------

/// Open a GIF muxer for exactly one `gif` video stream (see the module
/// docs for how several packets become one animated file).
pub fn open_muxer(
    output: Box<dyn WriteSeek>,
    streams: &[StreamInfo],
) -> CoreResult<Box<dyn Muxer>> {
    if streams.len() != 1 {
        return Err(CoreError::invalid(
            "GIF muxer: exactly one video stream expected",
        ));
    }
    let s = &streams[0];
    if s.params.media_type != MediaType::Video {
        return Err(CoreError::invalid("GIF muxer: stream must be video"));
    }
    if s.params.codec_id.as_str() != crate::CODEC_ID_STR {
        return Err(CoreError::invalid(format!(
            "GIF muxer: codec_id must be gif (got {})",
            s.params.codec_id
        )));
    }
    // The `gif` encoder's option schema travels on its output params;
    // its `loop_count` decides the NETSCAPE2.0 block of a merged file.
    let loop_count = parse_options::<EncodeOptions>(&s.params.options)
        .map(|o| o.loop_count)
        .unwrap_or(EncodeOptions::default().loop_count);
    Ok(Box::new(GifMuxer {
        output,
        loop_count,
        packets: Vec::new(),
        header_written: false,
        trailer_written: false,
    }))
}

struct GifMuxer {
    output: Box<dyn WriteSeek>,
    loop_count: Option<u16>,
    packets: Vec<Packet>,
    header_written: bool,
    trailer_written: bool,
}

impl Muxer for GifMuxer {
    fn format_name(&self) -> &str {
        CONTAINER_NAME
    }

    fn write_header(&mut self) -> CoreResult<()> {
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> CoreResult<()> {
        if !self.header_written {
            return Err(CoreError::other("GIF muxer: write_header not called"));
        }
        if !crate::probe(&packet.data) {
            return Err(CoreError::invalid(
                "GIF muxer: packet is not a GIF data stream (bad header)",
            ));
        }
        self.packets.push(packet.clone());
        Ok(())
    }

    fn write_trailer(&mut self) -> CoreResult<()> {
        if self.trailer_written {
            return Ok(());
        }
        match self.packets.len() {
            0 => return Err(CoreError::invalid("GIF muxer: no packets written")),
            // One packet is already a complete file.
            1 => self.output.write_all(&self.packets[0].data)?,
            _ => {
                let merged = merge_packets(&self.packets, self.loop_count)?;
                self.output.write_all(&merged)?;
            }
        }
        self.output.flush()?;
        self.trailer_written = true;
        Ok(())
    }
}

/// `duration` in `tb` as a §23.c.vii Delay Time (centiseconds, rounded
/// to nearest, saturating at the 16-bit field).
fn duration_to_centis(duration: i64, tb: TimeBase) -> u16 {
    let centis = tb.rescale_rnd(duration, TIME_BASE, Rounding::NearestAway);
    u16::try_from(centis.max(0)).unwrap_or(u16::MAX)
}

fn is_loop_block(block: &Block) -> bool {
    match block {
        Block::Application(a) => {
            LoopControl::from_application(a).is_some()
                || AnimextsLoopControl::from_application(a).is_some()
        }
        _ => false,
    }
}

/// Set the §23.c.vii Delay Time of a graphic-rendering block, creating
/// a Graphic Control Extension when the block has none and the delay is
/// non-zero.
fn set_block_delay(block: &mut Block, delay_centis: u16) {
    let gce = match block {
        Block::Image(f) => &mut f.graphic_control,
        Block::PlainText {
            graphic_control, ..
        } => graphic_control,
        _ => return,
    };
    match gce {
        Some(g) => g.delay_centis = delay_centis,
        None if delay_centis > 0 => {
            *gce = Some(GraphicControl {
                disposal: DisposalMethod::None,
                user_input: false,
                transparent_index: None,
                delay_centis,
            });
        }
        None => {}
    }
}

/// Merge N complete GIF data streams (one per packet) into one animated
/// file — see the module docs.
fn merge_packets(packets: &[Packet], loop_count: Option<u16>) -> CoreResult<Vec<u8>> {
    let opts = DecodeOptions::default().unlimited();
    let mut files = Vec::with_capacity(packets.len());
    for (i, p) in packets.iter().enumerate() {
        let file = crate::parse_with(&p.data, &opts).map_err(|e| {
            CoreError::invalid(format!("GIF muxer: packet {i} does not parse: {e}"))
        })?;
        files.push(file);
    }
    let first = &files[0];
    let (sw, sh) = (first.screen_width, first.screen_height);
    let gct = first.global_palette.clone();

    let mut blocks: Vec<Block> = Vec::new();
    let mut has_loop = false;
    for (i, (file, pkt)) in files.iter().zip(packets).enumerate() {
        if file.screen_width != sw || file.screen_height != sh {
            return Err(CoreError::invalid(format!(
                "GIF muxer: packet {i} is a {}×{} logical screen, packet 0 is {sw}×{sh}",
                file.screen_width, file.screen_height
            )));
        }
        let own_table = file.global_palette != gct;
        let from = blocks.len();
        for block in &file.blocks {
            match block {
                Block::Application(_) if is_loop_block(block) => {
                    // One *Looping* block per file; the first wins.
                    if !has_loop {
                        has_loop = true;
                        blocks.push(block.clone());
                    }
                }
                Block::Image(f) => {
                    let mut f = f.clone();
                    if own_table && f.local_palette.is_none() {
                        // The image drew from its own packet's Global
                        // Color Table; carry that table along as a §21
                        // Local Color Table.
                        f.local_palette = file.global_palette.clone();
                    }
                    blocks.push(Block::Image(f));
                }
                Block::PlainText { .. } if own_table => {
                    return Err(CoreError::unsupported(format!(
                        "GIF muxer: packet {i} holds a Plain Text block but its Global Color \
                         Table differs from packet 0's (§25.a: Plain Text renders from the \
                         Global Color Table)"
                    )));
                }
                other => blocks.push(other.clone()),
            }
        }
        if let Some(d) = pkt.duration {
            let centis = duration_to_centis(d, pkt.time_base);
            if let Some(last) = blocks[from..]
                .iter_mut()
                .rev()
                .find(|b| b.is_graphic_rendering())
            {
                set_block_delay(last, centis);
            }
        }
    }
    if !has_loop {
        if let Some(n) = loop_count {
            let ctl = LoopControl {
                loop_count: Some(n),
                buffer_size: None,
            };
            blocks.insert(0, Block::Application(ctl.to_application()));
        }
    }

    let mut merged = GifFile {
        version: first.version,
        screen_width: sw,
        screen_height: sh,
        color_resolution: first.color_resolution,
        global_palette_sorted: first.global_palette_sorted,
        background_index: first.background_index,
        pixel_aspect_ratio: first.pixel_aspect_ratio,
        global_palette: gct,
        blocks,
    };
    merged.upgrade_version_if_needed();
    Ok(encode_file_with(&merged, &EncodeOptions::default())?)
}
