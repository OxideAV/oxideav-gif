//! Container string metadata through the registry (round 483):
//! `Muxer::set_metadata` / `carries_metadata_key` on the GIF muxer
//! write `comment` pairs as §24 Comment Extensions and `loop_count` as
//! the NETSCAPE2.0 *Looping* block; `Demuxer::metadata` reads both
//! back. Every other key is dropped and reported as not carried.

#![cfg(feature = "registry")]

use std::io::{Cursor, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use oxideav_core::{
    CodecId, CodecParameters, Demuxer, Error as CoreError, Frame as CoreFrame, MetadataBlob, Muxer,
    Packet, PixelFormat, RuntimeContext, StreamInfo, TimeBase, VideoFrame,
};
use oxideav_gif::container;
use oxideav_gif::{CODEC_ID_STR, GifImage, comment_blob, comment_blob_kind, parse};

fn ctx() -> RuntimeContext {
    let mut ctx = RuntimeContext::new();
    oxideav_gif::register(&mut ctx);
    ctx
}

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
        Ok(())
    }
}

impl Seek for SharedBuf {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.lock().unwrap().seek(pos)
    }
}

fn open_mux(stream: &StreamInfo) -> (SharedBuf, Box<dyn Muxer>) {
    let buf = SharedBuf::default();
    let sink: Box<dyn oxideav_core::WriteSeek> = Box::new(buf.clone());
    let mux = container::open_muxer(sink, std::slice::from_ref(stream)).expect("open_muxer");
    (buf, mux)
}

/// Mux `packets` under `stream` with `meta` handed to `set_metadata`.
fn mux_with(stream: &StreamInfo, packets: &[Packet], meta: &[(String, String)]) -> Vec<u8> {
    let (buf, mut mux) = open_mux(stream);
    mux.set_metadata(meta).expect("set_metadata");
    mux.write_header().unwrap();
    for p in packets {
        mux.write_packet(p).unwrap();
    }
    mux.write_trailer().unwrap();
    buf.bytes()
}

fn open(ctx: &RuntimeContext, bytes: &[u8]) -> Box<dyn Demuxer> {
    let reader: Box<dyn oxideav_core::ReadSeek> = Box::new(Cursor::new(bytes.to_vec()));
    container::open_demuxer(reader, &ctx.codecs).expect("open_demuxer")
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn values<'a>(meta: &'a [(String, String)], key: &str) -> Vec<&'a str> {
    meta.iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .collect()
}

fn params(w: u32, h: u32, blobs: Vec<MetadataBlob>) -> CodecParameters {
    let mut p = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    p.width = Some(w);
    p.height = Some(h);
    p.pixel_format = Some(PixelFormat::Rgb24);
    p.blobs = blobs;
    p
}

fn rgb_frame(w: u32, h: u32, seed: u8) -> VideoFrame {
    let px: Vec<u8> = (0..w * h)
        .flat_map(|i| [seed, (i as u8).wrapping_mul(40), 7])
        .collect();
    VideoFrame::from(GifImage::from_rgb8(w, h, px).unwrap())
}

fn encoder_packets(
    ctx: &RuntimeContext,
    params: &CodecParameters,
    frames: usize,
) -> (StreamInfo, Vec<Packet>) {
    let mut enc = ctx.codecs.first_encoder(params).expect("first_encoder");
    let mut packets = Vec::new();
    for i in 0..frames {
        let mut vf = rgb_frame(4, 2, i as u8 * 50);
        vf.pts = Some(i as i64 * 10);
        enc.send_frame(&CoreFrame::Video(vf)).unwrap();
        let mut pkt = enc.receive_packet().unwrap();
        pkt.time_base = TimeBase::new(1, 100);
        pkt.duration = Some(10);
        packets.push(pkt);
    }
    enc.flush().unwrap();
    let stream = StreamInfo {
        index: 0,
        time_base: TimeBase::new(1, 100),
        duration: None,
        start_time: Some(0),
        params: enc.output_params().clone(),
    };
    (stream, packets)
}

#[test]
fn muxer_carries_comment_and_loop_count_only() {
    let ctx = ctx();
    let (stream, _) = encoder_packets(&ctx, &params(4, 2, Vec::new()), 1);
    let (_, mux) = open_mux(&stream);
    assert!(mux.carries_metadata_key("comment"));
    assert!(mux.carries_metadata_key("loop_count"));
    for k in ["title", "artist", "copyright", "date", "description", ""] {
        assert!(!mux.carries_metadata_key(k), "{k}");
    }
    assert_eq!(container::METADATA_KEYS, ["comment", "loop_count"]);
}

#[test]
fn still_comments_round_trip_in_order_other_keys_dropped() {
    let ctx = ctx();
    let (stream, packets) = encoder_packets(&ctx, &params(4, 2, Vec::new()), 1);
    let long = "x".repeat(700); // three §24.c.iii sub-blocks
    let meta = pairs(&[
        ("title", "not carried"),
        ("comment", "first"),
        ("artist", "nobody"),
        ("comment", "zweite — ünïcødé"),
        ("comment", ""),
        ("comment", &long),
    ]);
    let bytes = mux_with(&stream, &packets, &meta);
    assert_eq!(&bytes[..6], b"GIF89a", "§24.b: Comment needs 89a");
    let demux = open(&ctx, &bytes);
    let got = demux.metadata();
    assert_eq!(
        values(got, "comment"),
        ["first", "zweite — ünïcødé", long.as_str()],
        "every non-empty comment, in order"
    );
    assert!(values(got, "title").is_empty());
    assert!(values(got, "artist").is_empty());
    let file = parse(&bytes).unwrap();
    assert!(file.comments_in_recommended_position());
    // Pixels untouched by the rewrite.
    assert_eq!(
        oxideav_gif::decode(&bytes).unwrap().planes,
        oxideav_gif::decode(&packets[0].data).unwrap().planes
    );
}

#[test]
fn no_carried_key_leaves_a_single_packet_verbatim() {
    let ctx = ctx();
    let mut p = params(4, 2, Vec::new());
    p.options.insert("loop_count", "-1");
    let (stream, packets) = encoder_packets(&ctx, &p, 1);
    let bytes = mux_with(&stream, &packets, &pairs(&[("title", "t")]));
    assert_eq!(bytes, packets[0].data);
}

#[test]
fn animation_comments_and_loop_count_round_trip() {
    let ctx = ctx();
    let (stream, packets) = encoder_packets(&ctx, &params(4, 2, Vec::new()), 3);
    let meta = pairs(&[("comment", "anim"), ("loop_count", "3")]);
    let bytes = mux_with(&stream, &packets, &meta);
    let demux = open(&ctx, &bytes);
    assert_eq!(
        demux.metadata(),
        pairs(&[("loop_count", "3"), ("comment", "anim")])
    );
    assert_eq!(parse(&bytes).unwrap().frames().count(), 3);
}

#[test]
fn loop_count_pair_replaces_the_option_and_the_packets_block() {
    let ctx = ctx();
    // The encoder's default option writes loop forever (0) into each
    // packet; the pair wins.
    let (stream, packets) = encoder_packets(&ctx, &params(4, 2, Vec::new()), 2);
    let bytes = mux_with(&stream, &packets, &pairs(&[("loop_count", "65535")]));
    let file = parse(&bytes).unwrap();
    assert_eq!(file.loop_count(), Some(65535));
    let loops = file
        .application_extensions()
        .filter(|a| oxideav_gif::app_ext::LoopControl::from_application(a).is_some())
        .count();
    assert_eq!(loops, 1, "exactly one Looping block");

    // `-1` writes none.
    let bytes = mux_with(&stream, &packets, &pairs(&[("loop_count", "-1")]));
    assert_eq!(parse(&bytes).unwrap().loop_count(), None);
    assert!(values(open(&ctx, &bytes).metadata(), "loop_count").is_empty());

    // A single packet takes the pair too.
    let bytes = mux_with(&stream, &packets[..1], &pairs(&[("loop_count", "7")]));
    assert_eq!(parse(&bytes).unwrap().loop_count(), Some(7));
}

#[test]
fn bad_loop_count_is_invalid_data() {
    let ctx = ctx();
    let (stream, _) = encoder_packets(&ctx, &params(4, 2, Vec::new()), 1);
    for v in ["65536", "-2", "forever", ""] {
        let (_, mut mux) = open_mux(&stream);
        let err = mux.set_metadata(&pairs(&[("loop_count", v)])).unwrap_err();
        assert!(matches!(err, CoreError::InvalidData(_)), "{v}: {err:?}");
    }
}

#[test]
fn later_call_replaces_and_empty_clears() {
    let ctx = ctx();
    let mut p = params(4, 2, Vec::new());
    p.options.insert("loop_count", "-1");
    let (stream, packets) = encoder_packets(&ctx, &p, 1);
    let (buf, mut mux) = open_mux(&stream);
    mux.set_metadata(&pairs(&[("comment", "old")])).unwrap();
    mux.set_metadata(&pairs(&[("comment", "new")])).unwrap();
    mux.write_header().unwrap();
    mux.write_packet(&packets[0]).unwrap();
    mux.write_trailer().unwrap();
    assert_eq!(
        values(open(&ctx, &buf.bytes()).metadata(), "comment"),
        ["new"]
    );
    // After the trailer the set is refused.
    assert!(matches!(
        mux.set_metadata(&[]),
        Err(CoreError::InvalidData(_))
    ));

    let (buf, mut mux) = open_mux(&stream);
    mux.set_metadata(&pairs(&[("comment", "old")])).unwrap();
    mux.set_metadata(&[]).unwrap();
    mux.write_header().unwrap();
    mux.write_packet(&packets[0]).unwrap();
    mux.write_trailer().unwrap();
    assert_eq!(buf.bytes(), packets[0].data, "cleared: verbatim");
}

#[test]
fn comment_pair_and_matching_blob_write_one_comment() {
    let ctx = ctx();
    // The encoder writes the stream's `gif-comment` blobs into each
    // packet; the pair with the same text is not written again.
    let blobs = vec![comment_blob(b"shared".to_vec())];
    let (stream, packets) = encoder_packets(&ctx, &params(4, 2, blobs), 2);
    let meta = pairs(&[("comment", "shared"), ("comment", "extra")]);
    for n in [1, 2] {
        let bytes = mux_with(&stream, &packets[..n], &meta);
        let file = parse(&bytes).unwrap();
        let got: Vec<&[u8]> = file.comments().collect();
        assert_eq!(got.len(), 2, "{n} packet(s): {got:?}");
        assert!(got.contains(&b"shared".as_slice()));
        assert!(got.contains(&b"extra".as_slice()));
    }
}

/// Open → mux with `demux.metadata()` and the stream blobs → open: the
/// text pairs and the blobs come back identical, a non-UTF-8 comment
/// keeps its raw bytes, and nothing is duplicated.
#[test]
fn demux_remux_with_both_channels_is_identity() {
    let ctx = ctx();
    let blobs = vec![
        comment_blob(b"plain ascii".to_vec()),
        comment_blob(b"\xFF\xFEbinary\x00".to_vec()),
    ];
    let mut p = params(4, 2, blobs);
    p.options.insert("loop_count", "2");
    for frames in [1usize, 3] {
        let (stream, packets) = encoder_packets(&ctx, &p, frames);
        let source = mux_with(&stream, &packets, &[]);
        let demux = open(&ctx, &source);
        let meta = demux.metadata().to_vec();
        let src_stream = demux.streams()[0].clone();
        assert_eq!(values(&meta, "comment").len(), 2);

        // (a) the demuxed packets themselves.
        let mut d = open(&ctx, &source);
        let mut pkts = Vec::new();
        while let Ok(pk) = d.next_packet() {
            pkts.push(pk);
        }
        // (b) packets re-encoded with the stream's blobs riding along,
        // which already hold the comments.
        let (enc_stream, enc_pkts) = {
            let mut q = src_stream.params.clone();
            q.pixel_format = Some(PixelFormat::Rgb24);
            q.options.insert("loop_count", "2");
            encoder_packets(&ctx, &q, frames)
        };
        // (c) packets with no metadata at all, stream blobs only.
        let (bare_stream, bare_pkts) = {
            let mut q = src_stream.params.clone();
            q.pixel_format = Some(PixelFormat::Rgb24);
            q.options.insert("embed_metadata", "false");
            encoder_packets(&ctx, &q, frames)
        };
        let mut bare_stream = bare_stream;
        bare_stream.params.blobs = src_stream.params.blobs.clone();
        for (label, st, pk) in [
            ("demuxed", &src_stream, &pkts),
            ("re-encoded", &enc_stream, &enc_pkts),
            ("bare", &bare_stream, &bare_pkts),
        ] {
            let out = mux_with(st, pk, &meta);
            let back = open(&ctx, &out);
            assert_eq!(
                back.metadata(),
                meta.as_slice(),
                "{frames} frame(s), {label}"
            );
            let raw: Vec<&[u8]> = back.streams()[0]
                .params
                .blobs_of(comment_blob_kind())
                .map(|b| b.data.as_slice())
                .collect();
            assert_eq!(
                raw,
                [b"plain ascii".as_slice(), b"\xFF\xFEbinary\x00"],
                "{frames} frame(s), {label}: raw bytes survive"
            );
        }
    }
}
