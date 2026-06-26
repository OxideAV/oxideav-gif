//! End-to-end coverage of the §23.c.vii presentation-timeline + seek
//! surface through the full pipeline: build an animation, serialise it
//! with the LZW encoder, decode it back, and confirm the time-domain
//! seek (`presentation_timeline` / `frame_index_at` /
//! `frame_index_at_global` / `compose_frame_at_global`) agrees with the
//! pixels the `compose` / `Playback` state machine produces.
//!
//! These exercise the real container + LZW bytes (not just an in-memory
//! `GifImage`), so a regression in encode, decode, or the seek maths is
//! caught at the boundary a player actually sees.

use core::time::Duration;
use oxideav_gif::{
    compose, compose_frame_at_global, decode, encode, AnimationBuilder, DisposalMethod, Playback,
    Rgb,
};

/// A 2×2 four-colour palette: index 0 black (background), 1 red, 2 green,
/// 3 blue.
fn palette() -> Vec<Rgb> {
    vec![
        Rgb::new(0, 0, 0),
        Rgb::new(0xFF, 0, 0),
        Rgb::new(0, 0xFF, 0),
        Rgb::new(0, 0, 0xFF),
    ]
}

/// Build → encode → decode a three-frame 2×2 looping animation with
/// distinct per-frame delays.
fn roundtrip_three_frame(loop_count: Option<u16>) -> oxideav_gif::GifImage {
    let mut b = AnimationBuilder::new(2, 2, palette());
    b = match loop_count {
        None => b,
        Some(0) => b.loop_forever(),
        Some(n) => b.loop_count(n),
    };
    let img = b
        // Frame 0: all red, 10 cs (100 ms), keep.
        .add_full_frame(vec![1; 4], 10, DisposalMethod::None)
        .unwrap()
        // Frame 1: all green, 20 cs (200 ms), keep.
        .add_full_frame(vec![2; 4], 20, DisposalMethod::None)
        .unwrap()
        // Frame 2: all blue, 70 cs (700 ms), keep.
        .add_full_frame(vec![3; 4], 70, DisposalMethod::None)
        .unwrap()
        .build()
        .unwrap();

    let bytes = encode(&img).unwrap();
    decode(&bytes).unwrap()
}

#[test]
fn presentation_timeline_survives_roundtrip() {
    let img = roundtrip_three_frame(None);
    let tl: Vec<_> = img.presentation_timeline().collect();
    assert_eq!(tl.len(), 3);
    assert_eq!(tl[0].start, Duration::ZERO);
    assert_eq!(tl[0].duration, Duration::from_millis(100));
    assert_eq!(tl[1].start, Duration::from_millis(100));
    assert_eq!(tl[1].duration, Duration::from_millis(200));
    assert_eq!(tl[2].start, Duration::from_millis(300));
    assert_eq!(tl[2].duration, Duration::from_millis(700));
    let last = tl.last().unwrap();
    assert_eq!(last.start + last.duration, img.single_pass_duration());
    assert_eq!(img.single_pass_duration(), Duration::from_millis(1000));
}

#[test]
fn frame_index_at_agrees_with_playback_iterator() {
    let img = roundtrip_three_frame(None);

    // Collect the per-frame delays the playback iterator surfaces and
    // rebuild the same cumulative timeline, then confirm frame_index_at
    // lands on the right frame inside each interval.
    let delays: Vec<Duration> = Playback::new(&img)
        .frames()
        .map(|r| r.unwrap().delay)
        .collect();
    assert_eq!(delays.len(), 3);

    let mut start = Duration::ZERO;
    for (idx, d) in delays.iter().enumerate() {
        // The midpoint of each non-empty interval resolves to that frame.
        let mid = start + *d / 2;
        assert_eq!(img.frame_index_at(mid), Some(idx));
        start += *d;
    }
    // Past the end of the single pass -> None.
    assert_eq!(img.frame_index_at(start), None);
}

#[test]
fn compose_frame_at_global_pixels_match_compose_output() {
    let img = roundtrip_three_frame(Some(2)); // 3 passes
    let frames = compose(&img).unwrap();
    assert_eq!(frames.len(), 3);
    let per_pass = img.single_pass_duration(); // 1000 ms

    // Sample the middle of every frame interval in every pass and confirm
    // the seek returns the same canvas compose produced for that frame.
    let interval_mids = [
        Duration::from_millis(50),  // frame 0 [0,100)
        Duration::from_millis(200), // frame 1 [100,300)
        Duration::from_millis(650), // frame 2 [300,1000)
    ];
    for pass in 0u64..3 {
        let pass_base = per_pass * (pass as u32);
        for (frame_idx, mid) in interval_mids.iter().enumerate() {
            let global = pass_base + *mid;
            let seek = compose_frame_at_global(&img, global).unwrap().unwrap();
            assert_eq!(seek.pass, pass, "pass at {global:?}");
            assert_eq!(seek.frame_index, frame_idx, "frame at {global:?}");
            assert_eq!(
                seek.canvas, frames[frame_idx].canvas,
                "pixels at {global:?}"
            );
        }
    }

    // After 3 passes (3000 ms) the finite run is exhausted.
    assert_eq!(compose_frame_at_global(&img, per_pass * 3).unwrap(), None);
}

#[test]
fn infinite_loop_seek_never_exhausts() {
    let img = roundtrip_three_frame(Some(0)); // loop forever
    let per_pass = img.single_pass_duration();
    // A far-future offset still resolves: pass 100, the first frame.
    let global = per_pass * 100 + Duration::from_millis(50);
    let seek = compose_frame_at_global(&img, global).unwrap().unwrap();
    assert_eq!(seek.pass, 100);
    assert_eq!(seek.frame_index, 0);
}

#[test]
fn frames_bounding_box_after_roundtrip() {
    // A frame placed in one corner of a larger screen leaves a margin.
    let img = AnimationBuilder::new(8, 8, palette())
        .add_placed_frame(1, 1, 3, 2, vec![1; 6], 10, DisposalMethod::None)
        .unwrap()
        .build()
        .unwrap();
    let bytes = encode(&img).unwrap();
    let decoded = decode(&bytes).unwrap();
    assert_eq!(decoded.frames_bounding_box(), Some((1, 1, 3, 2)));
    assert!(decoded.frames_inhabit_subregion());
}
