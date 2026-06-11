//! Encoder-side inter-frame rect optimisation tests.
//!
//! [`GifImage::optimize_frame_rects`] crops each §20 Image frame to
//! the bounding rectangle of pixels it actually changes on the
//! composed logical screen (§20.c.ii–v placement, §23.c.iv disposal,
//! §23.c.viii transparency). The invariant under test throughout:
//! `compose(before) == compose(after)` — the pass must never change
//! anything a viewer displays.

use oxideav_gif::{
    compose, decode, encode, Block, DisposalMethod, Frame, GifImage, GraphicControl, PlainText,
    Rgb, Version,
};

fn palette_4() -> Vec<Rgb> {
    vec![
        Rgb::new(0, 0, 0),
        Rgb::new(0xFF, 0, 0),
        Rgb::new(0, 0xFF, 0),
        Rgb::new(0, 0, 0xFF),
    ]
}

fn base_image(screen: u16, blocks: Vec<Block>) -> GifImage {
    GifImage {
        version: Version::Gif89a,
        screen_width: screen,
        screen_height: screen,
        color_resolution: 1,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(palette_4()),
        blocks,
    }
}

fn full_frame(screen: u16, indices: Vec<u8>, gce: Option<GraphicControl>) -> Frame {
    assert_eq!(indices.len(), (screen as usize) * (screen as usize));
    Frame {
        left: 0,
        top: 0,
        width: screen,
        height: screen,
        local_palette: None,
        palette_sorted: false,
        interlaced: false,
        indices,
        graphic_control: gce,
    }
}

fn gce(disposal: DisposalMethod, transparent_index: Option<u8>) -> GraphicControl {
    GraphicControl {
        disposal,
        user_input: false,
        transparent_index,
        delay_centis: 5,
    }
}

/// Pull the rect of the §20 Image at block position `i`.
fn rect_of(img: &GifImage, i: usize) -> (u16, u16, u16, u16) {
    match &img.blocks[i] {
        Block::Image(f) => (f.left, f.top, f.width, f.height),
        other => panic!("block {i} is not an image: {other:?}"),
    }
}

fn assert_compose_equal(before: &GifImage, after: &GifImage) {
    assert_eq!(
        compose(before).unwrap(),
        compose(after).unwrap(),
        "optimize_frame_rects changed the composed output"
    );
}

/// F2 repeats F1 except for a 2×2 green patch at (5,3) → F2 must be
/// cropped to exactly that patch, and the encoded stream must shrink.
#[test]
fn shrinks_to_changed_patch() {
    let screen = 8u16;
    let f1 = vec![1u8; 64]; // solid red
    let mut f2 = f1.clone();
    for y in 3..5usize {
        for x in 5..7usize {
            f2[y * 8 + x] = 2; // green patch
        }
    }
    let mut img = base_image(
        screen,
        vec![
            Block::Image(full_frame(
                screen,
                f1,
                Some(gce(DisposalMethod::Keep, None)),
            )),
            Block::Image(full_frame(screen, f2, None)),
        ],
    );
    let original = img.clone();
    let bytes_before = encode(&img).unwrap();

    assert_eq!(img.optimize_frame_rects(), 1);
    assert_eq!(rect_of(&img, 1), (5, 3, 2, 2));
    match &img.blocks[1] {
        Block::Image(f) => assert_eq!(f.indices, vec![2; 4]),
        _ => unreachable!(),
    }
    // F1 renders onto a fully transparent initial canvas, so every
    // opaque pixel is a change — full rect, untouched.
    assert_eq!(rect_of(&img, 0), (0, 0, 8, 8));

    assert_compose_equal(&original, &img);
    let bytes_after = encode(&img).unwrap();
    assert!(
        bytes_after.len() < bytes_before.len(),
        "expected size win: {} -> {}",
        bytes_before.len(),
        bytes_after.len()
    );
    // The optimised stream still round-trips exactly.
    assert_eq!(decode(&bytes_after).unwrap(), img);
}

/// An exact-duplicate frame changes nothing → 1×1 crop at its original
/// top-left (§20 has no zero-area image), composed output unchanged.
#[test]
fn duplicate_frame_shrinks_to_single_pixel() {
    let screen = 4u16;
    let raster = vec![3u8; 16];
    let mut img = base_image(
        screen,
        vec![
            Block::Image(full_frame(
                screen,
                raster.clone(),
                Some(gce(DisposalMethod::None, None)),
            )),
            Block::Image(full_frame(screen, raster, None)),
        ],
    );
    let original = img.clone();
    assert_eq!(img.optimize_frame_rects(), 1);
    assert_eq!(rect_of(&img, 1), (0, 0, 1, 1));
    assert_compose_equal(&original, &img);
}

/// §23.c.iv value 2 — "the area used by the graphic must be restored
/// to the background color". Cropping would shrink the cleared area,
/// so RestoreBackground frames are never modified.
#[test]
fn restore_background_frame_left_untouched() {
    let screen = 4u16;
    let f1 = vec![1u8; 16];
    let mut f2 = f1.clone();
    f2[5] = 2; // single changed pixel — croppable were it not for the disposal
    let mut img = base_image(
        screen,
        vec![
            Block::Image(full_frame(
                screen,
                f1,
                Some(gce(DisposalMethod::Keep, None)),
            )),
            Block::Image(full_frame(
                screen,
                f2,
                Some(gce(DisposalMethod::RestoreBackground, None)),
            )),
            Block::Image(full_frame(screen, vec![2u8; 16], None)),
        ],
    );
    let original = img.clone();
    // Frame 2 must stay full-rect despite its 1-pixel diff. (Frames 1
    // and 3 change every pixel they cover — full-rect plans are
    // rejected as no-saving — so nothing in this stream shrinks.)
    assert_eq!(img.optimize_frame_rects(), 0);
    assert_eq!(rect_of(&img, 1), (0, 0, 4, 4), "RestoreBackground frame");
    assert_eq!(img, original, "stream must be byte-identical");
    assert_compose_equal(&original, &img);
}

/// §23.c.iv value 3 — restore to previous reverts to the pre-render
/// canvas; pixels a cropped frame no longer overwrites already equal
/// that canvas, so cropping is safe.
#[test]
fn restore_previous_frame_cropped() {
    let screen = 4u16;
    let f1 = vec![1u8; 16];
    let mut f2 = f1.clone();
    f2[10] = 3; // blue blink pixel at (2,2)
    let mut img = base_image(
        screen,
        vec![
            Block::Image(full_frame(
                screen,
                f1,
                Some(gce(DisposalMethod::Keep, None)),
            )),
            Block::Image(full_frame(
                screen,
                f2,
                Some(gce(DisposalMethod::RestorePrevious, None)),
            )),
            Block::Image(full_frame(screen, vec![2u8; 16], None)),
        ],
    );
    let original = img.clone();
    assert!(img.optimize_frame_rects() >= 1);
    assert_eq!(rect_of(&img, 1), (2, 2, 1, 1), "blink frame cropped");
    assert_compose_equal(&original, &img);
}

/// §23.c.viii — transparent pixels "are not modified" on the display
/// device, so a mostly-transparent overlay crops to the bounding box
/// of its opaque pixels (here: the first frame, diffed against the
/// fully transparent initial canvas).
#[test]
fn transparent_overlay_crops_to_opaque_bbox() {
    let screen = 8u16;
    let mut indices = vec![0u8; 64]; // index 0 = transparent below
    indices[3 * 8 + 2] = 1; // (2,3)
    indices[5 * 8 + 6] = 2; // (6,5)
    let mut img = base_image(
        screen,
        vec![Block::Image(full_frame(
            screen,
            indices,
            Some(gce(DisposalMethod::None, Some(0))),
        ))],
    );
    let original = img.clone();
    assert_eq!(img.optimize_frame_rects(), 1);
    // Bounding box of the two opaque pixels: x 2..=6, y 3..=5.
    assert_eq!(rect_of(&img, 0), (2, 3, 5, 3));
    assert_compose_equal(&original, &img);
}

/// §25 Plain Text blocks render through the same disposal state
/// machine but are never cropped; image frames around them still get
/// exact diffs.
#[test]
fn plain_text_blocks_participate_but_stay_untouched() {
    let screen = 16u16;
    let pt = PlainText {
        left: 0,
        top: 0,
        width: 8,
        height: 8,
        cell_width: 8,
        cell_height: 8,
        fg_color_index: 1,
        bg_color_index: 2,
        text: b"A".to_vec(),
    };
    let f1 = vec![3u8; 256];
    let mut f2 = f1.clone();
    f2[0] = 1; // (0,0) red — overlaps the glyph cell the text painted
    let mut img = base_image(
        screen,
        vec![
            Block::Image(full_frame(
                screen,
                f1,
                Some(gce(DisposalMethod::Keep, None)),
            )),
            Block::PlainText {
                params: pt.clone(),
                graphic_control: Some(gce(DisposalMethod::Keep, None)),
            },
            Block::Image(full_frame(screen, f2, None)),
        ],
    );
    let original = img.clone();
    // Frame 3 must be diffed against the canvas *with* the rendered
    // text, not against frame 1 — it repaints the 8×8 text cell back
    // to blue plus one red pixel, so its crop covers the text cell.
    assert_eq!(img.optimize_frame_rects(), 1);
    assert_eq!(rect_of(&img, 2), (0, 0, 8, 8));
    match &img.blocks[1] {
        Block::PlainText { params, .. } => assert_eq!(params, &pt),
        _ => panic!("plain text block was modified"),
    }
    assert_compose_equal(&original, &img);
}

/// A stream that doesn't compose (placement escapes the §18 logical
/// screen) is left completely unmodified.
#[test]
fn non_composing_stream_left_untouched() {
    // (3,3) + 4×4 escapes the 4×4 logical screen.
    let f = Frame {
        left: 3,
        top: 3,
        width: 4,
        height: 4,
        local_palette: None,
        palette_sorted: false,
        interlaced: false,
        indices: vec![1; 16],
        graphic_control: None,
    };
    let mut img = base_image(4, vec![Block::Image(f)]);
    let original = img.clone();
    assert_eq!(img.optimize_frame_rects(), 0);
    assert_eq!(img, original);
}

/// Reproducible xorshift32 (same shape as `randomized_roundtrip.rs`).
struct Xs(u32);
impl Xs {
    fn new(seed: u32) -> Self {
        Self(seed | 1)
    }
    fn next_u32(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next_u32() % n
    }
}

/// Randomized property: for animations mixing all four disposal
/// methods, transparency, and sparse inter-frame changes, the pass
/// must (a) preserve the composed output exactly, (b) keep the stream
/// encodable + round-trippable, and (c) be idempotent.
#[test]
fn randomized_compose_equivalence() {
    const SCREEN: u16 = 24;
    const AREA: usize = (SCREEN as usize) * (SCREEN as usize);
    let disposals = [
        DisposalMethod::None,
        DisposalMethod::Keep,
        DisposalMethod::RestoreBackground,
        DisposalMethod::RestorePrevious,
    ];
    for seed in 0u32..24 {
        let mut rng = Xs::new(0x0F1C_E000 + seed);
        let mut raster: Vec<u8> = (0..AREA).map(|_| (rng.below(4)) as u8).collect();
        let mut blocks = Vec::new();
        for frame_no in 0..6 {
            if frame_no > 0 {
                // Sparse change: 1..=8 random pixels get new indices.
                for _ in 0..(1 + rng.below(8)) {
                    let at = rng.below(AREA as u32) as usize;
                    raster[at] = rng.below(4) as u8;
                }
            }
            let disposal = disposals[rng.below(4) as usize];
            let transparent_index = if rng.below(3) == 0 { Some(0u8) } else { None };
            blocks.push(Block::Image(full_frame(
                SCREEN,
                raster.clone(),
                Some(gce(disposal, transparent_index)),
            )));
        }
        let original = base_image(SCREEN, blocks);
        let mut optimized = original.clone();
        let shrunk = optimized.optimize_frame_rects();
        assert_compose_equal(&original, &optimized);
        assert_eq!(
            optimized.optimize_frame_rects(),
            0,
            "seed {seed}: pass must be idempotent (first call shrank {shrunk})"
        );
        let bytes_before = encode(&original).unwrap();
        let bytes_after = encode(&optimized).unwrap();
        assert!(
            bytes_after.len() <= bytes_before.len(),
            "seed {seed}: optimisation grew the stream"
        );
        assert_eq!(decode(&bytes_after).unwrap(), optimized, "seed {seed}");
    }
}
