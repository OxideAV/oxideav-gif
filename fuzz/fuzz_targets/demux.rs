#![no_main]

//! Framework-path fuzz harness: the bytes are a file handed to the GIF
//! container demuxer, every packet it cuts goes through the registered
//! `gif` decoder (whole-stream or per-frame animation mode, as the
//! stream's `extradata` says), and the packets are muxed back.
//!
//! Round 477 additions: the stream's metadata blobs (ICC / Exif / XMP
//! Application Extensions, `gif-comment` per Comment Extension) are
//! read off the demuxer and must agree with the Layer 1 parse; every
//! decoded still carries a well-formed palette-alpha record (one byte
//! per entry at most, `0` only where the Layer 1 palette is
//! transparent); the first decoded frame is re-encoded through the
//! registry encoder with the stream's blobs, and that packet must
//! parse and demux with the same blobs; the packets after the first
//! (which lack the file's leading extensions) are muxed under the
//! stream's blobs so the muxer's insertion path runs.
//!
//! Contract: every call returns to its caller. A `panic!`, slice OOB,
//! integer overflow in debug, or OOM abort is a finding. `Err` on any
//! input is fine. The demuxer must never allocate the canvas eagerly,
//! so a hostile Logical Screen is cheap to open; the decoder is held to
//! a small pixel budget through `DecoderLimits` so a legal-but-huge
//! screen is refused instead of allocated.

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use oxideav_core::{DecoderLimits, Error, Frame as CoreFrame, PixelFormat, RuntimeContext};
use oxideav_gif::container;

const MAX_PIXELS: u64 = 1 << 20; // 1 Mpx canvas budget per frame

fuzz_target!(|data: &[u8]| {
    let mut ctx = RuntimeContext::new();
    oxideav_gif::register(&mut ctx);

    let reader: Box<dyn oxideav_core::ReadSeek> = Box::new(Cursor::new(data.to_vec()));
    let Ok(mut demux) = container::open_demuxer(reader, &ctx.codecs) else {
        return;
    };
    let stream = demux.streams()[0].clone();
    let _ = demux.metadata();
    let _ = demux.duration_micros();

    // Blobs: what the demuxer declares equals the Layer 1 view of the
    // same file whenever the strict parser accepts it.
    let blobs = stream.params.blobs.clone();
    if let Ok(file) = oxideav_gif::parse(data) {
        assert_eq!(oxideav_gif::metadata_blobs(&file), blobs);
    }
    for b in &blobs {
        assert!(
            b.kind == oxideav_core::BlobKind::ICC
                || b.kind == oxideav_core::BlobKind::EXIF
                || b.kind == oxideav_core::BlobKind::XMP
                || b.kind == oxideav_gif::COMMENT_BLOB_KIND
        );
    }

    let mut params = stream.params.clone();
    let mut limits = DecoderLimits::default();
    limits.max_pixels_per_frame = MAX_PIXELS;
    limits.max_alloc_bytes_per_frame = MAX_PIXELS * 4;
    params.limits = limits;
    let Ok(mut dec) = ctx.codecs.first_decoder(&params) else {
        return;
    };

    let still = !container::is_animation_stream(&stream.params);
    let mut packets = Vec::new();
    let mut first_frame = None;
    while let Ok(pkt) = demux.next_packet() {
        let _ = dec.send_packet(&pkt);
        loop {
            match dec.receive_frame() {
                Ok(CoreFrame::Video(v)) => {
                    if still {
                        // The palette-alpha record is well formed and
                        // matches the Layer 1 palette.
                        if let Some(a) = v.palette_alpha() {
                            let pal = v.palette().expect("record implies a palette");
                            assert!(a.len() <= pal.len() / 3);
                            assert!(a.iter().any(|&x| x != 255), "record only when needed");
                        }
                        if let Ok(img) = oxideav_gif::decode_with(
                            &pkt.data,
                            &oxideav_gif::DecodeOptions::default(),
                        ) {
                            if let Some(pal) = &img.palette {
                                for (i, e) in pal.entries.iter().enumerate() {
                                    assert_eq!(v.palette_rgba(i as u8), Some(*e));
                                }
                            }
                        }
                    }
                    if first_frame.is_none() {
                        first_frame = Some(v);
                    }
                }
                Ok(_) => {}
                Err(Error::NeedMore) | Err(Error::Eof) => break,
                Err(_) => break,
            }
        }
        packets.push(pkt);
    }
    let _ = dec.flush();
    while dec.receive_frame().is_ok() {}

    // Re-encode the first decoded frame with the stream's blobs: a
    // packet the encoder accepts must parse and demux with those blobs.
    if let Some(v) = first_frame {
        let mut eparams = stream.params.clone();
        eparams.pixel_format = Some(if still {
            stream.params.pixel_format.unwrap_or(PixelFormat::Rgba)
        } else {
            PixelFormat::Rgba
        });
        if let Ok(mut enc) = ctx.codecs.first_encoder(&eparams) {
            if enc.send_frame(&CoreFrame::Video(v)).is_ok() {
                let pkt = enc.receive_packet().expect("packet after Ok send_frame");
                let file = oxideav_gif::parse(&pkt.data).expect("re-encoded packet parses");
                let reader: Box<dyn oxideav_core::ReadSeek> =
                    Box::new(Cursor::new(pkt.data.clone()));
                let d = container::open_demuxer(reader, &ctx.codecs)
                    .expect("re-encoded packet demuxes");
                let back = &d.streams()[0].params.blobs;
                let (meta, comments) = oxideav_gif::blobs_to_gif(&blobs);
                assert_eq!(file.icc_profile().map(<[u8]>::to_vec), meta.icc);
                assert_eq!(file.xmp_packet().map(<[u8]>::to_vec), meta.xmp);
                assert_eq!(file.comments().count(), comments.len());
                assert_eq!(
                    back.iter()
                        .filter(|b| b.kind == oxideav_core::BlobKind::ICC)
                        .count(),
                    usize::from(meta.icc.is_some())
                );
            }
        }
    }

    // Mux the packets back (the merged file must at least parse), then
    // the tail packets alone (no leading extensions → the stream's
    // blobs are inserted).
    let sink: Box<dyn oxideav_core::WriteSeek> = Box::new(Cursor::new(Vec::new()));
    if let Ok(mut mux) = container::open_muxer(sink, std::slice::from_ref(&stream)) {
        let _ = mux.write_header();
        for p in &packets {
            let _ = mux.write_packet(p);
        }
        let _ = mux.write_trailer();
    }
    if packets.len() > 1 {
        let sink: Box<dyn oxideav_core::WriteSeek> = Box::new(Cursor::new(Vec::new()));
        if let Ok(mut mux) = container::open_muxer(sink, std::slice::from_ref(&stream)) {
            let _ = mux.write_header();
            for p in &packets[1..] {
                let _ = mux.write_packet(p);
            }
            let _ = mux.write_trailer();
        }
    }
});
