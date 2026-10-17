#![no_main]

//! Registry-encoder fuzz harness (round 477): arbitrary bytes become a
//! `VideoFrame` in one of the encoder's declared input layouts
//! (`ACCEPTED_PIXEL_FORMATS`, plus undeclared ones that must be refused
//! by type), with an optional palette-alpha record (any length, longer
//! than the palette included), stream and frame metadata blobs of
//! every kind (ICC / Exif with and without the `Exif\0\0` prefix / XMP
//! / `gif-comment` / kinds GIF cannot carry) and the `quality` /
//! `dither` / `interlace` options. The packet the encoder emits must
//! parse, decode as a `Pal8` picture of the requested geometry, demux
//! with the blobs the encoder was asked to write, and mux twice.
//!
//! Contract: every call returns to its caller. A `panic!`, slice OOB,
//! integer overflow in debug, or OOM abort is a finding; `Err` from
//! `send_frame` on hostile planes is fine. An `Ok` packet that does
//! not parse, decode, or carry its blobs is a finding too.

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use oxideav_core::{
    BlobKind, CodecId, CodecParameters, Error, Frame as CoreFrame, MetadataBlob, PixelFormat,
    RuntimeContext, VideoFrame, VideoPlane,
};
use oxideav_gif::container;
use oxideav_gif::{
    blobs_to_gif, comment_blob, effective_blobs, ACCEPTED_PIXEL_FORMATS, CODEC_ID_STR,
};

/// Layouts the encoder must refuse by type (never a panic).
const UNDECLARED: &[PixelFormat] = &[
    PixelFormat::Yuv420P,
    PixelFormat::Yuv444P,
    PixelFormat::Cmyk,
    PixelFormat::RgbF32Le,
    PixelFormat::Nv12,
    PixelFormat::Yuva420P,
];

const MAX_DIM: u8 = 24;

/// Cyclic byte source over the fuzz input (short inputs still fill
/// every plane).
struct Src<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Src<'a> {
    fn byte(&mut self) -> u8 {
        if self.data.is_empty() {
            return 0;
        }
        let b = self.data[self.pos % self.data.len()];
        self.pos += 1;
        b
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.byte()).collect()
    }
}

fn blob_from(src: &mut Src, selector: u8) -> Option<MetadataBlob> {
    let len = usize::from(src.byte()) % 40;
    let data = src.bytes(len);
    Some(match selector % 8 {
        0 => MetadataBlob::new(BlobKind::ICC, data),
        1 => MetadataBlob::new(BlobKind::EXIF, data),
        2 => {
            let mut v = b"Exif\0\0".to_vec();
            v.extend(data);
            MetadataBlob::new(BlobKind::EXIF, v)
        }
        3 => MetadataBlob::new(BlobKind::XMP, data).with_mime("application/rdf+xml"),
        4 => comment_blob(data),
        5 => MetadataBlob::new(BlobKind::IPTC, data),
        6 => MetadataBlob::new(BlobKind::custom("png-text"), data),
        _ => return None,
    })
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let mut src = Src { data, pos: 8 };
    let sel = data[0];
    let w = u32::from(data[1] % MAX_DIM) + 1;
    let h = u32::from(data[2] % MAX_DIM) + 1;
    let flags = data[3];
    let quality = data[4];
    let pal_len = usize::from(data[5]) + 1; // 1..=256
    let alpha_len = usize::from(data[6]); // 0..=255 — may exceed the palette
    let n_blobs = usize::from(data[7] % 6);

    let declared = usize::from(sel) % (ACCEPTED_PIXEL_FORMATS.len() + UNDECLARED.len());
    let (pf, expect_refusal) = if declared < ACCEPTED_PIXEL_FORMATS.len() {
        (ACCEPTED_PIXEL_FORMATS[declared], false)
    } else {
        (UNDECLARED[declared - ACCEPTED_PIXEL_FORMATS.len()], true)
    };

    // Planes: every declared layout stores full-resolution planes; the
    // fuzzer also shortens one plane now and then (flag bit 4) to drive
    // the validation path.
    let pad = if flags & 1 != 0 {
        usize::from(src.byte() % 9)
    } else {
        0
    };
    let short = flags & 16 != 0;
    let mut planes = Vec::new();
    let count = pf.plane_count();
    for i in 0..count {
        let row = pf.plane_row_bytes(i, w).unwrap_or(w as usize * 4);
        let stride = row + pad;
        let mut need = stride * h as usize;
        if short && i + 1 == count && need > 0 {
            need -= 1;
        }
        planes.push(VideoPlane {
            stride,
            data: src.bytes(need),
        });
    }
    let mut frame = VideoFrame {
        pts: Some(0),
        planes,
    };
    if pf == PixelFormat::Pal8 || flags & 2 != 0 {
        frame.set_palette(src.bytes(pal_len * 3));
        if flags & 4 != 0 {
            frame.set_palette_alpha(src.bytes(alpha_len));
        }
    }
    // Stream and frame blobs.
    let mut stream_blobs = Vec::new();
    let mut frame_blobs = Vec::new();
    for _ in 0..n_blobs {
        let s = src.byte();
        if let Some(b) = blob_from(&mut src, s) {
            if s & 0x80 != 0 {
                frame_blobs.push(b);
            } else {
                stream_blobs.push(b);
            }
        }
    }
    if !frame_blobs.is_empty() {
        frame.set_blobs(frame_blobs.clone());
    }

    let mut ctx = RuntimeContext::new();
    oxideav_gif::register(&mut ctx);
    let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    params.width = Some(w);
    params.height = Some(h);
    params.pixel_format = Some(pf);
    params.blobs = stream_blobs.clone();
    if flags & 8 != 0 {
        params
            .options
            .insert("quality", (quality % 101).to_string());
    }
    if flags & 32 != 0 {
        params.options.insert("dither", "floyd_steinberg");
    }
    if flags & 64 != 0 {
        params.options.insert("interlace", "true");
    }
    if flags & 128 != 0 {
        params.options.insert("embed_metadata", "false");
    }
    let Ok(mut enc) = ctx.codecs.first_encoder(&params) else {
        return;
    };
    assert_eq!(enc.output_params().pixel_format, Some(PixelFormat::Pal8));

    match enc.send_frame(&CoreFrame::Video(frame)) {
        Ok(()) => assert!(!expect_refusal, "{pf:?} must be refused"),
        Err(Error::Unsupported(_)) if expect_refusal => return,
        Err(Error::Unsupported(msg)) => panic!("declared {pf:?} refused: {msg}"),
        Err(_) => return, // hostile planes / palette: typed error, fine
    }
    let pkt = enc.receive_packet().expect("packet after Ok send_frame");

    // The packet is a complete file of the requested geometry.
    let file = oxideav_gif::parse(&pkt.data).expect("encoder packet parses");
    assert_eq!(
        (u32::from(file.screen_width), u32::from(file.screen_height)),
        (w, h)
    );
    let img = oxideav_gif::decode(&pkt.data).expect("encoder packet decodes");
    assert_eq!(img.format, oxideav_gif::PixelFormat::Pal8);
    assert_eq!((img.width, img.height), (w, h));

    // Through the registry decoder with the coded label.
    let mut dec = ctx
        .codecs
        .first_decoder(enc.output_params())
        .expect("decoder");
    dec.send_packet(&pkt).expect("decoder accepts the packet");
    let CoreFrame::Video(v) = dec.receive_frame().expect("one frame") else {
        panic!("non-video");
    };
    assert_eq!(v.planes[0].stride, w as usize);
    assert!(v.palette().is_some());
    if let Some(a) = v.palette_alpha() {
        assert!(a.len() <= v.palette().unwrap().len() / 3);
    }

    // The blobs the encoder was asked to write come back off the demuxer.
    let reader: Box<dyn oxideav_core::ReadSeek> = Box::new(Cursor::new(pkt.data.clone()));
    let demux = container::open_demuxer(reader, &ctx.codecs).expect("packet demuxes");
    let got = demux.streams()[0].params.blobs.clone();
    if flags & 128 != 0 {
        assert!(got.is_empty(), "embed_metadata=false writes nothing");
    } else {
        let (meta, comments) = blobs_to_gif(&effective_blobs(&stream_blobs, &frame_blobs));
        let find = |k: BlobKind| got.iter().find(|b| b.kind == k).map(|b| b.data.clone());
        assert_eq!(find(BlobKind::ICC), meta.icc);
        assert_eq!(find(BlobKind::EXIF), meta.exif);
        assert_eq!(find(BlobKind::XMP), meta.xmp);
        let got_comments: Vec<&[u8]> = got
            .iter()
            .filter(|b| b.kind == oxideav_gif::COMMENT_BLOB_KIND)
            .map(|b| b.data.as_slice())
            .collect();
        let want_comments: Vec<&[u8]> = comments
            .iter()
            .filter_map(|b| match b {
                oxideav_gif::Block::Comment(c) => Some(c.as_slice()),
                _ => None,
            })
            .collect();
        assert_eq!(got_comments, want_comments);
    }

    // Mux the packet twice (merge path, one copy of the metadata) and
    // once (verbatim / insertion path), with the stream's blobs.
    let stream = oxideav_core::StreamInfo {
        index: 0,
        time_base: pkt.time_base,
        duration: None,
        start_time: Some(0),
        params: enc.output_params().clone(),
    };
    for n in [1usize, 2] {
        let sink: Box<dyn oxideav_core::WriteSeek> = Box::new(Cursor::new(Vec::new()));
        let mut mux = container::open_muxer(sink, std::slice::from_ref(&stream)).expect("muxer");
        mux.write_header().unwrap();
        for _ in 0..n {
            mux.write_packet(&pkt).expect("encoder packet muxes");
        }
        mux.write_trailer().expect("merge of encoder packets");
    }
});
