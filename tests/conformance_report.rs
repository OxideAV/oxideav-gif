//! Integration coverage for [`GifImage::conformance_report`].
//!
//! The unit tests in `src/conformance.rs` pin each individual rule on a
//! hand-built `GifImage`. These integration tests pin the two
//! cross-cutting properties the diagnostic surface must hold against
//! the real encode/decode/build paths:
//!
//! 1. Every image that *decodes* from a spec-derived byte stream, and
//!    every image the `AnimationBuilder` produces, reports **no
//!    conformance errors** — the report agrees with the decoder/builder
//!    that the stream is well-formed.
//! 2. A clean (`!has_errors()`) report implies [`encode`] accepts the
//!    image; a report carrying a [`ConformanceRule`] the encoder also
//!    enforces fatally (frame indices length, version, palette size)
//!    implies [`encode`] rejects it. The diagnostic surface and the
//!    fatal encoder validation never disagree on those shared rules.

use oxideav_gif::{
    decode, encode, AnimationBuilder, Block, ConformanceRule, ConformanceSeverity, DisposalMethod,
    Frame, GifImage, GraphicControl, PlainText, Rgb, Version,
};

/// A minimal well-formed GIF89a animation: 2×2, two frames, looping.
fn builder_animation() -> GifImage {
    let palette = vec![Rgb::new(0xFF, 0, 0), Rgb::new(0, 0xFF, 0)];
    AnimationBuilder::new(2, 2, palette)
        .loop_forever()
        .add_full_frame(vec![0, 1, 1, 0], 50, DisposalMethod::RestoreBackground)
        .unwrap()
        .add_full_frame(vec![1, 0, 0, 1], 50, DisposalMethod::None)
        .unwrap()
        .build()
        .unwrap()
}

#[test]
fn builder_output_is_conformant() {
    let img = builder_animation();
    let report = img.conformance_report();
    assert!(
        report.is_clean(),
        "AnimationBuilder output must be fully conformant, got: {:?}",
        report.issues()
    );
}

#[test]
fn builder_output_survives_roundtrip_conformant() {
    let img = builder_animation();
    let bytes = encode(&img).unwrap();
    let decoded = decode(&bytes).unwrap();
    let report = decoded.conformance_report();
    assert!(
        !report.has_errors(),
        "decoded builder output must have no conformance errors, got: {:?}",
        report.issues()
    );
}

/// A GIF89a stream with a §23 GCE, a §24 Comment and a §26 Application
/// block, every field built from a literal reading of the spec. Decodes
/// and must report clean.
#[test]
fn decoded_multi_block_stream_is_conformant() {
    #[rustfmt::skip]
    const FIXTURE: &[u8] = &[
        // Header §17 — GIF89a
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61,
        // Logical Screen Descriptor §18 — 2×2, GCT flag set, 4-entry table
        0x02, 0x00, 0x02, 0x00,
        // packed: GCT flag=1, ColorRes=0, Sort=0, Size=1 (=> 2^(1+1)=4 entries)
        0b1000_0001, 0x00, 0x00,
        // Global Color Table §19 — 4 entries
        0x00, 0x00, 0x00,
        0xFF, 0x00, 0x00,
        0x00, 0xFF, 0x00,
        0x00, 0x00, 0xFF,
        // Comment Extension §24 — "hi"
        0x21, 0xFE, 0x02, b'h', b'i', 0x00,
        // Graphic Control Extension §23 — disposal 0, delay 10, no transparency
        0x21, 0xF9, 0x04, 0x00, 0x0A, 0x00, 0x00, 0x00,
        // Image Descriptor §20 — full screen, no LCT, not interlaced
        0x2C, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x02, 0x00, 0x00,
        // Table-Based Image Data §22 — mcs 2 then one §15 sub-block,
        // decoding to indices [0,1,2,3] (the proven 2×2 fixture stream).
        0x02, 0x03, 0x44, 0x64, 0x0A, 0x00,
        // Trailer §27
        0x3B,
    ];
    let img = decode(FIXTURE).expect("fixture decodes");
    let report = img.conformance_report();
    assert!(
        !report.has_errors(),
        "spec-derived fixture must have no conformance errors, got: {:?}",
        report.issues()
    );
}

/// Property: when a hand-mutated image trips a conformance *error* whose
/// rule the encoder *also* enforces fatally, `encode` rejects it; when
/// the report is clean, `encode` accepts. (The diagnostic surface is a
/// superset of the encoder's fatal checks — it also reports
/// recommendations and placement/range issues the encoder tolerates, so
/// the converse only holds for the shared rules.)
#[test]
fn shared_rules_agree_with_encoder() {
    // Clean image: encoder must accept.
    let clean = builder_animation();
    assert!(clean.conformance_report().is_clean());
    assert!(encode(&clean).is_ok());

    // Indices-length mismatch — a rule the encoder enforces fatally.
    // (`loop_forever()` puts the NETSCAPE Application block at index 0,
    // so locate the first Image block rather than assuming index 0.)
    let mut bad_len = builder_animation();
    let img_idx = bad_len
        .blocks
        .iter()
        .position(|b| matches!(b, Block::Image(_)))
        .unwrap();
    if let Block::Image(f) = &mut bad_len.blocks[img_idx] {
        f.indices.pop();
    }
    let report = bad_len.conformance_report();
    assert!(report
        .errors()
        .any(|i| i.rule == ConformanceRule::FrameIndicesLength));
    assert!(
        encode(&bad_len).is_err(),
        "an indices-length error must make encode fail too"
    );

    // Version too low — the encoder rejects an 87a stream carrying a GCE.
    let mut bad_ver = builder_animation();
    bad_ver.version = Version::Gif87a;
    let report = bad_ver.conformance_report();
    assert!(report
        .errors()
        .any(|i| i.rule == ConformanceRule::VersionTooLow));
    assert!(encode(&bad_ver).is_err());
}

/// Property: a §20.a frame-escapes-screen error and a pixel-range error
/// are *tolerated* by the encoder (it serialises whatever bytes it is
/// given for the placement rectangle and pixel buffer) — confirming the
/// diagnostic surface reaches beyond the encoder's fatal subset.
#[test]
fn placement_and_pixel_range_are_diagnostic_only() {
    // Frame escaping the screen: encoder still emits bytes.
    let palette = vec![Rgb::new(0, 0, 0), Rgb::new(255, 255, 255)];
    let escaping = GifImage {
        version: Version::Gif89a,
        screen_width: 2,
        screen_height: 2,
        color_resolution: 1,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(palette.clone()),
        blocks: vec![Block::Image(Frame {
            left: 5, // 5 + 2 = 7 > screen width 2
            top: 0,
            width: 2,
            height: 2,
            local_palette: None,
            palette_sorted: false,
            interlaced: false,
            indices: vec![0, 1, 1, 0],
            graphic_control: None,
        })],
    };
    let report = escaping.conformance_report();
    assert!(report
        .errors()
        .any(|i| i.rule == ConformanceRule::FrameEscapesScreen));
    assert!(
        encode(&escaping).is_ok(),
        "§20.a placement is diagnostic-only; the encoder tolerates it"
    );
}

/// Property: a recommendation-only report (§23.e.ii) is *not* clean but
/// *has no errors*, and the image still encodes.
#[test]
fn recommendation_only_image_encodes() {
    let palette = vec![Rgb::new(0, 0, 0), Rgb::new(255, 255, 255)];
    let img = GifImage {
        version: Version::Gif89a,
        screen_width: 2,
        screen_height: 2,
        color_resolution: 1,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(palette),
        blocks: vec![Block::Image(Frame {
            left: 0,
            top: 0,
            width: 2,
            height: 2,
            local_palette: None,
            palette_sorted: false,
            interlaced: false,
            indices: vec![0, 1, 1, 0],
            graphic_control: Some(GraphicControl {
                disposal: DisposalMethod::None,
                user_input: true,
                transparent_index: None,
                delay_centis: 0, // §23.e.ii recommendation
            }),
        })],
    };
    let report = img.conformance_report();
    assert!(!report.is_clean());
    assert!(!report.has_errors());
    assert_eq!(report.count(ConformanceSeverity::Recommendation), 1);
    assert!(encode(&img).is_ok());
}

/// Plain Text block referencing the GCT in range is conformant; out of
/// range is an error.
#[test]
fn plain_text_index_bounds() {
    let palette = vec![Rgb::new(0, 0, 0), Rgb::new(255, 255, 255)];
    let mk = |fg: u8, bg: u8| GifImage {
        version: Version::Gif89a,
        screen_width: 16,
        screen_height: 16,
        color_resolution: 1,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(palette.clone()),
        blocks: vec![Block::PlainText {
            params: PlainText {
                left: 0,
                top: 0,
                width: 16,
                height: 16,
                cell_width: 8,
                cell_height: 8,
                fg_color_index: fg,
                bg_color_index: bg,
                text: b"ok".to_vec(),
            },
            graphic_control: None,
        }],
    };
    assert!(mk(1, 0).conformance_report().is_clean());
    assert!(mk(9, 0)
        .conformance_report()
        .errors()
        .any(|i| i.rule == ConformanceRule::PlainTextIndexRange));
}

/// `validate_strict` end-to-end: a decoded, well-formed stream gates
/// `Ok`, and a hand-broken image gates `Err` whose message renders the
/// spec-cited issues.
#[test]
fn validate_strict_through_decode_and_mutation() {
    let img = builder_animation();
    let bytes = encode(&img).unwrap();
    let decoded = decode(&bytes).unwrap();
    assert!(
        decoded.validate_strict().is_ok(),
        "a decoded well-formed stream must validate strictly"
    );

    // Break a frame so it escapes the screen, then confirm the strict
    // gate rejects with a §20.a-cited message.
    let mut broken = decoded;
    let img_idx = broken
        .blocks
        .iter()
        .position(|b| matches!(b, Block::Image(_)))
        .unwrap();
    if let Block::Image(f) = &mut broken.blocks[img_idx] {
        f.width = 99; // overruns the 2-px screen
    }
    let err = broken
        .validate_strict()
        .expect_err("escaping frame rejected");
    assert!(format!("{err}").contains("§20.a"));
}
