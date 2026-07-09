//! End-to-end coverage for encode-side interlacing (§20.c.vii Interlace
//! Flag + Appendix E four-pass row order), driven through the
//! [`GifImage::set_frames_interlaced`] mutator.
//!
//! Interlacing is a *storage-order* choice, never a pixel change: the
//! encoder re-shuffles each interlaced frame's rows into the Appendix E
//! four-pass order at serialisation time, and the decoder presents every
//! frame already de-interlaced. So the load-bearing property is that
//! flipping the flag leaves the *decoded* pixels and the *composed* RGBA
//! output byte-identical while changing only the on-disk layout. These
//! tests pin that across the Appendix E pass boundaries, sub-rectangle
//! placement, transparency + disposal, and the `optimize_frame_rects`
//! encoder pass.

use oxideav_gif::{compose, decode, encode, AnimationBuilder, DisposalMethod, GifImage, Rgb};

/// A 16-entry greyscale palette (plus one distinct colour for
/// transparency corners) — enough distinct rows that Appendix E's row
/// permutation actually reorders the on-disk bytes.
fn palette16() -> Vec<Rgb> {
    (0..16u8)
        .map(|i| {
            let v = i.wrapping_mul(17);
            Rgb::new(v, v ^ 0x5A, v.wrapping_add(0x24))
        })
        .collect()
}

/// A `w × h` index plane whose value at `(r, c)` varies with both row and
/// column, so a row permutation produces a genuinely different byte
/// sequence.
fn varied_indices(w: u16, h: u16, palette_len: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(w as usize * h as usize);
    for r in 0..h as u32 {
        for c in 0..w as u32 {
            out.push(((r * 7 + c * 3 + 1) % palette_len as u32) as u8);
        }
    }
    out
}

fn single_frame(w: u16, h: u16) -> GifImage {
    let pal = palette16();
    AnimationBuilder::new(w, h, pal)
        .add_full_frame(varied_indices(w, h, 16), 10, DisposalMethod::None)
        .expect("add frame")
        .build()
        .expect("build")
}

/// Encoding an interlaced stream and decoding it must reproduce the exact
/// same `GifImage` as the sequential source: the decoder de-interlaces
/// back to row-major order and preserves the Interlace Flag, so the two
/// images are equal field-for-field.
#[test]
fn interlaced_encode_decode_is_field_stable() {
    let mut img = single_frame(11, 13);
    let changed = img.set_frames_interlaced(true);
    assert_eq!(changed, 1);
    assert!(img.has_interlaced_frames());

    let bytes = encode(&img).expect("encode interlaced");
    let decoded = decode(&bytes).expect("decode interlaced");
    assert_eq!(decoded, img, "interlaced round-trip is field-stable");
    // The decoded frame's flag survives and its raster is de-interlaced
    // back to the original row-major order.
    assert!(decoded.frames().next().unwrap().interlaced);
    assert_eq!(
        decoded.frames().next().unwrap().indices,
        varied_indices(11, 13, 16)
    );
}

/// Toggling the flag must not change the decoded pixels — the sequential
/// and interlaced encodings decode to the same raster — but it *does*
/// change the on-disk bytes for a raster with row-to-row variation
/// (otherwise the Appendix E permutation would be a no-op and the flag
/// pointless).
#[test]
fn interlacing_changes_bytes_but_not_pixels() {
    let sequential = single_frame(11, 13);
    let mut interlaced = sequential.clone();
    interlaced.set_frames_interlaced(true);

    let seq_bytes = encode(&sequential).expect("encode sequential");
    let int_bytes = encode(&interlaced).expect("encode interlaced");
    assert_ne!(
        seq_bytes, int_bytes,
        "row permutation must alter the compressed payload"
    );

    let seq_px = decode(&seq_bytes)
        .unwrap()
        .frames()
        .next()
        .unwrap()
        .indices
        .clone();
    let int_px = decode(&int_bytes)
        .unwrap()
        .frames()
        .next()
        .unwrap()
        .indices
        .clone();
    assert_eq!(seq_px, int_px, "de-interlaced pixels identical");
}

/// Appendix E splits an image's rows into four passes; the pass structure
/// changes shape at small heights (a 1-row image is pass 1 only, etc.).
/// Every height across those boundaries must survive an interlaced
/// encode → decode round-trip with pixel-exact output.
#[test]
fn appendix_e_pass_boundaries_roundtrip() {
    for h in 1u16..=20 {
        for w in [1u16, 5, 8] {
            let original = varied_indices(w, h, 16);
            let mut img = AnimationBuilder::new(w, h, palette16())
                .add_full_frame(original.clone(), 0, DisposalMethod::None)
                .expect("add")
                .build()
                .expect("build");
            img.set_frames_interlaced(true);

            let bytes = encode(&img).expect("encode");
            let decoded = decode(&bytes).expect("decode");
            assert_eq!(
                decoded.frames().next().unwrap().indices,
                original,
                "interlaced {w}x{h} did not round-trip to the row-major raster"
            );
        }
    }
}

/// A sub-rectangle (non-full-screen) interlaced frame must still
/// round-trip: Appendix E orders the *frame's own* rows, independent of
/// its placement on the logical screen.
#[test]
fn placed_interlaced_frame_roundtrips() {
    let pal = palette16();
    let sub = varied_indices(6, 9, 16);
    let mut img = AnimationBuilder::new(16, 16, pal)
        .add_placed_frame(4, 3, 6, 9, sub.clone(), 5, DisposalMethod::None)
        .expect("add placed")
        .build()
        .expect("build");
    img.set_frames_interlaced(true);

    let decoded = decode(&encode(&img).unwrap()).unwrap();
    let f = decoded.frames().next().unwrap();
    assert_eq!((f.left, f.top, f.width, f.height), (4, 3, 6, 9));
    assert!(f.interlaced);
    assert_eq!(f.indices, sub);
}

/// The load-bearing compositing property: an animation composited with
/// every frame interlaced yields byte-identical canvases to the same
/// animation composited sequentially. Exercised through a multi-frame
/// stream with mixed disposal methods so the §23 state machine is in
/// play, and through the encode → decode → compose pipeline (not just the
/// in-memory image).
#[test]
fn interlaced_animation_composites_identically() {
    let pal = palette16();
    let sequential = AnimationBuilder::new(12, 10, pal)
        .loop_forever()
        .add_full_frame(varied_indices(12, 10, 16), 4, DisposalMethod::None)
        .expect("f0")
        .add_placed_frame(
            2,
            1,
            8,
            6,
            varied_indices(8, 6, 16),
            4,
            DisposalMethod::RestoreBackground,
        )
        .expect("f1")
        .add_full_frame(
            varied_indices(12, 10, 16),
            4,
            DisposalMethod::RestorePrevious,
        )
        .expect("f2")
        .build()
        .expect("build");

    let mut interlaced = sequential.clone();
    assert_eq!(interlaced.set_frames_interlaced(true), 3);

    // In-memory compose equivalence.
    let seq_frames = compose(&sequential).expect("compose seq");
    let int_frames = compose(&interlaced).expect("compose int");
    assert_eq!(
        seq_frames, int_frames,
        "compose(interlaced) == compose(sequential)"
    );

    // Same through a full encode → decode → compose trip.
    let seq_rt = compose(&decode(&encode(&sequential).unwrap()).unwrap()).unwrap();
    let int_rt = compose(&decode(&encode(&interlaced).unwrap()).unwrap()).unwrap();
    assert_eq!(seq_rt, int_rt, "round-tripped compose stays equal");
    assert_eq!(
        seq_frames, seq_rt,
        "encode round-trip preserves the composited pixels"
    );
}

/// `optimize_frame_rects` (the inter-frame cropping encoder pass) must
/// leave the composited output unchanged on an interlaced stream, exactly
/// as it does on a sequential one — the crop happens in row-major index
/// space and the Interlace Flag rides along untouched.
#[test]
fn optimize_frame_rects_preserves_interlaced_compose() {
    let pal = palette16();
    // Two identical full frames: the second is a pure duplicate, which
    // optimize_frame_rects collapses to a 1×1 no-change placement.
    let raster = varied_indices(10, 8, 16);
    let mut img = AnimationBuilder::new(10, 8, pal)
        .loop_forever()
        .add_full_frame(raster.clone(), 3, DisposalMethod::None)
        .expect("f0")
        .add_full_frame(raster, 3, DisposalMethod::None)
        .expect("f1")
        .build()
        .expect("build");
    img.set_frames_interlaced(true);

    let before = compose(&img).expect("compose before");
    let _ = img.optimize_frame_rects();
    let after = compose(&img).expect("compose after");
    assert_eq!(
        before, after,
        "optimize_frame_rects changed composited pixels"
    );

    // Still encodes and decodes to the same composited output.
    let rt = compose(&decode(&encode(&img).unwrap()).unwrap()).unwrap();
    assert_eq!(after, rt, "optimized interlaced stream round-trips");
}
