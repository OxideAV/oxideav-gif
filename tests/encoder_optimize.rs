//! Encoder Global vs Local Color Table optimisation.
//!
//! [`GifImage::optimize_color_tables`] hoists a per-frame palette into
//! the §18 Global Color Table when every frame carries the same
//! palette, removing the redundant §21 Local Color Table on each
//! frame. The on-wire savings are `3 × 2^(size_bits + 1) + 1` bytes
//! per frame: the 3-byte RGB triplets of the LCT (rounded up to the
//! next power of two per §21) plus the byte we save by clearing the
//! Local Color Table Flag in the §20.c packed-fields byte.
//!
//! The transformation is purely byte-level — the decoded pixel values
//! must be identical before and after, since §21 says a frame with
//! the LCT flag clear uses the §18 Global Color Table.

use oxideav_gif::{compose, decode, encode, Block, Frame, GifImage, Rgb, Version};

fn frame_with_local(palette: Vec<Rgb>, fill: u8) -> Frame {
    Frame {
        left: 0,
        top: 0,
        width: 4,
        height: 4,
        local_palette: Some(palette),
        palette_sorted: false,
        interlaced: false,
        indices: vec![fill; 16],
        graphic_control: None,
    }
}

fn shared_palette() -> Vec<Rgb> {
    // 8-entry palette → §18 size field = 2 → on-disk 8 RGB triplets
    // = 24 bytes per LCT.
    vec![
        Rgb::new(0, 0, 0),
        Rgb::new(0xFF, 0, 0),
        Rgb::new(0, 0xFF, 0),
        Rgb::new(0, 0, 0xFF),
        Rgb::new(0xFF, 0xFF, 0),
        Rgb::new(0xFF, 0, 0xFF),
        Rgb::new(0, 0xFF, 0xFF),
        Rgb::new(0xFF, 0xFF, 0xFF),
    ]
}

fn three_frame_image(global: Option<Vec<Rgb>>, frames: Vec<Frame>) -> GifImage {
    GifImage {
        version: Version::Gif89a,
        screen_width: 4,
        screen_height: 4,
        color_resolution: 2,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: global,
        blocks: frames.into_iter().map(Block::Image).collect(),
    }
}

/// Hoisting a shared LCT into the GCT must produce a strictly smaller
/// encoded stream and preserve every decoded pixel.
#[test]
fn hoist_shared_lct_shrinks_stream_and_preserves_pixels() {
    let pal = shared_palette();
    let mut img = three_frame_image(
        None,
        vec![
            frame_with_local(pal.clone(), 0),
            frame_with_local(pal.clone(), 1),
            frame_with_local(pal.clone(), 2),
        ],
    );
    let before = encode(&img).expect("baseline encode");
    assert!(img.optimize_color_tables(), "hoist should succeed");
    let after = encode(&img).expect("hoisted encode");

    // We removed 2 LCTs (the third frame's "LCT" becomes the GCT we
    // didn't have before). Each LCT is 24 bytes (8-entry palette);
    // we add a 24-byte GCT but save 24 + 24 = 48 bytes from clearing
    // the two redundant LCTs. Net savings = 24 bytes.
    let saved = before.len() - after.len();
    assert!(
        saved >= 24,
        "expected ≥ 24 bytes saved, got {saved}; before={} after={}",
        before.len(),
        after.len()
    );

    // Pixels are unchanged: §21 says a frame with the LCT flag clear
    // uses the §18 GCT, and we hoisted the same palette.
    let composed_before = compose(&decode(&before).unwrap()).unwrap();
    let composed_after = compose(&decode(&after).unwrap()).unwrap();
    assert_eq!(composed_before, composed_after);
}

/// When frame palettes differ, the optimisation refuses and the stream
/// is byte-identical to baseline.
#[test]
fn differing_palettes_refuse_to_hoist() {
    let pal_a = shared_palette();
    let mut pal_b = shared_palette();
    pal_b[0] = Rgb::new(1, 2, 3); // perturb one entry
    let mut img = three_frame_image(
        None,
        vec![
            frame_with_local(pal_a.clone(), 0),
            frame_with_local(pal_b, 1),
        ],
    );
    let before = encode(&img).unwrap();
    assert!(!img.optimize_color_tables(), "must refuse mixed palettes");
    let after = encode(&img).unwrap();
    assert_eq!(before, after, "refused hoist must not perturb the stream");
}

/// A stream that already has a GCT plus matching LCTs decodes to the
/// same pixels both before and after the hoist.
#[test]
fn redundant_lcts_clear_against_existing_gct() {
    let pal = shared_palette();
    let mut img = three_frame_image(
        Some(pal.clone()),
        vec![
            frame_with_local(pal.clone(), 0),
            frame_with_local(pal.clone(), 1),
            frame_with_local(pal, 2),
        ],
    );
    let before_pixels = compose(&decode(&encode(&img).unwrap()).unwrap()).unwrap();
    assert!(img.optimize_color_tables(), "hoist should succeed");
    let after_bytes = encode(&img).unwrap();
    let after_pixels = compose(&decode(&after_bytes).unwrap()).unwrap();
    assert_eq!(before_pixels, after_pixels);
    // Every frame's LCT is now cleared.
    for f in img.frames() {
        assert!(f.local_palette.is_none());
    }
}
