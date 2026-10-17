//! Metadata blobs through the registry (round 477, core 0.1.40
//! `oxideav_core::blob`): the demuxer puts the ICC / Exif / XMP
//! Application Extensions and the §24 Comments on
//! `CodecParameters::blobs`; the encoder writes the stream's blobs —
//! overridden per kind by the frame's record — back as extensions; the
//! muxer keeps one copy per merged file and inserts stream blobs no
//! packet carries.

#![cfg(feature = "registry")]

use std::io::{Cursor, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use oxideav_core::{
    BlobKind, CodecId, CodecParameters, Error as CoreError, Frame as CoreFrame, MetadataBlob,
    Packet, PixelFormat, RuntimeContext, StreamInfo, TimeBase, VideoFrame,
};
use oxideav_gif::app_ext::{ExifMetadata, IccProfile, XmpPacket};
use oxideav_gif::container;
use oxideav_gif::{
    blobs_to_gif, comment_blob, comment_blob_kind, effective_blobs, encode_file, metadata_blobs,
    parse, Block, GifFile, GifFrameData, GifImage, Rgb, Version, CODEC_ID_STR, COMMENT_BLOB_KIND,
};

const ICC: &[u8] = b"\x00\x00\x00\x80acsp-fake-profile";
const XMP: &[u8] = b"<?xpacket begin=''?><x:xmpmeta/><?xpacket end='w'?>";
/// A minimal TIFF structure: little-endian mark, magic 42, IFD at 8,
/// zero entries.
const EXIF_TIFF: &[u8] = &[0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00];
const COMMENT_A: &[u8] = b"made by a test";
/// Not UTF-8 — the blob carries bytes, the `comment` metadata string is
/// lossy.
const COMMENT_B: &[u8] = b"\xFF\xFEbinary\x00";

fn ctx() -> RuntimeContext {
    let mut ctx = RuntimeContext::new();
    oxideav_gif::register(&mut ctx);
    ctx
}

fn exif_bytes_with_prefix() -> Vec<u8> {
    let mut v = b"Exif\0\0".to_vec();
    v.extend_from_slice(EXIF_TIFF);
    v
}

/// A 2×2 two-colour still with every metadata extension and two
/// comments, in the §24.e.ii leading position.
fn still_with_metadata(exif: Vec<u8>) -> Vec<u8> {
    let file = GifFile {
        version: Version::Gif89a,
        screen_width: 2,
        screen_height: 2,
        color_resolution: 1,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(vec![Rgb { r: 1, g: 2, b: 3 }, Rgb { r: 9, g: 8, b: 7 }]),
        blocks: vec![
            Block::Application(
                IccProfile {
                    bytes: ICC.to_vec(),
                }
                .to_application(),
            ),
            Block::Application(ExifMetadata::new(exif).to_application()),
            Block::Application(
                XmpPacket {
                    bytes: XMP.to_vec(),
                }
                .to_application(),
            ),
            Block::Comment(COMMENT_A.to_vec()),
            Block::Comment(COMMENT_B.to_vec()),
            Block::Image(GifFrameData {
                left: 0,
                top: 0,
                width: 2,
                height: 2,
                local_palette: None,
                palette_sorted: false,
                interlaced: false,
                indices: vec![0, 1, 1, 0],
                graphic_control: None,
            }),
        ],
    };
    encode_file(&file).unwrap()
}

fn open(ctx: &RuntimeContext, bytes: &[u8]) -> Box<dyn oxideav_core::Demuxer> {
    let reader: Box<dyn oxideav_core::ReadSeek> = Box::new(Cursor::new(bytes.to_vec()));
    container::open_demuxer(reader, &ctx.codecs).expect("open_demuxer")
}

fn kinds(blobs: &[MetadataBlob]) -> Vec<String> {
    blobs.iter().map(|b| b.kind.as_str().to_string()).collect()
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

fn mux(stream: &StreamInfo, packets: &[Packet]) -> Vec<u8> {
    let buf = SharedBuf::default();
    let sink: Box<dyn oxideav_core::WriteSeek> = Box::new(buf.clone());
    let mut mux = container::open_muxer(sink, std::slice::from_ref(stream)).expect("open_muxer");
    mux.write_header().unwrap();
    for p in packets {
        mux.write_packet(p).unwrap();
    }
    mux.write_trailer().unwrap();
    buf.bytes()
}

fn params_with_blobs(w: u32, h: u32, blobs: Vec<MetadataBlob>) -> CodecParameters {
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

/// Encode `frames` (with optional per-frame blobs) under `params`.
fn encoder_packets(
    ctx: &RuntimeContext,
    params: &CodecParameters,
    frames: Vec<VideoFrame>,
) -> (StreamInfo, Vec<Packet>) {
    let mut enc = ctx.codecs.first_encoder(params).expect("first_encoder");
    let mut packets = Vec::new();
    for (i, mut vf) in frames.into_iter().enumerate() {
        vf.pts = Some(i as i64 * 10);
        enc.send_frame(&CoreFrame::Video(vf)).unwrap();
        let mut pkt = enc.receive_packet().unwrap();
        pkt.time_base = TimeBase::new(1, 100);
        pkt.duration = Some(10);
        packets.push(pkt);
    }
    enc.flush().unwrap();
    assert!(matches!(enc.receive_packet(), Err(CoreError::Eof)));
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
fn demuxer_puts_icc_exif_xmp_and_comments_on_the_stream_blobs() {
    let ctx = ctx();
    let bytes = still_with_metadata(EXIF_TIFF.to_vec());
    let demux = open(&ctx, &bytes);
    let p = &demux.streams()[0].params;
    assert_eq!(
        kinds(&p.blobs),
        ["icc", "exif", "xmp", COMMENT_BLOB_KIND, COMMENT_BLOB_KIND]
    );
    assert_eq!(p.blob(&BlobKind::ICC).unwrap().data, ICC);
    assert_eq!(p.blob(&BlobKind::EXIF).unwrap().data, EXIF_TIFF);
    assert_eq!(p.blob(&BlobKind::XMP).unwrap().data, XMP);
    let comments: Vec<&[u8]> = p
        .blobs_of(comment_blob_kind())
        .map(|b| b.data.as_slice())
        .collect();
    assert_eq!(comments, [COMMENT_A, COMMENT_B], "raw bytes, in order");
    assert!(p.blobs.iter().all(|b| b.mime.is_none()));
    // The string metadata is still there (lossy for the binary one).
    let meta = demux.metadata();
    assert_eq!(
        meta.iter().filter(|(k, _)| k == "comment").count(),
        2,
        "comment metadata pairs"
    );
    // Same list as the Layer 1 helper on the parsed file.
    assert_eq!(metadata_blobs(&parse(&bytes).unwrap()), p.blobs);
}

#[test]
fn demuxer_strips_the_app1_prefix_from_exif() {
    let ctx = ctx();
    let bytes = still_with_metadata(exif_bytes_with_prefix());
    let demux = open(&ctx, &bytes);
    let p = &demux.streams()[0].params;
    assert_eq!(
        p.blob(&BlobKind::EXIF).unwrap().data,
        EXIF_TIFF,
        "payload starts at the byte-order mark"
    );
    // Layer 1 keeps the wire bytes.
    assert_eq!(
        parse(&bytes).unwrap().exif(),
        Some(exif_bytes_with_prefix().as_slice())
    );
}

#[test]
fn file_without_metadata_has_no_blobs() {
    let ctx = ctx();
    let bytes = oxideav_gif::encode(
        &GifImage::from_rgb8(1, 1, vec![1, 2, 3]).unwrap(),
        &oxideav_gif::EncodeOptions::default(),
    )
    .unwrap();
    let demux = open(&ctx, &bytes);
    assert!(demux.streams()[0].params.blobs.is_empty());
}

#[test]
fn animation_stream_carries_the_leading_metadata_once() {
    let ctx = ctx();
    let mut file = parse(&still_with_metadata(EXIF_TIFF.to_vec())).unwrap();
    // Two frames → animation packetisation.
    let image = file.blocks.last().unwrap().clone();
    file.blocks.push(image);
    let bytes = encode_file(&file).unwrap();
    let demux = open(&ctx, &bytes);
    let p = &demux.streams()[0].params;
    assert!(container::is_animation_stream(p));
    assert_eq!(
        kinds(&p.blobs),
        ["icc", "exif", "xmp", COMMENT_BLOB_KIND, COMMENT_BLOB_KIND]
    );
}

#[test]
fn encoder_writes_stream_blobs_as_extensions_and_comments() {
    let ctx = ctx();
    let blobs = vec![
        MetadataBlob::new(BlobKind::ICC, ICC.to_vec()),
        MetadataBlob::new(BlobKind::EXIF, EXIF_TIFF.to_vec()),
        MetadataBlob::new(BlobKind::XMP, XMP.to_vec()).with_mime("application/rdf+xml"),
        comment_blob(COMMENT_A.to_vec()),
        comment_blob(COMMENT_B.to_vec()),
        // Kinds GIF has no carriage for are dropped, not refused.
        MetadataBlob::new(BlobKind::IPTC, vec![0x1C, 0x02]),
        MetadataBlob::new(BlobKind::COVER_ART, vec![0xFF, 0xD8]).with_mime("image/jpeg"),
        MetadataBlob::new(BlobKind::custom("png-text"), b"k\0v".to_vec()),
    ];
    let params = params_with_blobs(3, 2, blobs);
    let (_, packets) = encoder_packets(&ctx, &params, vec![rgb_frame(3, 2, 1)]);
    let file = parse(&packets[0].data).unwrap();
    assert_eq!(file.icc_profile(), Some(ICC));
    assert_eq!(file.exif(), Some(EXIF_TIFF));
    assert_eq!(file.xmp_packet(), Some(XMP));
    assert_eq!(file.comments().collect::<Vec<_>>(), [COMMENT_A, COMMENT_B]);
    assert!(file.comments_in_recommended_position());
    assert_eq!(
        file.application_extensions().count(),
        3,
        "only the three metadata extensions"
    );
    // What the registry demuxer reads back equals what was written.
    let demux = open(&ctx, &packets[0].data);
    let back = &demux.streams()[0].params.blobs;
    assert_eq!(
        kinds(back),
        ["icc", "exif", "xmp", COMMENT_BLOB_KIND, COMMENT_BLOB_KIND]
    );
    assert_eq!(back[0].data, ICC);
    assert_eq!(back[2].data, XMP);
}

#[test]
fn encoder_strips_an_app1_prefixed_exif_blob() {
    let ctx = ctx();
    let params = params_with_blobs(
        2,
        1,
        vec![MetadataBlob::new(BlobKind::EXIF, exif_bytes_with_prefix())],
    );
    let (_, packets) = encoder_packets(&ctx, &params, vec![rgb_frame(2, 1, 1)]);
    assert_eq!(parse(&packets[0].data).unwrap().exif(), Some(EXIF_TIFF));
}

#[test]
fn frame_blobs_override_stream_blobs_of_the_same_kind() {
    let ctx = ctx();
    let stream_blobs = vec![
        MetadataBlob::new(BlobKind::ICC, b"stream-icc".to_vec()),
        MetadataBlob::new(BlobKind::XMP, XMP.to_vec()),
        comment_blob(b"stream comment 1".to_vec()),
        comment_blob(b"stream comment 2".to_vec()),
    ];
    let params = params_with_blobs(2, 1, stream_blobs.clone());
    let mut with_frame_blobs = rgb_frame(2, 1, 1);
    with_frame_blobs.set_blobs(vec![
        MetadataBlob::new(BlobKind::ICC, b"frame-icc".to_vec()),
        comment_blob(b"frame comment".to_vec()),
    ]);
    let plain = rgb_frame(2, 1, 2);
    let (_, packets) = encoder_packets(&ctx, &params, vec![with_frame_blobs, plain]);

    let first = parse(&packets[0].data).unwrap();
    assert_eq!(first.icc_profile(), Some(&b"frame-icc"[..]));
    assert_eq!(
        first.xmp_packet(),
        Some(XMP),
        "kind the frame lacks: stream"
    );
    assert_eq!(
        first.comments().collect::<Vec<_>>(),
        [&b"frame comment"[..]],
        "the frame's comment set replaces the stream's"
    );
    let second = parse(&packets[1].data).unwrap();
    assert_eq!(second.icc_profile(), Some(&b"stream-icc"[..]));
    assert_eq!(second.comments().count(), 2);

    // The pure function behind it.
    let eff = effective_blobs(
        &stream_blobs,
        &[MetadataBlob::new(BlobKind::ICC, b"f".to_vec())],
    );
    assert_eq!(
        kinds(&eff),
        ["xmp", COMMENT_BLOB_KIND, COMMENT_BLOB_KIND, "icc"]
    );
    assert_eq!(eff[3].data, b"f");
    let (meta, comments) = blobs_to_gif(&eff);
    assert_eq!(meta.icc.as_deref(), Some(&b"f"[..]));
    assert_eq!(meta.xmp.as_deref(), Some(XMP));
    assert_eq!(meta.exif, None);
    assert_eq!(comments.len(), 2);
}

#[test]
fn embed_metadata_off_writes_no_blobs() {
    let ctx = ctx();
    let mut params = params_with_blobs(
        2,
        1,
        vec![
            MetadataBlob::new(BlobKind::ICC, ICC.to_vec()),
            comment_blob(COMMENT_A.to_vec()),
        ],
    );
    params.options.insert("embed_metadata", "false");
    let (_, packets) = encoder_packets(&ctx, &params, vec![rgb_frame(2, 1, 1)]);
    let file = parse(&packets[0].data).unwrap();
    assert_eq!(file.icc_profile(), None);
    assert_eq!(file.comments().count(), 0);
    assert_eq!(file.version, Version::Gif87a, "no extension at all");
}

#[test]
fn muxer_keeps_one_copy_of_the_metadata_across_merged_packets() {
    let ctx = ctx();
    let blobs = vec![
        MetadataBlob::new(BlobKind::ICC, ICC.to_vec()),
        MetadataBlob::new(BlobKind::XMP, XMP.to_vec()),
        comment_blob(COMMENT_A.to_vec()),
    ];
    let params = params_with_blobs(2, 1, blobs.clone());
    let frames = vec![rgb_frame(2, 1, 1), rgb_frame(2, 1, 2), rgb_frame(2, 1, 3)];
    let (stream, packets) = encoder_packets(&ctx, &params, frames);
    assert_eq!(stream.params.blobs, blobs, "output params keep the blobs");
    for p in &packets {
        assert_eq!(parse(&p.data).unwrap().icc_profile(), Some(ICC));
    }
    let merged = mux(&stream, &packets);
    let file = parse(&merged).unwrap();
    assert_eq!(file.application_extensions().count(), 3, "ICC + XMP + loop");
    assert_eq!(file.icc_profile(), Some(ICC));
    assert_eq!(file.xmp_packet(), Some(XMP));
    assert_eq!(file.comments().collect::<Vec<_>>(), [COMMENT_A]);
    assert_eq!(file.frames().count(), 3);
    // Round trip: the demuxer reads the same blobs off the merged file.
    let demux = open(&ctx, &merged);
    assert_eq!(demux.streams()[0].params.blobs, blobs);
}

#[test]
fn muxer_inserts_stream_blobs_that_no_packet_carries() {
    let ctx = ctx();
    // Packets written without metadata (embed_metadata off), but the
    // stream the muxer is opened with declares blobs.
    let mut params = params_with_blobs(2, 1, Vec::new());
    params.options.insert("embed_metadata", "false");
    let frames = vec![rgb_frame(2, 1, 1), rgb_frame(2, 1, 2)];
    let (mut stream, packets) = encoder_packets(&ctx, &params, frames);
    assert!(parse(&packets[0].data).unwrap().icc_profile().is_none());
    stream.params.blobs = vec![
        MetadataBlob::new(BlobKind::ICC, ICC.to_vec()),
        MetadataBlob::new(BlobKind::EXIF, exif_bytes_with_prefix()),
        comment_blob(COMMENT_B.to_vec()),
    ];
    // Several packets.
    let merged = mux(&stream, &packets);
    let file = parse(&merged).unwrap();
    assert_eq!(file.icc_profile(), Some(ICC));
    assert_eq!(
        file.exif(),
        Some(EXIF_TIFF),
        "prefix never reaches the wire"
    );
    assert_eq!(file.comments().collect::<Vec<_>>(), [COMMENT_B]);
    assert_eq!(file.frames().count(), 2);
    assert!(file.comments_in_recommended_position());

    // One packet: re-serialised with the extensions in front, same pixels.
    let single = mux(&stream, &packets[..1]);
    assert_ne!(single, packets[0].data, "no longer verbatim");
    let file = parse(&single).unwrap();
    assert_eq!(file.icc_profile(), Some(ICC));
    assert_eq!(file.comments().collect::<Vec<_>>(), [COMMENT_B]);
    assert_eq!(
        oxideav_gif::decode(&single).unwrap().planes,
        oxideav_gif::decode(&packets[0].data).unwrap().planes
    );

    // A packet that already carries the extension is written verbatim.
    let mut params = params_with_blobs(2, 1, stream.params.blobs.clone());
    params.options.insert("loop_count", "-1");
    let (stream2, packets2) = encoder_packets(&ctx, &params, vec![rgb_frame(2, 1, 1)]);
    assert_eq!(mux(&stream2, &packets2), packets2[0].data);
}

#[test]
fn demux_encode_mux_demux_preserves_blobs() {
    let ctx = ctx();
    let bytes = still_with_metadata(EXIF_TIFF.to_vec());
    let demux = open(&ctx, &bytes);
    let stream = demux.streams()[0].clone();
    let want = stream.params.blobs.clone();
    assert_eq!(want.len(), 5);
    // Re-encode the picture through the registry with the stream's blobs.
    let mut params = stream.params.clone();
    params.pixel_format = Some(PixelFormat::Rgb24);
    let img = oxideav_gif::decode(&bytes).unwrap();
    let rgb = GifImage::from_rgb8(img.width, img.height, img.to_rgb8()).unwrap();
    let (out_stream, packets) = encoder_packets(&ctx, &params, vec![VideoFrame::from(rgb)]);
    let merged = mux(&out_stream, &packets);
    let demux = open(&ctx, &merged);
    assert_eq!(demux.streams()[0].params.blobs, want);
}
