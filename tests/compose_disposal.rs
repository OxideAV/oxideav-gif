//! End-to-end disposal-method tests: build a multi-frame GIF byte
//! stream with the public encoder, run it through `decode` + `compose`,
//! then assert the final canvas state per GIF89a §23.
//!
//! Each test exercises one of the four well-defined disposal-method
//! values (§23.c.iv values 0–3). The fixture-construction path is
//! deliberately the public API so we exercise the same parser the
//! field will hit.

use oxideav_gif::{
    compose, decode, encode, Block, DisposalMethod, Frame, GifImage, GraphicControl, Rgb, Version,
};

/// 4-entry palette with the colours used by the disposal tests.
fn palette() -> Vec<Rgb> {
    vec![
        Rgb::new(0, 0, 0),    // 0 - black (the test's background)
        Rgb::new(0xFF, 0, 0), // 1 - red
        Rgb::new(0, 0xFF, 0), // 2 - green
        Rgb::new(0, 0, 0xFF), // 3 - blue
    ]
}

fn frame(left: u16, top: u16, w: u16, h: u16, fill: u8, gce: Option<GraphicControl>) -> Frame {
    Frame {
        left,
        top,
        width: w,
        height: h,
        local_palette: None,
        palette_sorted: false,
        interlaced: false,
        indices: vec![fill; (w as usize) * (h as usize)],
        graphic_control: gce,
    }
}

fn image_with(blocks: Vec<Block>) -> GifImage {
    GifImage {
        version: Version::Gif89a,
        screen_width: 4,
        screen_height: 4,
        color_resolution: 1,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(palette()),
        blocks,
    }
}

fn px(canvas: &oxideav_gif::RgbaCanvas, x: u16, y: u16) -> [u8; 4] {
    let off = ((y as usize) * (canvas.width as usize) + (x as usize)) * 4;
    [
        canvas.pixels[off],
        canvas.pixels[off + 1],
        canvas.pixels[off + 2],
        canvas.pixels[off + 3],
    ]
}

/// Build a 2-frame stream where frame 1's disposal is `disposal`,
/// then encode + decode + compose it and return the composed frames.
fn roundtrip(disposal: DisposalMethod) -> Vec<oxideav_gif::ComposedFrame> {
    let f1 = frame(
        0,
        0,
        2,
        2,
        1, // red
        Some(GraphicControl {
            disposal,
            user_input: false,
            transparent_index: None,
            delay_centis: 7,
        }),
    );
    let f2 = frame(2, 2, 2, 2, 2, None); // green
    let img = image_with(vec![Block::Image(f1), Block::Image(f2)]);
    let bytes = encode(&img).expect("encode");
    let decoded = decode(&bytes).expect("decode");
    compose(&decoded).expect("compose")
}

/// §23.c.iv value 0 — "No disposal specified": leave previous frame's
/// pixels in place. Frame 2's green quadrant is drawn alongside the
/// surviving red quadrant.
#[test]
fn disposal_0_no_disposal_specified() {
    let frames = roundtrip(DisposalMethod::None);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].delay_centis, 7);
    let final_canvas = &frames[1].canvas;
    assert_eq!(px(final_canvas, 0, 0), [0xFF, 0, 0, 0xFF], "red survives");
    assert_eq!(px(final_canvas, 1, 1), [0xFF, 0, 0, 0xFF], "red survives");
    assert_eq!(px(final_canvas, 2, 2), [0, 0xFF, 0, 0xFF], "green drawn");
    assert_eq!(px(final_canvas, 3, 3), [0, 0xFF, 0, 0xFF], "green drawn");
}

/// §23.c.iv value 1 — "Do not dispose": same visible result as 0,
/// stronger semantic intent.
#[test]
fn disposal_1_do_not_dispose() {
    let frames = roundtrip(DisposalMethod::Keep);
    let final_canvas = &frames[1].canvas;
    assert_eq!(px(final_canvas, 0, 0), [0xFF, 0, 0, 0xFF]);
    assert_eq!(px(final_canvas, 2, 2), [0, 0xFF, 0, 0xFF]);
}

/// §23.c.iv value 2 — "Restore to background color": the rectangle
/// covered by the disposing frame must be wiped to the LSD background
/// colour (palette[0] = black, alpha 0xFF) before the next frame.
#[test]
fn disposal_2_restore_to_background_color() {
    let frames = roundtrip(DisposalMethod::RestoreBackground);
    // Frame-1 snapshot still red.
    assert_eq!(px(&frames[0].canvas, 0, 0), [0xFF, 0, 0, 0xFF]);
    // Frame-2 final state — red rect was cleared to background black.
    let f2 = &frames[1].canvas;
    assert_eq!(px(f2, 0, 0), [0, 0, 0, 0xFF], "wiped to bg");
    assert_eq!(px(f2, 1, 1), [0, 0, 0, 0xFF], "wiped to bg");
    assert_eq!(px(f2, 2, 2), [0, 0xFF, 0, 0xFF], "green drawn");
}

/// §23.c.iv value 3 — "Restore to previous": the canvas reverts to
/// the state visible immediately before this frame rendered, then the
/// next frame draws on top of that prior state.
///
/// Three frames so we can prove a real revert happened:
///   F1: red 2×2 at (0,0), Keep      → baseline visible state
///   F2: blue 2×2 at (0,0), RestorePrevious
///   F3: green 2×2 at (2,2), None
/// After F2's disposal the canvas matches the post-F1 state (red
/// visible, no blue), and F3 then paints green into its quadrant.
#[test]
fn disposal_3_restore_to_previous() {
    let img = image_with(vec![
        Block::Image(frame(
            0,
            0,
            2,
            2,
            1, // red
            Some(GraphicControl {
                disposal: DisposalMethod::Keep,
                user_input: false,
                transparent_index: None,
                delay_centis: 0,
            }),
        )),
        Block::Image(frame(
            0,
            0,
            2,
            2,
            3, // blue
            Some(GraphicControl {
                disposal: DisposalMethod::RestorePrevious,
                user_input: false,
                transparent_index: None,
                delay_centis: 0,
            }),
        )),
        Block::Image(frame(2, 2, 2, 2, 2, None)), // green
    ]);
    let bytes = encode(&img).unwrap();
    let decoded = decode(&bytes).unwrap();
    let frames = compose(&decoded).unwrap();
    assert_eq!(frames.len(), 3);
    // F2 snapshot — blue temporarily on top.
    assert_eq!(px(&frames[1].canvas, 0, 0), [0, 0, 0xFF, 0xFF]);
    // F3 final — blue undone, red visible, green drawn.
    let f3 = &frames[2].canvas;
    assert_eq!(px(f3, 0, 0), [0xFF, 0, 0, 0xFF], "red restored");
    assert_eq!(px(f3, 1, 1), [0xFF, 0, 0, 0xFF], "red restored");
    assert_eq!(px(f3, 2, 2), [0, 0xFF, 0, 0xFF], "green drawn");
}
