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

// ---------------------------------------------------------------------
// Round 213 — per-frame compositor edge-case sweep.
//
// The four tests above cover the canonical, non-overlapping case for
// each §23.c.iv disposal value 0–3. The sweep below targets the
// behaviour the §23 spec actually leaves implicit:
//
//   * §23.c.viii "Transparent" pixels are NOT modified — the prior
//     canvas state must show through. This is the only render-time
//     branch in `render_frame` that skips a write.
//   * §18.c.iii Restore-to-background with NO §18.c.ii Global Color
//     Table — the §18.c.vii Background Color Index is meaningless,
//     so the cleared rectangle becomes fully transparent black
//     (`background_color_rgba()` returns `[0, 0, 0, 0]`).
//   * §23.c.iv value 2 clears ONLY the disposing frame's own
//     placement rect. A larger prior frame's pixels outside the
//     small rect must survive the dispose-background sweep.
//   * §23.c.iv value 3 captures the snapshot at the START of the
//     disposing frame — including any transparent-pixel show-through
//     accumulated from earlier frames. Restoring must revert to that
//     post-show-through state, not to the pristine pre-everything
//     canvas.
//   * Nested / chained RestorePrevious — each frame's snapshot is
//     independent; restoring frame N reverts to the state at the
//     start of frame N, not the start of frame 0.
//   * Disposal applied to a §20 image that fully covers the screen —
//     the §18 logical screen and the frame's placement rect coincide,
//     so RestoreBackground wipes the entire canvas.
//   * The implicit cross-frame contract that the snapshot stored as
//     `delay_centis` on a `ComposedFrame` reports the disposing
//     frame's own delay, never the next frame's.
// ---------------------------------------------------------------------

/// Build a 6×6 stream backed by `palette()` (background = palette[0]
/// = black). Helper for the larger-screen edge-case tests.
fn image_6x6_with(blocks: Vec<Block>) -> GifImage {
    GifImage {
        version: Version::Gif89a,
        screen_width: 6,
        screen_height: 6,
        color_resolution: 1,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(palette()),
        blocks,
    }
}

/// Build a fixed-shape frame whose `indices` are written cell-by-cell
/// from the caller-supplied closure. Lets each test sketch the exact
/// per-pixel layout (including transparent-index cells) without
/// hand-rolling the linearisation each time.
fn shaped_frame(
    left: u16,
    top: u16,
    w: u16,
    h: u16,
    gce: Option<GraphicControl>,
    mut f: impl FnMut(u16, u16) -> u8,
) -> Frame {
    let mut indices = Vec::with_capacity((w as usize) * (h as usize));
    for y in 0..h {
        for x in 0..w {
            indices.push(f(x, y));
        }
    }
    Frame {
        left,
        top,
        width: w,
        height: h,
        local_palette: None,
        palette_sorted: false,
        interlaced: false,
        indices,
        graphic_control: gce,
    }
}

/// Transparent-index pass-through (§23.c.viii) — partial-coverage
/// frame N+1 leaves frame N's pixels visible everywhere it places a
/// transparent index. The visible canvas after N+1 must be the
/// element-wise overlay of the opaque pixels of N+1 over N.
#[test]
fn transparent_index_preserves_prior_canvas() {
    // F1: 4×4 of red (DisposalMethod::None — F2 sees F1's full canvas).
    let f1 = frame(
        0,
        0,
        4,
        4,
        1, // red
        Some(GraphicControl {
            disposal: DisposalMethod::None,
            user_input: false,
            transparent_index: None,
            delay_centis: 0,
        }),
    );
    // F2: same 4×4 footprint, checkerboard of green (index 2) +
    // transparent (index 3). transparent_index = 3 means index-3
    // cells must NOT modify the canvas → red shows through there.
    let f2 = shaped_frame(
        0,
        0,
        4,
        4,
        Some(GraphicControl {
            disposal: DisposalMethod::None,
            user_input: false,
            transparent_index: Some(3),
            delay_centis: 0,
        }),
        |x, y| if (x + y) % 2 == 0 { 2 } else { 3 },
    );
    let bytes = encode(&image_with(vec![Block::Image(f1), Block::Image(f2)])).unwrap();
    let frames = compose(&decode(&bytes).unwrap()).unwrap();
    let c2 = &frames[1].canvas;
    // Opaque green where (x+y)%2 == 0.
    assert_eq!(px(c2, 0, 0), [0, 0xFF, 0, 0xFF], "green at (0,0)");
    assert_eq!(px(c2, 2, 0), [0, 0xFF, 0, 0xFF], "green at (2,0)");
    assert_eq!(px(c2, 3, 1), [0, 0xFF, 0, 0xFF], "green at (3,1)");
    // Transparent index → prior red survives.
    assert_eq!(px(c2, 1, 0), [0xFF, 0, 0, 0xFF], "red through transparency");
    assert_eq!(px(c2, 0, 1), [0xFF, 0, 0, 0xFF], "red through transparency");
    assert_eq!(px(c2, 3, 0), [0xFF, 0, 0, 0xFF], "red through transparency");
}

/// §23.c.iv value 2 with NO §18 Global Color Table — `background_
/// color_rgba()` falls back to `[0, 0, 0, 0]` per the §18.c.iii
/// conservative reading, so the disposed rect becomes fully
/// transparent black (alpha 0), distinguishable from any palette
/// colour by the alpha byte.
#[test]
fn restore_background_with_no_gct_clears_to_transparent_black() {
    // Local-palette frame, no GCT on the stream. Disposal=2 should
    // wipe the rect to alpha=0 because background_color_rgba() can't
    // resolve a colour.
    let lp = palette();
    let f1 = Frame {
        left: 0,
        top: 0,
        width: 2,
        height: 2,
        local_palette: Some(lp),
        palette_sorted: false,
        interlaced: false,
        indices: vec![1; 4], // red
        graphic_control: Some(GraphicControl {
            disposal: DisposalMethod::RestoreBackground,
            user_input: false,
            transparent_index: None,
            delay_centis: 0,
        }),
    };
    // F2 must also carry a local palette — there is no GCT to fall
    // back on. Placed at (2,2) so its draw doesn't overlap F1's
    // (already-cleared) rect.
    let f2 = Frame {
        left: 2,
        top: 2,
        width: 2,
        height: 2,
        local_palette: Some(palette()),
        palette_sorted: false,
        interlaced: false,
        indices: vec![2; 4], // green
        graphic_control: None,
    };
    let img = GifImage {
        version: Version::Gif89a,
        screen_width: 4,
        screen_height: 4,
        color_resolution: 1,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: None, // <- key part: no GCT
        blocks: vec![Block::Image(f1), Block::Image(f2)],
    };
    let bytes = encode(&img).unwrap();
    let frames = compose(&decode(&bytes).unwrap()).unwrap();
    let c2 = &frames[1].canvas;
    // F1's old rect is now alpha-0 black (no GCT → no background
    // colour resolution; §23.c.iv value 2 falls back per §18.c.iii).
    assert_eq!(px(c2, 0, 0), [0, 0, 0, 0], "transparent black, alpha=0");
    assert_eq!(px(c2, 1, 1), [0, 0, 0, 0], "transparent black, alpha=0");
    // F2's drawn green is opaque.
    assert_eq!(px(c2, 2, 2), [0, 0xFF, 0, 0xFF]);
    assert_eq!(px(c2, 3, 3), [0, 0xFF, 0, 0xFF]);
}

/// §23.c.iv value 2 wipes ONLY the disposing frame's own placement
/// rect. A prior frame's pixels at coordinates outside that rect
/// must survive — the dispose call uses the disposing frame's
/// rectangle, not the entire prior canvas footprint.
#[test]
fn restore_background_clears_only_disposing_frames_own_rect() {
    // F1 covers the entire 6×6 screen with red — F2 is a small 2×2
    // patch of green at the centre that disposes to background. After
    // F2 disposes, only the 2×2 patch should be black; the rest of
    // the canvas stays red. F3 then paints blue elsewhere to make the
    // assertion meaningful.
    let f1 = frame(
        0,
        0,
        6,
        6,
        1, // red, full-screen
        Some(GraphicControl {
            disposal: DisposalMethod::None,
            user_input: false,
            transparent_index: None,
            delay_centis: 0,
        }),
    );
    let f2 = frame(
        2,
        2,
        2,
        2,
        2, // green, centre patch
        Some(GraphicControl {
            disposal: DisposalMethod::RestoreBackground,
            user_input: false,
            transparent_index: None,
            delay_centis: 0,
        }),
    );
    // F3 placed off the centre to give us a probe point that wasn't
    // touched by F2 at all — it should still show F1's red.
    let f3 = frame(0, 5, 1, 1, 3, None); // blue at corner
    let bytes = encode(&image_6x6_with(vec![
        Block::Image(f1),
        Block::Image(f2),
        Block::Image(f3),
    ]))
    .unwrap();
    let frames = compose(&decode(&bytes).unwrap()).unwrap();
    let c3 = &frames[2].canvas;
    // F2's 2×2 rect was wiped to background black.
    assert_eq!(px(c3, 2, 2), [0, 0, 0, 0xFF], "bg-cleared by F2 dispose");
    assert_eq!(px(c3, 3, 3), [0, 0, 0, 0xFF], "bg-cleared by F2 dispose");
    // Pixels OUTSIDE F2's rect are untouched red from F1.
    assert_eq!(px(c3, 0, 0), [0xFF, 0, 0, 0xFF], "F1 red survives");
    assert_eq!(px(c3, 5, 0), [0xFF, 0, 0, 0xFF], "F1 red survives");
    assert_eq!(px(c3, 4, 4), [0xFF, 0, 0, 0xFF], "F1 red survives");
    // F3's drawn pixel.
    assert_eq!(px(c3, 0, 5), [0, 0, 0xFF, 0xFF], "F3 blue drawn");
}

/// §23.c.iv value 3 — RestorePrevious — snapshots the canvas
/// *immediately before this block rendered*, so transparent-pixel
/// show-through accumulated in earlier frames is part of the
/// snapshot. After the restore, the show-through state is recovered
/// exactly, not replaced with a pristine pre-everything canvas.
#[test]
fn restore_previous_captures_show_through_state() {
    // Layout:
    //   F1 (red 4×4, Keep) establishes the visible baseline.
    //   F2 (full transparent layer over 4×4, GCE w/ transparent_index
    //      = 0, fill = 0; RestorePrevious) — this F2 draws NOTHING
    //      (every pixel is transparent_index) but its placement is
    //      4×4. The snapshot taken at F2's render time captures the
    //      red canvas. After F2 disposes via value 3, the canvas
    //      reverts to the same red — no change visible.
    //   F3 (blue 2×2 at (2,2), None) draws on top.
    let f1 = frame(
        0,
        0,
        4,
        4,
        1, // red
        Some(GraphicControl {
            disposal: DisposalMethod::Keep,
            user_input: false,
            transparent_index: None,
            delay_centis: 0,
        }),
    );
    let f2 = shaped_frame(
        0,
        0,
        4,
        4,
        Some(GraphicControl {
            disposal: DisposalMethod::RestorePrevious,
            user_input: false,
            transparent_index: Some(0), // all pixels transparent
            delay_centis: 0,
        }),
        |_, _| 0, // every pixel is the transparent index → no writes
    );
    let f3 = frame(2, 2, 2, 2, 3, None); // blue
    let bytes = encode(&image_with(vec![
        Block::Image(f1),
        Block::Image(f2),
        Block::Image(f3),
    ]))
    .unwrap();
    let frames = compose(&decode(&bytes).unwrap()).unwrap();
    // F2 snapshot — fully transparent draw, canvas still shows F1.
    let c2 = &frames[1].canvas;
    assert_eq!(
        px(c2, 0, 0),
        [0xFF, 0, 0, 0xFF],
        "F1 red through F2 transparency"
    );
    assert_eq!(
        px(c2, 3, 3),
        [0xFF, 0, 0, 0xFF],
        "F1 red through F2 transparency"
    );
    // After F2 disposes via RestorePrevious, canvas reverts to the
    // pre-F2 state — which IS the F1 red canvas. F3 then paints blue.
    let c3 = &frames[2].canvas;
    assert_eq!(px(c3, 0, 0), [0xFF, 0, 0, 0xFF], "red restored (== pre-F2)");
    assert_eq!(px(c3, 1, 0), [0xFF, 0, 0, 0xFF], "red restored (== pre-F2)");
    assert_eq!(px(c3, 2, 2), [0, 0, 0xFF, 0xFF], "F3 blue drawn");
    assert_eq!(px(c3, 3, 3), [0, 0, 0xFF, 0xFF], "F3 blue drawn");
}

/// Nested / chained RestorePrevious — each frame's pre-render
/// snapshot is independent. F2 and F3 both dispose-previous, so after
/// F3 disposes the canvas reverts to the state at the start of F3
/// (= state at the end of F2's dispose = state at the start of F2 =
/// F1's red canvas). F4 then paints on top.
#[test]
fn nested_restore_previous_chain() {
    // F1: red 4×4, Keep.
    // F2: green 4×4, RestorePrevious.
    // F3: blue 4×4, RestorePrevious.
    // F4: 2×2 at (2,2) palette[0] = black (explicit drawn black so we
    //     can tell it apart from "no draw"), no GCE.
    let f1 = frame(
        0,
        0,
        4,
        4,
        1, // red
        Some(GraphicControl {
            disposal: DisposalMethod::Keep,
            user_input: false,
            transparent_index: None,
            delay_centis: 0,
        }),
    );
    let f2 = frame(
        0,
        0,
        4,
        4,
        2, // green
        Some(GraphicControl {
            disposal: DisposalMethod::RestorePrevious,
            user_input: false,
            transparent_index: None,
            delay_centis: 0,
        }),
    );
    let f3 = frame(
        0,
        0,
        4,
        4,
        3, // blue
        Some(GraphicControl {
            disposal: DisposalMethod::RestorePrevious,
            user_input: false,
            transparent_index: None,
            delay_centis: 0,
        }),
    );
    let f4 = frame(2, 2, 2, 2, 0, None); // black drawn pixels
    let bytes = encode(&image_with(vec![
        Block::Image(f1),
        Block::Image(f2),
        Block::Image(f3),
        Block::Image(f4),
    ]))
    .unwrap();
    let frames = compose(&decode(&bytes).unwrap()).unwrap();
    assert_eq!(frames.len(), 4);
    // F2 snapshot — green visible.
    assert_eq!(px(&frames[1].canvas, 0, 0), [0, 0xFF, 0, 0xFF]);
    // After F2's RestorePrevious, the pre-F2 state (= red canvas)
    // is recovered, then F3 paints blue over it.
    assert_eq!(px(&frames[2].canvas, 0, 0), [0, 0, 0xFF, 0xFF]);
    // After F3's RestorePrevious, pre-F3 state recovered. Note: F3's
    // pre-render snapshot was taken AFTER F2's dispose ran, so it's
    // the red canvas — not the green canvas, not the post-F2 snapshot.
    // F4 then paints black at (2,2) over that red canvas.
    let c4 = &frames[3].canvas;
    assert_eq!(
        px(c4, 0, 0),
        [0xFF, 0, 0, 0xFF],
        "red restored after F3 dispose"
    );
    assert_eq!(
        px(c4, 1, 0),
        [0xFF, 0, 0, 0xFF],
        "red restored after F3 dispose"
    );
    assert_eq!(
        px(c4, 3, 0),
        [0xFF, 0, 0, 0xFF],
        "red restored after F3 dispose"
    );
    assert_eq!(px(c4, 2, 2), [0, 0, 0, 0xFF], "F4 black drawn");
    assert_eq!(px(c4, 3, 3), [0, 0, 0, 0xFF], "F4 black drawn");
}

/// §23.c.iv value 2 on a frame whose placement rect equals the
/// entire §18 Logical Screen — the dispose-background sweep wipes
/// the whole canvas.
#[test]
fn restore_background_full_screen_frame_wipes_canvas() {
    let f1 = frame(
        0,
        0,
        4,
        4, // full screen
        1, // red
        Some(GraphicControl {
            disposal: DisposalMethod::RestoreBackground,
            user_input: false,
            transparent_index: None,
            delay_centis: 0,
        }),
    );
    let f2 = frame(0, 0, 1, 1, 2, None); // green at corner
    let bytes = encode(&image_with(vec![Block::Image(f1), Block::Image(f2)])).unwrap();
    let frames = compose(&decode(&bytes).unwrap()).unwrap();
    // F1 snapshot — full red canvas.
    assert_eq!(px(&frames[0].canvas, 0, 0), [0xFF, 0, 0, 0xFF]);
    assert_eq!(px(&frames[0].canvas, 3, 3), [0xFF, 0, 0, 0xFF]);
    // F2 canvas — everything outside F2's 1×1 patch is bg black.
    let c2 = &frames[1].canvas;
    assert_eq!(px(c2, 0, 0), [0, 0xFF, 0, 0xFF], "F2 green drawn");
    assert_eq!(px(c2, 1, 0), [0, 0, 0, 0xFF], "wiped by F1 dispose");
    assert_eq!(px(c2, 3, 3), [0, 0, 0, 0xFF], "wiped by F1 dispose");
    assert_eq!(px(c2, 2, 2), [0, 0, 0, 0xFF], "wiped by F1 dispose");
}

/// `ComposedFrame::delay_centis` reports the *disposing* frame's
/// own §23.c.vii Delay Time — not the next frame's, not a sum. The
/// snapshot at index i carries the GCE.delay_centis of block i.
#[test]
fn delay_centis_matches_frames_own_gce() {
    let f1 = frame(
        0,
        0,
        2,
        2,
        1,
        Some(GraphicControl {
            disposal: DisposalMethod::None,
            user_input: false,
            transparent_index: None,
            delay_centis: 11, // distinct, prime
        }),
    );
    let f2 = frame(
        2,
        2,
        2,
        2,
        2,
        Some(GraphicControl {
            disposal: DisposalMethod::RestoreBackground,
            user_input: false,
            transparent_index: None,
            delay_centis: 23, // distinct, prime
        }),
    );
    // F3 deliberately has NO GCE → delay_centis must default to 0.
    let f3 = frame(0, 2, 2, 2, 3, None);
    let bytes = encode(&image_with(vec![
        Block::Image(f1),
        Block::Image(f2),
        Block::Image(f3),
    ]))
    .unwrap();
    let frames = compose(&decode(&bytes).unwrap()).unwrap();
    assert_eq!(frames[0].delay_centis, 11, "F1 reports its own delay");
    assert_eq!(frames[1].delay_centis, 23, "F2 reports its own delay");
    assert_eq!(frames[2].delay_centis, 0, "F3 has no GCE → 0");
}
