//! The GIF container through the framework registry (round 472):
//! probe → `open_demuxer` → `first_decoder`, and `first_encoder` →
//! `open_muxer`, pinned byte-for-byte against the Layer 1 `decode` /
//! `decode_all` results for every layout the crate produces.

#![cfg(feature = "registry")]

use std::io::{Cursor, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxideav_core::{
    CodecId, CodecParameters, Error as CoreError, Frame as CoreFrame, Packet, PixelFormat,
    RuntimeContext, StreamInfo, TimeBase, VideoFrame,
};
use oxideav_gif::container::{self, extradata_palette, is_animation_stream, TIME_BASE};
use oxideav_gif::{
    app_ext::LoopControl, decode, decode_all, encode, encode_all, info, Block, DisposalMethod,
    EncodeOptions, Frame, GifFile, GifFrameData, GifImage, GraphicControl, Palette, Rgb, Version,
    CODEC_ID_STR,
};

// ---- fixtures (built with Layer 1, no files on disk) ----------------------

fn ctx() -> RuntimeContext {
    let mut ctx = RuntimeContext::new();
    oxideav_gif::register(&mut ctx);
    ctx
}

fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

/// 4×2 opaque image with two colours → `Pal8` covering the screen.
fn still_pal8_opaque() -> Vec<u8> {
    let img = GifImage::from_rgb8(
        4,
        2,
        vec![
            1, 2, 3, 1, 2, 3, 9, 8, 7, 9, 8, 7, 9, 8, 7, 1, 2, 3, 1, 2, 3, 9, 8, 7,
        ],
    )
    .unwrap();
    encode(&img, &EncodeOptions::default()).unwrap()
}

/// 6×4 screen, 4-entry Global Color Table, one 3×2 image at (1, 1)
/// without a transparent index → `Pal8` with a synthetic transparent
/// entry (and a padded table).
fn still_pal8_subrect_synthetic() -> Vec<u8> {
    let file = GifFile {
        version: Version::Gif89a,
        screen_width: 6,
        screen_height: 4,
        color_resolution: 1,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(vec![
            rgb(10, 20, 30),
            rgb(40, 50, 60),
            rgb(70, 80, 90),
            rgb(100, 110, 120),
        ]),
        blocks: vec![Block::Image(GifFrameData {
            left: 1,
            top: 1,
            width: 3,
            height: 2,
            local_palette: None,
            palette_sorted: false,
            interlaced: false,
            indices: vec![0, 1, 2, 3, 2, 1],
            graphic_control: None,
        })],
    };
    file.to_bytes().unwrap()
}

/// 5×3 screen, 256-entry Global Color Table, one 2×2 image at (2, 1)
/// without a transparent index → no room for a transparent slot →
/// `Rgba`.
fn still_rgba() -> Vec<u8> {
    let gct: Vec<Rgb> = (0..=255u8).map(|i| rgb(i, 255 - i, i ^ 0x55)).collect();
    let file = GifFile {
        version: Version::Gif87a,
        screen_width: 5,
        screen_height: 3,
        color_resolution: 7,
        global_palette_sorted: false,
        background_index: 7,
        pixel_aspect_ratio: 0,
        global_palette: Some(gct),
        blocks: vec![Block::Image(GifFrameData {
            left: 2,
            top: 1,
            width: 2,
            height: 2,
            local_palette: None,
            palette_sorted: false,
            interlaced: false,
            indices: vec![200, 201, 202, 203],
            graphic_control: None,
        })],
    };
    file.to_bytes().unwrap()
}

/// 4×3 screen animation: NETSCAPE2.0 loop 3, a leading comment, three
/// frames mixing delays (10 / none / 25), disposal (Keep / none /
/// RestoreBackground), a Local Color Table and sub-rectangles, a
/// trailing comment.
fn animation() -> Vec<u8> {
    let gce = |delay: u16, disposal: DisposalMethod, ti: Option<u8>| GraphicControl {
        disposal,
        user_input: false,
        transparent_index: ti,
        delay_centis: delay,
    };
    let file = GifFile {
        version: Version::Gif89a,
        screen_width: 4,
        screen_height: 3,
        color_resolution: 1,
        global_palette_sorted: false,
        background_index: 1,
        pixel_aspect_ratio: 0,
        global_palette: Some(vec![
            rgb(0, 0, 0),
            rgb(255, 255, 255),
            rgb(255, 0, 0),
            rgb(0, 0, 255),
        ]),
        blocks: vec![
            Block::Application(
                LoopControl {
                    loop_count: Some(3),
                    buffer_size: None,
                }
                .to_application(),
            ),
            Block::Comment(b"made by the r472 test".to_vec()),
            Block::Image(GifFrameData {
                left: 0,
                top: 0,
                width: 4,
                height: 3,
                local_palette: None,
                palette_sorted: false,
                interlaced: false,
                indices: vec![0, 1, 2, 3, 3, 2, 1, 0, 0, 1, 2, 3],
                graphic_control: Some(gce(10, DisposalMethod::Keep, None)),
            }),
            Block::Image(GifFrameData {
                left: 1,
                top: 1,
                width: 2,
                height: 1,
                local_palette: Some(vec![rgb(9, 9, 9), rgb(200, 100, 50)]),
                palette_sorted: false,
                interlaced: false,
                indices: vec![1, 0],
                graphic_control: None,
            }),
            Block::Image(GifFrameData {
                left: 2,
                top: 0,
                width: 2,
                height: 2,
                local_palette: None,
                palette_sorted: false,
                interlaced: false,
                indices: vec![2, 0, 0, 2],
                graphic_control: Some(gce(25, DisposalMethod::RestoreBackground, Some(0))),
            }),
            Block::Comment(b"the end".to_vec()),
        ],
    };
    file.to_bytes().unwrap()
}

// ---- helpers ---------------------------------------------------------------

fn open(ctx: &RuntimeContext, bytes: &[u8]) -> Box<dyn oxideav_core::Demuxer> {
    let reader: Box<dyn oxideav_core::ReadSeek> = Box::new(Cursor::new(bytes.to_vec()));
    container::open_demuxer(reader, &ctx.codecs).expect("open_demuxer")
}

/// Everything the registry path yields for `bytes`: the stream, the
/// packets and the decoded video frames (pumped exactly as the gateway
/// does: send, drain to NeedMore, flush at Eof, drain to Eof).
fn pump(ctx: &RuntimeContext, bytes: &[u8]) -> (StreamInfo, Vec<Packet>, Vec<VideoFrame>) {
    let mut demux = open(ctx, bytes);
    assert_eq!(demux.streams().len(), 1);
    let stream = demux.streams()[0].clone();
    let mut dec = ctx
        .codecs
        .first_decoder(&stream.params)
        .expect("first_decoder");
    let mut packets = Vec::new();
    let mut frames = Vec::new();
    loop {
        match demux.next_packet() {
            Ok(pkt) => {
                dec.send_packet(&pkt).expect("send_packet");
                packets.push(pkt);
                loop {
                    match dec.receive_frame() {
                        Ok(CoreFrame::Video(v)) => frames.push(v),
                        Ok(_) => panic!("non-video frame"),
                        Err(CoreError::NeedMore) => break,
                        Err(e) => panic!("receive_frame: {e}"),
                    }
                }
            }
            Err(CoreError::Eof) => break,
            Err(e) => panic!("next_packet: {e}"),
        }
    }
    dec.flush().unwrap();
    loop {
        match dec.receive_frame() {
            Ok(CoreFrame::Video(v)) => frames.push(v),
            Ok(_) => panic!("non-video frame"),
            Err(CoreError::Eof) => break,
            Err(e) => panic!("receive_frame after flush: {e}"),
        }
    }
    (stream, packets, frames)
}

/// A `WriteSeek` whose bytes survive the muxer being dropped.
#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Cursor<Vec<u8>>>>);

impl SharedBuf {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().get_ref().clone()
    }
}

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().flush()
    }
}

impl Seek for SharedBuf {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.lock().unwrap().seek(pos)
    }
}

fn mux(stream: &StreamInfo, packets: &[Packet]) -> Vec<u8> {
    let out = SharedBuf::default();
    let sink: Box<dyn oxideav_core::WriteSeek> = Box::new(out.clone());
    let mut mux = container::open_muxer(sink, std::slice::from_ref(stream)).expect("open_muxer");
    mux.write_header().unwrap();
    for p in packets {
        mux.write_packet(p).unwrap();
    }
    mux.write_trailer().unwrap();
    out.bytes()
}

fn rgb_triplets(p: &Palette) -> Vec<u8> {
    p.entries.iter().flat_map(|e| [e[0], e[1], e[2]]).collect()
}

fn to_core(pf: oxideav_gif::PixelFormat) -> PixelFormat {
    oxideav_gif::to_core_pixel_format(pf)
}

// ---- acceptance 1: probe ------------------------------------------------

#[test]
fn probe_names_gif_from_magic_alone_and_with_hint_and_rejects_foreign_files() {
    let ctx = ctx();
    for bytes in [still_pal8_opaque(), animation()] {
        let mut cur = Cursor::new(bytes.clone());
        let name = ctx
            .containers
            .probe_input(&mut cur as &mut dyn oxideav_core::ReadSeek, None)
            .unwrap();
        assert_eq!(name, "gif");
        let mut cur = Cursor::new(bytes);
        let name = ctx
            .containers
            .probe_input(&mut cur as &mut dyn oxideav_core::ReadSeek, Some("gif"))
            .unwrap();
        assert_eq!(name, "gif");
    }
    // A PNG signature and a farbfeld header are not GIF, hint or not.
    for foreign in [
        b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec(),
        b"farbfeld\0\0\0\x01\0\0\0\x01".to_vec(),
    ] {
        let mut cur = Cursor::new(foreign.clone());
        assert!(ctx
            .containers
            .probe_input(&mut cur as &mut dyn oxideav_core::ReadSeek, None)
            .is_err());
        // With the wrong extension hint the magic still has to match:
        // the probe answers 0 for foreign bytes, so the registry falls
        // back to the extension's container, whose demuxer then rejects
        // the bytes.
        let reader: Box<dyn oxideav_core::ReadSeek> = Box::new(Cursor::new(foreign));
        assert!(container::open_demuxer(reader, &ctx.codecs).is_err());
    }
}

// ---- acceptance 2 + 3: stills, native layout, byte-exact vs Layer 1 -----

#[test]
fn still_layouts_match_layer1_decode_byte_for_byte() {
    let ctx = ctx();
    let fixtures: [(&str, Vec<u8>); 3] = [
        ("pal8 opaque", still_pal8_opaque()),
        ("pal8 sub-rect synthetic", still_pal8_subrect_synthetic()),
        ("rgba 256-entry", still_rgba()),
    ];
    let mut seen = Vec::new();
    for (name, bytes) in &fixtures {
        let expect = decode(bytes).unwrap();
        let header = info(bytes).unwrap();
        assert_eq!(header.frames, 1, "{name}");
        let (stream, packets, frames) = pump(&ctx, bytes);

        // Stream geometry + native layout exactly as `info` reports.
        let p = &stream.params;
        assert_eq!(p.codec_id, CodecId::new(CODEC_ID_STR), "{name}");
        assert_eq!(p.width, Some(header.width), "{name}");
        assert_eq!(p.height, Some(header.height), "{name}");
        assert_eq!(p.pixel_format, Some(to_core(header.format)), "{name}");
        assert_eq!(p.pixel_format, Some(to_core(expect.format)), "{name}");
        assert_eq!(stream.time_base, TIME_BASE);
        assert!(!is_animation_stream(p), "{name}");
        // GIF carries no colour signalling: nothing stamped on the stream.
        assert!(p.color_signal.is_unspecified(), "{name}");

        // Palette in extradata for the indexed layout, none for Rgba.
        match &expect.palette {
            Some(pal) => assert_eq!(
                extradata_palette(p),
                Some(rgb_triplets(pal).as_slice()),
                "{name}"
            ),
            None => assert_eq!(extradata_palette(p), None, "{name}"),
        }

        // One packet = the whole file, pts 0.
        assert_eq!(packets.len(), 1, "{name}");
        assert_eq!(packets[0].data, *bytes, "{name}");
        assert_eq!(packets[0].pts, Some(0));
        assert!(packets[0].flags.keyframe);

        // The decoded frame is the Layer 1 image, byte for byte.
        assert_eq!(frames.len(), 1, "{name}");
        let v = &frames[0];
        assert_eq!(v.image_planes().len(), 1);
        assert_eq!(v.planes[0].stride, expect.stride(), "{name}");
        assert_eq!(v.planes[0].data, expect.as_bytes().unwrap(), "{name}");
        assert_eq!(
            v.palette(),
            expect.palette.as_ref().map(rgb_triplets).as_deref(),
            "{name}: palette side-channel"
        );
        assert_eq!(v.pts, Some(0));
        // The bridge rebuilds the Layer 1 image from the registry frame.
        let back = GifImage::from_video_frame(v, p).unwrap();
        assert_eq!(back.width, expect.width);
        assert_eq!(back.format, expect.format);
        assert_eq!(back.planes, expect.planes, "{name}");
        seen.push(expect.format);
    }
    // The matrix really covered both native layouts.
    assert!(seen.contains(&oxideav_gif::PixelFormat::Pal8));
    assert!(seen.contains(&oxideav_gif::PixelFormat::Rgba));
}

// ---- acceptance 4: animation = one packet per frame, GIF ticks ----------

#[test]
fn animation_packets_carry_delays_and_compose_like_decode_all() {
    let ctx = ctx();
    let bytes = animation();
    let expect = decode_all(&bytes).unwrap();
    assert_eq!(expect.len(), 3);
    let (stream, packets, frames) = pump(&ctx, &bytes);

    let p = &stream.params;
    assert_eq!((p.width, p.height), (Some(4), Some(3)));
    assert_eq!(p.pixel_format, Some(PixelFormat::Rgba));
    assert!(is_animation_stream(p));
    assert_eq!(extradata_palette(p), None);
    assert_eq!(stream.time_base, TimeBase::new(1, 100));
    assert_eq!(stream.duration, Some(35));

    // Timing: pts cumulative, duration = GCE delay (None without a GCE).
    assert_eq!(packets.len(), 3);
    let timing: Vec<(Option<i64>, Option<i64>)> =
        packets.iter().map(|p| (p.pts, p.duration)).collect();
    assert_eq!(
        timing,
        vec![(Some(0), Some(10)), (Some(10), None), (Some(10), Some(25))]
    );
    assert!(packets[0].flags.keyframe);
    assert!(!packets[1].flags.keyframe);
    // Every packet is a standalone GIF Layer 1 reads on its own, with
    // the file's screen and (for packet 1) its own Local Color Table.
    for (i, pkt) in packets.iter().enumerate() {
        assert!(oxideav_gif::probe(&pkt.data));
        let one = GifFile::parse(&pkt.data).unwrap();
        assert_eq!((one.screen_width, one.screen_height), (4, 3), "packet {i}");
        assert_eq!(one.graphic_rendering_block_count(), 1, "packet {i}");
        assert!(decode(&pkt.data).is_ok(), "packet {i} decodes standalone");
    }
    // Leading special-purpose blocks ride packet 0, trailing ones the last.
    let first = GifFile::parse(&packets[0].data).unwrap();
    assert_eq!(first.loop_count(), Some(3));
    assert_eq!(first.comments().count(), 1);
    let last = GifFile::parse(&packets[2].data).unwrap();
    assert_eq!(last.comments().next(), Some(&b"the end"[..]));
    // Every byte of the file lands in exactly one packet.
    let prefix_len = 13 + 4 * 3; // header + LSD + 4-entry GCT
    let rebuilt: Vec<u8> = std::iter::once(&bytes[..prefix_len])
        .chain(
            packets
                .iter()
                .map(|p| &p.data[prefix_len..p.data.len() - 1]),
        )
        .flatten()
        .copied()
        .chain(std::iter::once(0x3B))
        .collect();
    assert_eq!(rebuilt, bytes);

    // Frames: the composited Rgba canvases of decode_all, byte for byte,
    // with the packet pts preserved.
    assert_eq!(frames.len(), 3);
    for (i, (v, want)) in frames.iter().zip(&expect).enumerate() {
        assert_eq!(v.planes[0].stride, 16, "frame {i}");
        assert_eq!(
            v.planes[0].data,
            want.image.as_bytes().unwrap(),
            "frame {i}"
        );
        assert!(v.palette().is_none());
        assert_eq!(v.pts, packets[i].pts, "frame {i}");
    }

    // Metadata: loop count + comments.
    let demux = open(&ctx, &bytes);
    assert_eq!(
        demux.metadata(),
        &[
            ("loop_count".to_string(), "3".to_string()),
            ("comment".to_string(), "made by the r472 test".to_string()),
            ("comment".to_string(), "the end".to_string()),
        ]
    );
    assert_eq!(demux.duration_micros(), Some(350_000));
}

#[test]
fn whole_stream_decoder_is_unchanged_for_animations_without_the_record() {
    // A producer that hands the whole animation as one packet without
    // the container's extradata keeps today's behaviour.
    let bytes = animation();
    let expect = decode_all(&bytes).unwrap();
    let params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    let mut dec = oxideav_gif::make_decoder(&params).unwrap();
    dec.send_packet(&Packet::new(0, TimeBase::new(1, 1), bytes))
        .unwrap();
    for (i, want) in expect.iter().enumerate() {
        let CoreFrame::Video(v) = dec.receive_frame().unwrap() else {
            panic!("non-video");
        };
        assert_eq!(v.pts, Some(i as i64));
        assert_eq!(v.planes[0].data, want.image.as_bytes().unwrap());
    }
    assert!(matches!(dec.receive_frame(), Err(CoreError::NeedMore)));
    dec.flush().unwrap();
    assert!(matches!(dec.receive_frame(), Err(CoreError::Eof)));
}

// ---- acceptance 5: muxer ---------------------------------------------------

fn encoder_packets(
    ctx: &RuntimeContext,
    frames: &[GifImage],
    tb: TimeBase,
    durations: &[i64],
    options: &[(&str, &str)],
) -> (StreamInfo, Vec<Packet>) {
    let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    params.width = Some(frames[0].width);
    params.height = Some(frames[0].height);
    params.pixel_format = Some(to_core(frames[0].format));
    for (k, v) in options {
        params.options.insert(*k, *v);
    }
    let mut enc = ctx.codecs.first_encoder(&params).expect("first_encoder");
    let mut packets = Vec::new();
    let mut pts = 0i64;
    for (img, dur) in frames.iter().zip(durations) {
        let mut vf = VideoFrame::from(img);
        vf.pts = Some(pts);
        enc.send_frame(&CoreFrame::Video(vf)).unwrap();
        let mut pkt = enc.receive_packet().unwrap();
        // What the gateway stamps on per-picture packets.
        pkt.time_base = tb;
        pkt.pts = Some(pts);
        pkt.dts = Some(pts);
        pkt.duration = Some(*dur);
        pts += dur;
        packets.push(pkt);
        assert!(matches!(enc.receive_packet(), Err(CoreError::NeedMore)));
    }
    enc.flush().unwrap();
    assert!(matches!(enc.receive_packet(), Err(CoreError::Eof)));
    let stream = StreamInfo {
        index: 0,
        time_base: tb,
        duration: None,
        start_time: Some(0),
        params: enc.output_params().clone(),
    };
    (stream, packets)
}

#[test]
fn muxer_writes_a_still_layer1_reads_back_identically() {
    let ctx = ctx();
    // Four opaque entries: the framework palette side-channel carries
    // RGB only, and the wire pads tables to a power of two, so this is
    // the shape that round-trips exactly.
    let pal = Palette::new(vec![
        [9, 8, 7, 255],
        [1, 2, 3, 255],
        [0, 0, 0, 255],
        [5, 5, 5, 255],
    ]);
    let img = GifImage::from_indexed(3, 2, vec![0, 1, 2, 3, 1, 0], pal).unwrap();
    let (stream, packets) = encoder_packets(
        &ctx,
        std::slice::from_ref(&img),
        TimeBase::new(1, 1000),
        &[0],
        &[],
    );
    let file = mux(&stream, &packets);
    // One packet is written verbatim.
    assert_eq!(file, packets[0].data);
    assert_eq!(decode(&file).unwrap(), img);
    // And the registry reads it back as the same Pal8 frame.
    let (s, _, frames) = pump(&ctx, &file);
    assert_eq!(s.params.pixel_format, Some(PixelFormat::Pal8));
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].planes[0].data, img.as_bytes().unwrap());
}

#[test]
fn muxer_merges_encoder_packets_into_an_animation_with_delays_and_loop() {
    let ctx = ctx();
    // Three opaque 3×2 Rgba frames of ≤ 256 colours (lossless quantise).
    let frames: Vec<GifImage> = [[255u8, 0, 0], [0, 255, 0], [0, 0, 255]]
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut px = Vec::new();
            for k in 0..6u8 {
                if k as usize == i {
                    px.extend_from_slice(&[c[0], c[1], c[2], 255]);
                } else {
                    px.extend_from_slice(&[20, 30, 40, 255]);
                }
            }
            GifImage::from_rgba8(3, 2, px).unwrap()
        })
        .collect();
    // Milliseconds in, centiseconds out: 120 ms → 12, 55 ms → 6
    // (rounded to nearest), 1000 ms → 100.
    let (stream, packets) = encoder_packets(
        &ctx,
        &frames,
        TimeBase::new(1, 1000),
        &[120, 55, 1000],
        &[("loop_count", "7")],
    );
    let file = mux(&stream, &packets);

    let parsed = GifFile::parse(&file).unwrap();
    assert_eq!(parsed.loop_count(), Some(7));
    assert_eq!(parsed.graphic_rendering_block_count(), 3);
    let got = decode_all(&file).unwrap();
    assert_eq!(got.len(), 3);
    for (i, (f, want)) in got.iter().zip(&frames).enumerate() {
        assert_eq!(f.image.to_rgba8(), want.to_rgba8(), "frame {i}");
    }
    assert_eq!(
        got.iter().map(|f| f.delay).collect::<Vec<_>>(),
        vec![
            Some(Duration::from_millis(120)),
            Some(Duration::from_millis(60)),
            Some(Duration::from_millis(1000)),
        ]
    );
    // The registry round trip: demux(mux(frames)) == frames.
    let (s, pk, vfs) = pump(&ctx, &file);
    assert!(is_animation_stream(&s.params));
    assert_eq!(
        pk.iter().map(|p| (p.pts, p.duration)).collect::<Vec<_>>(),
        vec![
            (Some(0), Some(12)),
            (Some(12), Some(6)),
            (Some(18), Some(100))
        ]
    );
    for (i, (v, want)) in vfs.iter().zip(&frames).enumerate() {
        assert_eq!(v.planes[0].data, want.to_rgba8(), "registry frame {i}");
    }

    // `loop_count = -1` writes no NETSCAPE block; the default loops forever.
    let (stream, packets) = encoder_packets(
        &ctx,
        &frames,
        TimeBase::new(1, 100),
        &[1, 2, 3],
        &[("loop_count", "-1")],
    );
    assert_eq!(
        GifFile::parse(&mux(&stream, &packets))
            .unwrap()
            .loop_count(),
        None
    );
    let (stream, packets) = encoder_packets(&ctx, &frames, TimeBase::new(1, 100), &[1, 2, 3], &[]);
    assert_eq!(
        GifFile::parse(&mux(&stream, &packets))
            .unwrap()
            .loop_count(),
        Some(0)
    );
}

#[test]
fn demux_then_mux_then_demux_preserves_frames_timing_and_loop() {
    let ctx = ctx();
    let original = animation();
    let want = decode_all(&original).unwrap();
    let (stream, packets, _) = pump(&ctx, &original);
    let remuxed = mux(&stream, &packets);

    // Layer 1 agrees on every composited frame, delay and disposal.
    let got = decode_all(&remuxed).unwrap();
    assert_eq!(got.len(), want.len());
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        assert_eq!(g.image.planes, w.image.planes, "frame {i}");
        assert_eq!(g.delay, w.delay, "frame {i}");
        assert_eq!(g.disposal, w.disposal, "frame {i}");
    }
    let parsed = GifFile::parse(&remuxed).unwrap();
    assert_eq!(parsed.loop_count(), Some(3));
    assert_eq!(parsed.comments().count(), 2);
    // Frame 1 drew from the shared Global Color Table in the original
    // and still does: no Local Color Table was invented.
    assert_eq!(
        parsed
            .frames()
            .filter(|f| f.local_palette.is_some())
            .count(),
        1
    );

    // And the registry path sees the same packets and frames again.
    let (_, packets2, frames2) = pump(&ctx, &remuxed);
    assert_eq!(
        packets2
            .iter()
            .map(|p| (p.pts, p.duration))
            .collect::<Vec<_>>(),
        packets
            .iter()
            .map(|p| (p.pts, p.duration))
            .collect::<Vec<_>>()
    );
    for (i, (v, w)) in frames2.iter().zip(&want).enumerate() {
        assert_eq!(v.planes[0].data, w.image.as_bytes().unwrap(), "frame {i}");
    }
}

#[test]
fn muxer_gives_images_their_own_table_when_global_tables_differ() {
    let ctx = ctx();
    // Two stills with different Global Color Tables (the encoder's
    // output for two differently-coloured frames).
    let a = GifImage::from_rgb8(2, 1, vec![255, 0, 0, 0, 255, 0]).unwrap();
    let b = GifImage::from_rgb8(2, 1, vec![0, 0, 255, 7, 7, 7]).unwrap();
    let (stream, packets) = encoder_packets(&ctx, &[a.clone(), b.clone()], TIME_BASE, &[5, 5], &[]);
    let file = mux(&stream, &packets);
    let parsed = GifFile::parse(&file).unwrap();
    let frames: Vec<_> = parsed.frames().collect();
    assert!(
        frames[0].local_palette.is_none(),
        "frame 0 keeps the shared GCT"
    );
    assert!(
        frames[1].local_palette.is_some(),
        "frame 1 carries its table as an LCT"
    );
    let got = decode_all(&file).unwrap();
    assert_eq!(got[0].image.to_rgb8(), a.to_rgb8());
    assert_eq!(got[1].image.to_rgb8(), b.to_rgb8());
}

#[test]
fn muxer_rejects_wrong_streams_and_non_gif_packets() {
    let mut params = CodecParameters::video(CodecId::new("png"));
    params.width = Some(1);
    params.height = Some(1);
    let stream = StreamInfo {
        index: 0,
        time_base: TIME_BASE,
        duration: None,
        start_time: Some(0),
        params,
    };
    let sink: Box<dyn oxideav_core::WriteSeek> = Box::new(Cursor::new(Vec::new()));
    assert!(container::open_muxer(sink, std::slice::from_ref(&stream)).is_err());

    let mut gif = stream.clone();
    gif.params.codec_id = CodecId::new(CODEC_ID_STR);
    let sink: Box<dyn oxideav_core::WriteSeek> = Box::new(Cursor::new(Vec::new()));
    let mut m = container::open_muxer(sink, std::slice::from_ref(&gif)).unwrap();
    // write_packet before write_header, then a non-GIF payload.
    let pkt = Packet::new(0, TIME_BASE, still_pal8_opaque());
    assert!(m.write_packet(&pkt).is_err());
    m.write_header().unwrap();
    assert!(m
        .write_packet(&Packet::new(0, TIME_BASE, b"not a gif".to_vec()))
        .is_err());
    // No packets → the trailer refuses to write an empty file.
    assert!(m.write_trailer().is_err());
}

// ---- acceptance 6: register installs codec AND container ------------------

#[test]
fn register_installs_codec_and_container() {
    let mut ctx = RuntimeContext::new();
    oxideav_gif::__oxideav_entry(&mut ctx);
    let id = CodecId::new(CODEC_ID_STR);
    assert!(ctx.codecs.has_decoder(&id));
    assert!(ctx.codecs.has_encoder(&id));
    assert!(ctx.containers.demuxer_names().any(|n| n == "gif"));
    assert!(ctx.containers.muxer_names().any(|n| n == "gif"));
    assert_eq!(ctx.containers.container_for_extension("gif"), Some("gif"));
    // The registry opens the demuxer by name.
    let reader: Box<dyn oxideav_core::ReadSeek> = Box::new(Cursor::new(still_pal8_opaque()));
    let d = ctx
        .containers
        .open_demuxer("gif", reader, &ctx.codecs)
        .unwrap();
    assert_eq!(d.format_name(), "gif");
}

// ---- acceptance 7: hostile input never panics ------------------------------

#[test]
fn hostile_inputs_fail_cleanly() {
    let ctx = ctx();
    let good = animation();
    let mut cases: Vec<Vec<u8>> = vec![
        Vec::new(),
        b"GIF89a".to_vec(),
        b"GIF89a\x04\x00".to_vec(),
        good[..13].to_vec(),
        good[..good.len() / 2].to_vec(),
        good[..good.len() - 1].to_vec(), // no trailer
    ];
    // Absurd Logical Screen (65535 × 65535) with no body.
    let mut absurd = b"GIF89a\xff\xff\xff\xff\x00\x00\x00".to_vec();
    absurd.push(0x3B);
    cases.push(absurd.clone());
    // Absurd screen with a 1×1 image: `decode` succeeds within limits,
    // the demuxer must not allocate the canvas eagerly.
    let mut big = b"GIF89a\xff\xff\xff\xff\x80\x00\x00".to_vec();
    big.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
    big.extend_from_slice(&[
        0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3B,
    ]);
    cases.push(big);
    for (i, bytes) in cases.iter().enumerate() {
        let reader: Box<dyn oxideav_core::ReadSeek> = Box::new(Cursor::new(bytes.clone()));
        match container::open_demuxer(reader, &ctx.codecs) {
            Err(_) => {}
            Ok(mut d) => {
                // Whatever opened must pump without panicking.
                let params = d.streams()[0].params.clone();
                let mut dec = ctx.codecs.first_decoder(&params).unwrap();
                while let Ok(p) = d.next_packet() {
                    let _ = dec.send_packet(&p);
                    while dec.receive_frame().is_ok() {}
                }
                let _ = i;
            }
        }
    }
    // Zero-length and foreign packets into both decoder modes.
    let still = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    let mut anim = still.clone();
    anim.extradata = vec![container::EXTRADATA_VERSION, container::EXTRADATA_ANIMATION];
    for params in [&still, &anim] {
        let mut dec = oxideav_gif::make_decoder(params).unwrap();
        assert!(dec
            .send_packet(&Packet::new(0, TIME_BASE, Vec::new()))
            .is_err());
        assert!(dec
            .send_packet(&Packet::new(
                0,
                TIME_BASE,
                b"GIF89a\x01\x00\x01\x00".to_vec()
            ))
            .is_err());
        assert!(matches!(dec.receive_frame(), Err(CoreError::NeedMore)));
    }
    // Animation mode: a packet whose Logical Screen differs from the
    // canvas is refused, the canvas survives.
    let (stream, packets, _) = pump(&ctx, &good);
    let mut dec = ctx.codecs.first_decoder(&stream.params).unwrap();
    dec.send_packet(&packets[0]).unwrap();
    assert!(dec.receive_frame().is_ok());
    assert!(dec
        .send_packet(&Packet::new(0, TIME_BASE, still_pal8_opaque()))
        .is_err());
    dec.send_packet(&packets[1]).unwrap();
    assert!(dec.receive_frame().is_ok());
    // reset() forgets the canvas and clears the Eof latch.
    dec.flush().unwrap();
    assert!(matches!(dec.receive_frame(), Err(CoreError::Eof)));
    dec.reset().unwrap();
    assert!(matches!(dec.receive_frame(), Err(CoreError::NeedMore)));
    dec.send_packet(&packets[0]).unwrap();
    let CoreFrame::Video(v) = dec.receive_frame().unwrap() else {
        panic!()
    };
    assert_eq!(
        v.planes[0].data,
        decode_all(&good).unwrap()[0].image.as_bytes().unwrap()
    );

    // The encoder's drain contract.
    let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    params.width = Some(1);
    params.height = Some(1);
    params.pixel_format = Some(PixelFormat::Rgba);
    let mut enc = oxideav_gif::make_encoder(&params).unwrap();
    assert!(matches!(enc.receive_packet(), Err(CoreError::NeedMore)));
    enc.flush().unwrap();
    assert!(matches!(enc.receive_packet(), Err(CoreError::Eof)));

    // `encode_all` output (Layer 1's animation) opens and matches too.
    let frames = [
        Frame::new(
            GifImage::from_rgba8(2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255]).unwrap(),
            Some(Duration::from_millis(70)),
        ),
        Frame::new(
            GifImage::from_rgba8(2, 1, vec![7, 8, 9, 255, 4, 5, 6, 255]).unwrap(),
            None,
        ),
    ];
    let anim = encode_all(&frames, &EncodeOptions::default()).unwrap();
    let (_, pk, vfs) = pump(&ctx, &anim);
    assert_eq!(
        pk.iter().map(|p| p.duration).collect::<Vec<_>>(),
        vec![Some(7), Some(0)]
    );
    let want = decode_all(&anim).unwrap();
    for (v, w) in vfs.iter().zip(&want) {
        assert_eq!(v.planes[0].data, w.image.as_bytes().unwrap());
    }
}
