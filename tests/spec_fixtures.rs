//! Spec-derived hand-traced fixtures. These bytes were computed by
//! reading GIF89a §17–§22 and Appendix F line-by-line and walking the
//! LZW state machine on paper. Any drift from the spec text shows up
//! here first.

use oxideav_gif::{decode_gif, encode_gif, GifFrame, GifImage, GifVersion};

/// Build a 1x1 image with palette = [black, white, red, green],
/// pixel = palette index 1 (white). This is the smallest interesting
/// GIF: one Clear code + one literal + one EOI.
fn one_pixel_white() -> GifImage {
    GifImage {
        version: GifVersion::Gif87a,
        width: 1,
        height: 1,
        color_resolution: 1,
        global_palette_sorted: false,
        background_color_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(vec![
            0, 0, 0, // 0: black
            255, 255, 255, // 1: white
            255, 0, 0, // 2: red
            0, 255, 0, // 3: green
        ]),
        blocks: vec![oxideav_gif::GifBlock::Frame(GifFrame {
            left: 0,
            top: 0,
            width: 1,
            height: 1,
            local_palette: None,
            local_palette_sorted: false,
            interlaced: false,
            indices: vec![1],
            control: None,
        })],
    }
}

#[test]
fn encode_one_pixel_white_matches_hand_traced_bytes() {
    let img = one_pixel_white();
    let out = encode_gif(&img).unwrap();

    // Hand-trace the expected bytes per §17–§27:
    //
    // Header (§17): "GIF87a" — 6 bytes.
    // LSD (§18): width=1, height=1 (LE u16), packed (GCT=1, color_res=1,
    //   sort=0, gct_size_bits=1) -> 0b1_001_0_001 = 0x91, bg=0, par=0.
    // GCT (§19): 4 entries * 3 bytes = 12 bytes.
    // Image Descriptor (§20): 0x2C, left=0, top=0, w=1, h=1 (all LE u16),
    //   packed = 0 (no LCT, not interlaced, not sorted, lct_size=0).
    // Image Data (§22): LZW Min Code Size = 2 (palette has 4 entries
    //   = 2 bits). Then sub-blocks: count, then data bytes, then 0x00.
    //
    // LZW encoding of [1] at code_size=2:
    //   clear = 4, eoi = 5, initial width = 3.
    //   Emit Clear (100, 3 bits)
    //   current = 1; loop body has nothing (single pixel)
    //   Emit current=1 (001, 3 bits)
    //   Emit EOI=5 (101, 3 bits)
    //   9 bits total, packed LSB-first:
    //     bit stream: 0,0,1, 1,0,0, 1,0,1
    //     byte 0 (bit0..bit7): 0,0,1,1,0,0,1,0 -> 0b0100_1100 = 0x4C
    //     byte 1 (bit8 only):  1, then padded with zeros = 0x01
    //   So sub-block payload = [0x4C, 0x01] (2 bytes).
    //
    // Wrapped: count byte 0x02, payload, 0x00 terminator.
    //
    // Trailer (§27): 0x3B.
    let expected: Vec<u8> = vec![
        // Header
        b'G', b'I', b'F', b'8', b'7', b'a', // LSD
        0x01, 0x00, // width = 1
        0x01, 0x00, // height = 1
        0x91, // packed
        0x00, // bg
        0x00, // par
        // GCT (4 entries)
        0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 255, 0, // Image Descriptor
        0x2C, 0x00, 0x00, // left
        0x00, 0x00, // top
        0x01, 0x00, // w
        0x01, 0x00, // h
        0x00, // packed
        // Image Data
        0x02, // LZW Min Code Size
        0x02, // sub-block count
        0x4C, 0x01, // LZW bytes
        0x00, // sub-block terminator
        // Trailer
        0x3B,
    ];
    assert_eq!(
        out, expected,
        "encoded bytes don't match hand-traced fixture"
    );
}

#[test]
fn decode_then_encode_one_pixel_round_trip() {
    let img1 = one_pixel_white();
    let bytes1 = encode_gif(&img1).unwrap();
    let img2 = decode_gif(&bytes1).unwrap();
    let bytes2 = encode_gif(&img2).unwrap();
    assert_eq!(bytes1, bytes2, "decode/encode round-trip not stable");
    // Image equality too.
    assert_eq!(img1, img2);
}

#[test]
fn decode_two_by_two_round_trip() {
    // 2x2 image with all four palette entries — exercises a slightly
    // longer LZW stream than the 1x1 case.
    let img = GifImage {
        version: GifVersion::Gif87a,
        width: 2,
        height: 2,
        color_resolution: 1,
        global_palette_sorted: false,
        background_color_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(vec![0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 255, 0]),
        blocks: vec![oxideav_gif::GifBlock::Frame(GifFrame {
            left: 0,
            top: 0,
            width: 2,
            height: 2,
            local_palette: None,
            local_palette_sorted: false,
            interlaced: false,
            indices: vec![0, 1, 2, 3],
            control: None,
        })],
    };
    let bytes = encode_gif(&img).unwrap();
    let decoded = decode_gif(&bytes).unwrap();
    assert_eq!(img, decoded);
    assert_eq!(decoded.frames().next().unwrap().indices, vec![0, 1, 2, 3]);
}

#[test]
fn header_is_gif87a_when_no_extensions_used() {
    let img = one_pixel_white();
    let bytes = encode_gif(&img).unwrap();
    assert_eq!(&bytes[..6], b"GIF87a");
}

#[test]
fn header_is_gif89a_when_graphic_control_extension_present() {
    use oxideav_gif::{DisposalMethod, GraphicControl};
    let mut img = one_pixel_white();
    if let oxideav_gif::GifBlock::Frame(f) = &mut img.blocks[0] {
        f.control = Some(GraphicControl {
            disposal: DisposalMethod::RestoreToBackground,
            user_input: false,
            delay_cs: 50,
            transparent_index: Some(0),
        });
    }
    let bytes = encode_gif(&img).unwrap();
    assert_eq!(
        &bytes[..6],
        b"GIF89a",
        "presence of GCE must promote to 89a per §6"
    );

    // Round-trip preserves the GCE bytes.
    let decoded = decode_gif(&bytes).unwrap();
    if let oxideav_gif::GifBlock::Frame(f) = &decoded.blocks[0] {
        let gce = f.control.as_ref().unwrap();
        assert_eq!(gce.delay_cs, 50);
        assert_eq!(gce.transparent_index, Some(0));
    } else {
        panic!("decoded block 0 is not a Frame");
    }
}

#[test]
fn comment_extension_round_trip() {
    use oxideav_gif::{CommentExtension, GifBlock};
    let mut img = one_pixel_white();
    img.blocks.push(GifBlock::Comment(CommentExtension {
        data: b"hand-traced fixture".to_vec(),
    }));
    let bytes = encode_gif(&img).unwrap();
    assert_eq!(&bytes[..6], b"GIF89a"); // promoted by Comment Ext
    let decoded = decode_gif(&bytes).unwrap();
    let comment = decoded
        .blocks
        .iter()
        .find_map(|b| match b {
            GifBlock::Comment(c) => Some(c),
            _ => None,
        })
        .unwrap();
    assert_eq!(comment.data, b"hand-traced fixture");
}

#[test]
fn application_extension_round_trip_netscape_loop() {
    use oxideav_gif::{ApplicationExtension, GifBlock};
    // The NETSCAPE2.0 looping extension is documented outside the GIF
    // spec, but the *Application Extension* container that carries it
    // is spec'd at §26. Confirm we round-trip the container.
    let mut img = one_pixel_white();
    img.blocks.push(GifBlock::Application(ApplicationExtension {
        identifier: *b"NETSCAPE",
        auth_code: *b"2.0",
        data: vec![0x01, 0x00, 0x00], // loop forever
    }));
    let bytes = encode_gif(&img).unwrap();
    let decoded = decode_gif(&bytes).unwrap();
    let app = decoded
        .blocks
        .iter()
        .find_map(|b| match b {
            GifBlock::Application(a) => Some(a),
            _ => None,
        })
        .unwrap();
    assert_eq!(&app.identifier, b"NETSCAPE");
    assert_eq!(&app.auth_code, b"2.0");
    assert_eq!(app.data, vec![0x01, 0x00, 0x00]);
}

#[test]
fn interlaced_round_trip_preserves_pixels() {
    // 4-row image — exercises all four interlace passes.
    let mut indices = Vec::with_capacity(16);
    for r in 0..4u8 {
        for _c in 0..4u8 {
            indices.push(r);
        }
    }
    let img = GifImage {
        version: GifVersion::Gif87a,
        width: 4,
        height: 4,
        color_resolution: 1,
        global_palette_sorted: false,
        background_color_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(vec![0, 0, 0, 80, 80, 80, 160, 160, 160, 240, 240, 240]),
        blocks: vec![oxideav_gif::GifBlock::Frame(GifFrame {
            left: 0,
            top: 0,
            width: 4,
            height: 4,
            local_palette: None,
            local_palette_sorted: false,
            interlaced: true,
            indices: indices.clone(),
            control: None,
        })],
    };
    let bytes = encode_gif(&img).unwrap();
    let decoded = decode_gif(&bytes).unwrap();
    let frame = decoded.frames().next().unwrap();
    assert!(frame.interlaced, "interlace flag must round-trip");
    assert_eq!(
        frame.indices, indices,
        "de-interlaced indices must match original"
    );
}

#[test]
fn local_color_table_round_trip() {
    let img = GifImage {
        version: GifVersion::Gif87a,
        width: 2,
        height: 2,
        color_resolution: 1,
        global_palette_sorted: false,
        background_color_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: None, // no GCT — frame must carry its own LCT
        blocks: vec![oxideav_gif::GifBlock::Frame(GifFrame {
            left: 0,
            top: 0,
            width: 2,
            height: 2,
            local_palette: Some(vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120]),
            local_palette_sorted: false,
            interlaced: false,
            indices: vec![0, 1, 2, 3],
            control: None,
        })],
    };
    let bytes = encode_gif(&img).unwrap();
    let decoded = decode_gif(&bytes).unwrap();
    assert_eq!(img, decoded);
}

#[test]
fn probe_rejects_non_gif_input() {
    use oxideav_gif::probe;
    assert!(probe(b"\x89PNG\r\n").is_none());
    assert!(probe(b"BM\x00\x00").is_none());
    assert!(probe(b"").is_none());
}

#[test]
fn empty_input_is_rejected() {
    assert!(decode_gif(&[]).is_err());
}

#[test]
fn truncated_header_is_rejected() {
    assert!(decode_gif(b"GIF").is_err());
    assert!(decode_gif(b"GIF89").is_err());
}
