//! Hand-derived spec-conformance fixtures.
//!
//! Each fixture is built byte-by-byte from a literal reading of the
//! GIF89a specification: the constant `[u8; N]` arrays are produced by
//! tracing each field's syntax diagram in the specification and
//! placing the documented value at the documented offset, packed
//! according to the rules in §13 / §15 / §18 / §20 / §22 / §23 / §27
//! and Appendix F.

use oxideav_gif::{decode, encode, Block, DisposalMethod, GifImage, GraphicControl, Rgb, Version};

/// 1×1 pixel image, GIF87a, single colour palette of two entries
/// (smallest legal palette per §18.c.vi: actual entries = 2^(field+1),
/// minimum field = 0 → 2 entries).
///
/// LZW Minimum Code Size is forced to 2 by Appendix F's
/// "ESTABLISH CODE SIZE" black-and-white clause. With a 1-pixel
/// raster the LZW stream consists only of:
///   Clear (= 4) at width 3, the single pixel index 0 at width 3,
///   EOI (= 5) at width 3.
///
/// 9 bits → packed right-to-left:
///   Byte 0 bits[0..8] = (0,0,1,0,0,0,1,0) = 0x44
///   Byte 1 bit 0      = 1                 = 0x01
#[test]
fn one_by_one_gif87a_decodes_and_roundtrips() {
    #[rustfmt::skip]
    const FIXTURE: &[u8] = &[
        // Header §17 — 'G','I','F','8','7','a'
        0x47, 0x49, 0x46, 0x38, 0x37, 0x61,
        // Logical Screen Descriptor §18
        0x01, 0x00,             // width = 1
        0x01, 0x00,             // height = 1
        0b1000_0000,            // GCT flag=1, ColorRes=0, Sort=0, Size=0 (=> 2 entries)
        0x00,                   // background index
        0x00,                   // pixel aspect ratio
        // Global Color Table §19 — 2 entries × 3 bytes
        0x00, 0x00, 0x00,       // entry 0: black
        0xFF, 0xFF, 0xFF,       // entry 1: white
        // Image Descriptor §20
        0x2C,                   // image separator
        0x00, 0x00,             // left
        0x00, 0x00,             // top
        0x01, 0x00,             // width = 1
        0x01, 0x00,             // height = 1
        0x00,                   // packed: no LCT, no interlace, no sort, no LCT size
        // LZW Minimum Code Size §22.c.i
        0x02,                   // 2 (Appendix F floor)
        // Sub-block §15 framing — 2 bytes of payload then 0x00 terminator
        0x02, 0x44, 0x01,       // Clear(4) + 0 + EOI(5) at w=3, 9 bits LSB-packed.
        // §16 Sub-block sequence terminator
        0x00,
        // Trailer §27
        0x3B,
    ];

    let img = decode(FIXTURE).expect("1×1 GIF decodes");
    assert_eq!(img.version, Version::Gif87a);
    assert_eq!(img.screen_width, 1);
    assert_eq!(img.screen_height, 1);
    let palette = img.global_palette.as_ref().unwrap();
    assert_eq!(palette.len(), 2);
    assert_eq!(palette[0], Rgb::new(0, 0, 0));
    assert_eq!(palette[1], Rgb::new(255, 255, 255));
    let frame = img.frames().next().unwrap();
    assert_eq!(frame.width, 1);
    assert_eq!(frame.height, 1);
    assert_eq!(frame.indices, vec![0]);

    // Round-trip back to bytes.
    let re_encoded = encode(&img).expect("round-trip encode");
    let img2 = decode(&re_encoded).expect("re-decoded");
    assert_eq!(img2, img);
}

/// 2×2 pixel image, GIF89a, 4-colour palette, with a Graphic Control
/// Extension carrying a transparent index and a 5-centisecond delay.
///
/// Pixels: 0, 1, 2, 3 (top-left, top-right, bottom-left, bottom-right).
/// LZW Minimum Code Size = 2 (4-entry palette ⇒ 2 bits, ties Appendix F floor).
///
/// LZW emission for `[0,1,2,3]`:
///   Clear (= 4) at w=3, 0 at w=3, 1 at w=3,
///   bump w → 4 after assigning entry 7,
///   2 at w=4, 3 at w=4, EOI (= 5) at w=4.
/// Total bits = 3*3 + 3*4 = 21 bits → 3 bytes.
#[test]
fn two_by_two_gif89a_with_gce_decodes_and_roundtrips() {
    // Walk the LZW bit stream:
    //   bits[0..3]   = Clear (4) packed LSB-first → (0,0,1)
    //   bits[3..6]   = 0                          → (0,0,0)
    //   bits[6..9]   = 1                          → (1,0,0)
    //   bits[9..13]  = 2 at w=4                   → (0,1,0,0)
    //   bits[13..17] = 3 at w=4                   → (1,1,0,0)
    //   bits[17..21] = EOI=5 at w=4               → (1,0,1,0)
    // Byte 0 = bits[0..8]   = (0,0,1,0,0,0,1,0)   = 0x44
    // Byte 1 = bits[8..16]  = (0,0,1,0,0,1,1,0)   = 0x64
    // Byte 2 = bits[16..24] = (0,1,0,1,0,0,0,0)   = 0x0A (3 trailing zeros)
    #[rustfmt::skip]
    const FIXTURE: &[u8] = &[
        // Header §17 — GIF89a
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61,
        // Logical Screen Descriptor §18
        0x02, 0x00, 0x02, 0x00,
        // GCT flag=1, ColorRes=0, Sort=0, Size=1 ⇒ 2^(1+1)=4 entries.
        0b1000_0001,
        0x00,                       // background index
        0x00,                       // aspect
        // Global Color Table §19 — 4 RGB entries
        0x00, 0x00, 0x00,           // black
        0xFF, 0x00, 0x00,           // red
        0x00, 0xFF, 0x00,           // green
        0x00, 0x00, 0xFF,           // blue
        // Graphic Control Extension §23
        0x21,                       // extension introducer
        0xF9,                       // GCE label
        0x04,                       // block size = 4
        // packed: reserved=0, disposal=2 (RestoreBackground), user_input=0, transparency=1
        // bit layout: rrr ddd u t = 000 010 0 1 = 0b00001001 = 0x09
        0x09,
        0x05, 0x00,                 // delay = 5 centiseconds
        0x03,                       // transparent index = 3
        0x00,                       // block terminator
        // Image Descriptor §20
        0x2C,
        0x00, 0x00, 0x00, 0x00,     // left, top
        0x02, 0x00, 0x02, 0x00,     // width=2, height=2
        0x00,                       // packed: no LCT, no interlace
        // LZW Minimum Code Size §22.c.i
        0x02,
        // Sub-block §15: 3 bytes
        0x03, 0x44, 0x64, 0x0A,
        0x00,                       // §16 terminator
        // Trailer §27
        0x3B,
    ];

    let img = decode(FIXTURE).expect("2×2 GIF decodes");
    assert_eq!(img.version, Version::Gif89a);
    assert_eq!(img.screen_width, 2);
    assert_eq!(img.screen_height, 2);
    let frame = img.frames().next().unwrap();
    assert_eq!(frame.indices, vec![0, 1, 2, 3]);
    let gce = frame.graphic_control.expect("GCE attached");
    assert_eq!(gce.disposal, DisposalMethod::RestoreBackground);
    assert!(!gce.user_input);
    assert_eq!(gce.transparent_index, Some(3));
    assert_eq!(gce.delay_centis, 5);

    // Round-trip equality.
    let re_encoded = encode(&img).expect("encode round-trip");
    let img2 = decode(&re_encoded).expect("decode round-trip");
    assert_eq!(img2, img);
}

/// Comment Extension §24 — a two-sub-block payload split exactly at
/// the maximum sub-block length (255 + 7 bytes).
#[test]
fn comment_extension_split_payload_roundtrips() {
    let mut payload = vec![b'a'; 255];
    payload.extend_from_slice(b"; tail\n");
    let img = GifImage {
        version: Version::Gif89a,
        screen_width: 1,
        screen_height: 1,
        color_resolution: 0,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(vec![Rgb::new(0, 0, 0), Rgb::new(255, 255, 255)]),
        blocks: vec![Block::Comment(payload.clone())],
    };
    let bytes = encode(&img).unwrap();
    let img2 = decode(&bytes).unwrap();
    assert_eq!(img2.blocks, vec![Block::Comment(payload)]);
}

/// Application Extension §26 — exercise the 8-byte identifier and
/// 3-byte authentication code.
#[test]
fn application_extension_roundtrips() {
    use oxideav_gif::Application;
    let img = GifImage {
        version: Version::Gif89a,
        screen_width: 1,
        screen_height: 1,
        color_resolution: 0,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(vec![Rgb::new(0, 0, 0), Rgb::new(255, 255, 255)]),
        blocks: vec![Block::Application(Application {
            identifier: *b"OXIDEAV1",
            auth_code: *b"01x",
            data: b"hello, world".to_vec(),
        })],
    };
    let bytes = encode(&img).unwrap();
    let img2 = decode(&bytes).unwrap();
    assert_eq!(img2, img);
}

/// Interlaced encode → interlaced decode round-trip. Verifies the
/// Appendix E row order matches between the two paths.
#[test]
fn interlaced_image_roundtrip() {
    use oxideav_gif::Frame;
    let w = 16u16;
    let h = 20u16;
    let palette: Vec<Rgb> = (0u8..16)
        .map(|i| {
            Rgb::new(
                i.wrapping_mul(16),
                255 - i.wrapping_mul(16),
                i.wrapping_mul(32),
            )
        })
        .collect();
    // Per-row gradient so we can detect any row-order misorganisation.
    let mut indices = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            indices.push(((y as usize + x as usize) % 16) as u8);
        }
    }
    let frame = Frame {
        left: 0,
        top: 0,
        width: w,
        height: h,
        local_palette: None,
        palette_sorted: false,
        interlaced: true,
        indices: indices.clone(),
        graphic_control: None,
    };
    let img = GifImage {
        version: Version::Gif89a,
        screen_width: w,
        screen_height: h,
        color_resolution: 3,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(palette),
        blocks: vec![Block::Image(frame)],
    };
    let bytes = encode(&img).unwrap();
    let img2 = decode(&bytes).unwrap();
    let frame2 = img2.frames().next().unwrap();
    assert!(frame2.interlaced);
    assert_eq!(frame2.indices, indices, "de-interlaced raster differs");
}

/// Multi-frame stream — three images sharing a global palette, the
/// second one carrying its own GCE with a non-default disposal.
#[test]
fn multi_frame_stream_roundtrips() {
    use oxideav_gif::Frame;
    let palette = vec![
        Rgb::new(0, 0, 0),
        Rgb::new(0xFF, 0, 0),
        Rgb::new(0, 0xFF, 0),
        Rgb::new(0, 0, 0xFF),
    ];
    let make_frame = |fill: u8, gce: Option<GraphicControl>| -> Frame {
        Frame {
            left: 0,
            top: 0,
            width: 4,
            height: 4,
            local_palette: None,
            palette_sorted: false,
            interlaced: false,
            indices: vec![fill; 16],
            graphic_control: gce,
        }
    };
    let img = GifImage {
        version: Version::Gif89a,
        screen_width: 4,
        screen_height: 4,
        color_resolution: 1,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(palette),
        blocks: vec![
            Block::Image(make_frame(0, None)),
            Block::Image(make_frame(
                1,
                Some(GraphicControl {
                    disposal: DisposalMethod::Keep,
                    user_input: false,
                    transparent_index: None,
                    delay_centis: 50,
                }),
            )),
            Block::Image(make_frame(2, None)),
        ],
    };
    let bytes = encode(&img).unwrap();
    let img2 = decode(&bytes).unwrap();
    assert_eq!(img2, img);
}

/// Empty-trailer-only Data Stream — Appendix B grammar permits zero
/// `<Data>` elements when only a global palette load is intended.
#[test]
fn header_only_stream_roundtrips() {
    let img = GifImage {
        version: Version::Gif87a,
        screen_width: 0,
        screen_height: 0,
        color_resolution: 0,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: None,
        blocks: vec![],
    };
    let bytes = encode(&img).unwrap();
    let img2 = decode(&bytes).unwrap();
    assert_eq!(img2, img);
}

/// Trailer ends the stream — bytes past 0x3B are ignored.
#[test]
fn trailing_garbage_after_trailer_is_ignored() {
    let mut bytes = encode(&GifImage {
        version: Version::Gif87a,
        screen_width: 0,
        screen_height: 0,
        color_resolution: 0,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: None,
        blocks: vec![],
    })
    .unwrap();
    bytes.extend_from_slice(b"trailing junk");
    decode(&bytes).expect("trailing data after trailer must not break decode");
}
