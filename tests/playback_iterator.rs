//! Integration tests for the lazy [`Playback`] / [`LoopingFrameIter`]
//! API. These exercise the public `decode → encode → re-decode →
//! Playback::frames` pipeline end-to-end so any wiring breakage in
//! the consumer-facing surface surfaces here, not just in the unit
//! tests inside `playback.rs`.

use core::time::Duration;

use oxideav_gif::app_ext::LoopControl;
use oxideav_gif::{
    compose, decode, encode, Block, DisposalMethod, Frame, GifImage, GraphicControl, Playback, Rgb,
    Version,
};

/// Build a 4×4 image with one global palette of 4 colours and the
/// caller's blocks.
fn base_image(blocks: Vec<Block>) -> GifImage {
    GifImage {
        version: Version::Gif89a,
        screen_width: 4,
        screen_height: 4,
        color_resolution: 1,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(vec![
            Rgb::new(0, 0, 0),
            Rgb::new(0xFF, 0, 0),
            Rgb::new(0, 0xFF, 0),
            Rgb::new(0, 0, 0xFF),
        ]),
        blocks,
    }
}

fn small_frame(left: u16, top: u16, fill: u8, gce: Option<GraphicControl>) -> Frame {
    Frame {
        left,
        top,
        width: 2,
        height: 2,
        local_palette: None,
        palette_sorted: false,
        interlaced: false,
        indices: vec![fill; 4],
        graphic_control: gce,
    }
}

/// `Playback::frames` and the eager `compose()` produce the same
/// canvas+delay sequence. Verified through the full encode →
/// decode round-trip so this catches any byte-level regressions
/// introduced between rounds.
#[test]
fn playback_frames_match_eager_compose_via_roundtrip() {
    let img = base_image(vec![
        Block::Image(small_frame(
            0,
            0,
            1,
            Some(GraphicControl {
                disposal: DisposalMethod::Keep,
                user_input: false,
                transparent_index: None,
                delay_centis: 7,
            }),
        )),
        Block::Image(small_frame(
            0,
            0,
            3,
            Some(GraphicControl {
                disposal: DisposalMethod::RestorePrevious,
                user_input: false,
                transparent_index: None,
                delay_centis: 5,
            }),
        )),
        Block::Image(small_frame(2, 2, 2, None)),
    ]);

    let bytes = encode(&img).unwrap();
    let parsed = decode(&bytes).unwrap();

    let eager = compose(&parsed).unwrap();
    let lazy: Vec<_> = Playback::new(&parsed)
        .frames()
        .collect::<oxideav_gif::Result<Vec<_>>>()
        .unwrap();

    assert_eq!(eager.len(), lazy.len());
    for (e, l) in eager.iter().zip(lazy.iter()) {
        assert_eq!(e.canvas, l.canvas);
        // §23.c.vii centiseconds → milliseconds.
        assert_eq!(
            l.delay,
            Duration::from_millis(e.delay_centis as u64 * 10),
            "delay conversion is exact"
        );
    }
}

/// A NETSCAPE2.0 Application Extension carrying `loop_count = 3`
/// must yield 3 + 1 = 4 complete passes.
#[test]
fn looping_frames_honour_netscape_count_via_roundtrip() {
    let netscape = LoopControl {
        loop_count: Some(3),
        buffer_size: None,
    }
    .to_application();
    let img = base_image(vec![
        Block::Application(netscape),
        Block::Image(small_frame(0, 0, 1, None)),
        Block::Image(small_frame(2, 2, 2, None)),
    ]);

    let bytes = encode(&img).unwrap();
    let parsed = decode(&bytes).unwrap();

    let collected: Vec<_> = Playback::new(&parsed)
        .looping_frames()
        .collect::<oxideav_gif::Result<Vec<_>>>()
        .unwrap();
    // 2 image blocks per pass, 4 passes → 8 frames.
    assert_eq!(collected.len(), 8);
}

/// No NETSCAPE2.0 extension → exactly one pass through the image
/// blocks. Mirrors the de-facto convention that animated GIFs
/// without a loop directive play once.
#[test]
fn looping_frames_default_one_pass_via_roundtrip() {
    let img = base_image(vec![
        Block::Image(small_frame(0, 0, 1, None)),
        Block::Image(small_frame(2, 2, 2, None)),
    ]);
    let bytes = encode(&img).unwrap();
    let parsed = decode(&bytes).unwrap();
    let collected: Vec<_> = Playback::new(&parsed)
        .looping_frames()
        .collect::<oxideav_gif::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(collected.len(), 2);
}
