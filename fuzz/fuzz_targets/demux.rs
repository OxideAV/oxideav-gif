#![no_main]

//! Framework-path fuzz harness: the bytes are a file handed to the GIF
//! container demuxer, every packet it cuts goes through the registered
//! `gif` decoder (whole-stream or per-frame animation mode, as the
//! stream's `extradata` says), and the packets are muxed back.
//!
//! Contract: every call returns to its caller. A `panic!`, slice OOB,
//! integer overflow in debug, or OOM abort is a finding. `Err` on any
//! input is fine. The demuxer must never allocate the canvas eagerly,
//! so a hostile Logical Screen is cheap to open; the decoder is held to
//! a small pixel budget through `DecoderLimits` so a legal-but-huge
//! screen is refused instead of allocated.

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use oxideav_core::{DecoderLimits, Error, RuntimeContext};
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

    let mut params = stream.params.clone();
    let mut limits = DecoderLimits::default();
    limits.max_pixels_per_frame = MAX_PIXELS;
    limits.max_alloc_bytes_per_frame = MAX_PIXELS * 4;
    params.limits = limits;
    let Ok(mut dec) = ctx.codecs.first_decoder(&params) else {
        return;
    };

    let mut packets = Vec::new();
    loop {
        match demux.next_packet() {
            Ok(pkt) => {
                let _ = dec.send_packet(&pkt);
                loop {
                    match dec.receive_frame() {
                        Ok(_) => {}
                        Err(Error::NeedMore) | Err(Error::Eof) => break,
                        Err(_) => break,
                    }
                }
                packets.push(pkt);
            }
            Err(_) => break,
        }
    }
    let _ = dec.flush();
    while dec.receive_frame().is_ok() {}

    // Mux the packets back (the merged file must at least parse).
    let sink: Box<dyn oxideav_core::WriteSeek> = Box::new(Cursor::new(Vec::new()));
    if let Ok(mut mux) = container::open_muxer(sink, std::slice::from_ref(&stream)) {
        let _ = mux.write_header();
        for p in &packets {
            let _ = mux.write_packet(p);
        }
        let _ = mux.write_trailer();
    }
});
