//! The registry encoder's declared input layouts (round 477, contract
//! r476 ruling "encoders declare their accepted input layouts"): every
//! `PixelFormat` in `ACCEPTED_PIXEL_FORMATS` is coded as the table in
//! its docs says, anything else is a typed `Unsupported`, and the
//! encoder's output label is the coded layout (`Pal8`).

#![cfg(feature = "registry")]

use oxideav_core::{
    CodecId, CodecParameters, Error as CoreError, Frame as CoreFrame, PixelFormat, RuntimeContext,
    VideoFrame, VideoPlane,
};
use oxideav_gif::{ACCEPTED_PIXEL_FORMATS, CODEC_ID_STR, decode, make_decoder, make_encoder};

const W: u32 = 3;
const H: u32 = 2;

/// Reference picture, RGBA per pixel; pixel 4 is fully transparent
/// (its colour is not expected to survive — GIF keeps one transparent
/// index), every other pixel is opaque.
const REF: [[u8; 4]; 6] = [
    [255, 0, 0, 255],
    [0, 255, 0, 255],
    [0, 0, 255, 255],
    [10, 20, 30, 255],
    [200, 200, 200, 0],
    [7, 7, 7, 255],
];

/// Grey reference: `v` per pixel (opaque).
const GREY: [u8; 6] = [0, 255, 128, 10, 200, 7];

/// Mono reference: one bit per pixel.
const BITS: [u8; 6] = [1, 0, 1, 1, 0, 0];

fn ctx() -> RuntimeContext {
    let mut ctx = RuntimeContext::new();
    oxideav_gif::register(&mut ctx);
    ctx
}

fn params(pf: PixelFormat) -> CodecParameters {
    let mut p = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    p.width = Some(W);
    p.height = Some(H);
    p.pixel_format = Some(pf);
    p
}

fn le16(v: u16) -> [u8; 2] {
    v.to_le_bytes()
}

/// What the registry encoder expects to see for `pf`, from the
/// reference picture, plus the RGBA every pixel must decode to
/// (`None` alpha = transparent, colour unchecked).
fn frame_for(pf: PixelFormat, pad: usize) -> (VideoFrame, Vec<[u8; 4]>) {
    let w = W as usize;
    let h = H as usize;
    // Build packed rows then add `pad` bytes of row padding.
    let packed = |row_bytes: usize, bytes: Vec<u8>| -> VideoPlane {
        let mut data = Vec::new();
        for y in 0..h {
            data.extend_from_slice(&bytes[y * row_bytes..(y + 1) * row_bytes]);
            data.extend_from_slice(&vec![0xEE; pad]);
        }
        VideoPlane {
            stride: row_bytes + pad,
            data,
        }
    };
    let rgba_expect: Vec<[u8; 4]> = REF.to_vec();
    let rgb_expect: Vec<[u8; 4]> = REF.iter().map(|p| [p[0], p[1], p[2], 255]).collect();
    let grey_expect: Vec<[u8; 4]> = GREY.iter().map(|&v| [v, v, v, 255]).collect();
    let mono_expect = |one_is_white: bool| -> Vec<[u8; 4]> {
        BITS.iter()
            .map(|&b| {
                let v = if (b == 1) == one_is_white { 255 } else { 0 };
                [v, v, v, 255]
            })
            .collect()
    };
    let planes_and_expect: (Vec<VideoPlane>, Vec<[u8; 4]>) = match pf {
        PixelFormat::Rgba => (vec![packed(w * 4, REF.concat())], rgba_expect),
        PixelFormat::Bgra => (
            vec![packed(
                w * 4,
                REF.iter().flat_map(|p| [p[2], p[1], p[0], p[3]]).collect(),
            )],
            rgba_expect,
        ),
        PixelFormat::Argb => (
            vec![packed(
                w * 4,
                REF.iter().flat_map(|p| [p[3], p[0], p[1], p[2]]).collect(),
            )],
            rgba_expect,
        ),
        PixelFormat::Abgr => (
            vec![packed(
                w * 4,
                REF.iter().flat_map(|p| [p[3], p[2], p[1], p[0]]).collect(),
            )],
            rgba_expect,
        ),
        PixelFormat::Rgb24 => (
            vec![packed(
                w * 3,
                REF.iter().flat_map(|p| [p[0], p[1], p[2]]).collect(),
            )],
            rgb_expect,
        ),
        PixelFormat::Bgr24 => (
            vec![packed(
                w * 3,
                REF.iter().flat_map(|p| [p[2], p[1], p[0]]).collect(),
            )],
            rgb_expect,
        ),
        PixelFormat::Gbrp8 => (
            vec![
                packed(w, REF.iter().map(|p| p[1]).collect()),
                packed(w, REF.iter().map(|p| p[2]).collect()),
                packed(w, REF.iter().map(|p| p[0]).collect()),
            ],
            rgb_expect,
        ),
        PixelFormat::Gbrap8 => (
            vec![
                packed(w, REF.iter().map(|p| p[1]).collect()),
                packed(w, REF.iter().map(|p| p[2]).collect()),
                packed(w, REF.iter().map(|p| p[0]).collect()),
                packed(w, REF.iter().map(|p| p[3]).collect()),
            ],
            rgba_expect,
        ),
        PixelFormat::Gray8 => (vec![packed(w, GREY.to_vec())], grey_expect),
        PixelFormat::Ya8 => (
            vec![packed(
                w * 2,
                GREY.iter()
                    .zip(REF.iter())
                    .flat_map(|(&v, p)| [v, p[3]])
                    .collect(),
            )],
            GREY.iter()
                .zip(REF.iter())
                .map(|(&v, p)| [v, v, v, p[3]])
                .collect(),
        ),
        PixelFormat::MonoBlack | PixelFormat::MonoWhite => {
            // 3 pixels per row → one byte per row, MSB first.
            let rows: Vec<u8> = (0..h)
                .map(|y| (0..w).fold(0u8, |acc, x| acc | (BITS[y * w + x] << (7 - x))))
                .collect();
            (
                vec![packed(1, rows)],
                mono_expect(pf == PixelFormat::MonoBlack),
            )
        }
        PixelFormat::Rgb48Le => (
            vec![packed(
                w * 6,
                REF.iter()
                    .flat_map(|p| [p[0], p[1], p[2]])
                    .flat_map(|v| le16(u16::from(v) * 257))
                    .collect(),
            )],
            rgb_expect,
        ),
        PixelFormat::Rgba64Le => (
            vec![packed(
                w * 8,
                REF.iter()
                    .flatten()
                    .flat_map(|&v| le16(u16::from(v) * 257))
                    .collect(),
            )],
            rgba_expect,
        ),
        PixelFormat::Gbrp16Le => (
            vec![
                packed(
                    w * 2,
                    REF.iter()
                        .flat_map(|p| le16(u16::from(p[1]) * 257))
                        .collect(),
                ),
                packed(
                    w * 2,
                    REF.iter()
                        .flat_map(|p| le16(u16::from(p[2]) * 257))
                        .collect(),
                ),
                packed(
                    w * 2,
                    REF.iter()
                        .flat_map(|p| le16(u16::from(p[0]) * 257))
                        .collect(),
                ),
            ],
            rgb_expect,
        ),
        PixelFormat::Gbrap16Le => (
            vec![
                packed(
                    w * 2,
                    REF.iter()
                        .flat_map(|p| le16(u16::from(p[1]) * 257))
                        .collect(),
                ),
                packed(
                    w * 2,
                    REF.iter()
                        .flat_map(|p| le16(u16::from(p[2]) * 257))
                        .collect(),
                ),
                packed(
                    w * 2,
                    REF.iter()
                        .flat_map(|p| le16(u16::from(p[0]) * 257))
                        .collect(),
                ),
                packed(
                    w * 2,
                    REF.iter()
                        .flat_map(|p| le16(u16::from(p[3]) * 257))
                        .collect(),
                ),
            ],
            rgba_expect,
        ),
        PixelFormat::Gray16Le => (
            vec![packed(
                w * 2,
                GREY.iter()
                    .flat_map(|&v| le16(u16::from(v) * 257))
                    .collect(),
            )],
            grey_expect,
        ),
        PixelFormat::Gray12Le => (
            vec![packed(
                w * 2,
                GREY.iter().flat_map(|&v| le16(u16::from(v) << 4)).collect(),
            )],
            grey_expect,
        ),
        PixelFormat::Gray10Le => (
            vec![packed(
                w * 2,
                GREY.iter().flat_map(|&v| le16(u16::from(v) << 2)).collect(),
            )],
            grey_expect,
        ),
        PixelFormat::Ya16Le => (
            vec![packed(
                w * 4,
                GREY.iter()
                    .zip(REF.iter())
                    .flat_map(|(&v, p)| [le16(u16::from(v) * 257), le16(u16::from(p[3]) * 257)])
                    .flatten()
                    .collect(),
            )],
            GREY.iter()
                .zip(REF.iter())
                .map(|(&v, p)| [v, v, v, p[3]])
                .collect(),
        ),
        PixelFormat::Pal8 => {
            // Indices into a palette holding the reference colours.
            let mut f = VideoFrame {
                pts: None,
                planes: vec![packed(w, (0..6u8).collect())],
            };
            f.set_palette_rgba(&REF);
            return (f, rgba_expect);
        }
        other => panic!("no fixture for {other:?}"),
    };
    (
        VideoFrame {
            pts: None,
            planes: planes_and_expect.0,
        },
        planes_and_expect.1,
    )
}

fn encode_one(pf: PixelFormat, frame: VideoFrame) -> Vec<u8> {
    let mut enc = make_encoder(&params(pf)).unwrap();
    enc.send_frame(&CoreFrame::Video(frame)).unwrap();
    enc.receive_packet().unwrap().data
}

fn check(pf: PixelFormat, bytes: &[u8], expect: &[[u8; 4]]) {
    let img = decode(bytes).unwrap();
    assert_eq!(
        img.format,
        oxideav_gif::PixelFormat::Pal8,
        "{pf:?}: coded as Pal8"
    );
    let got = img.to_rgba8();
    for (i, want) in expect.iter().enumerate() {
        let px = &got[i * 4..i * 4 + 4];
        if want[3] == 0 {
            assert_eq!(px[3], 0, "{pf:?}: pixel {i} transparent");
        } else {
            assert_eq!(px, want, "{pf:?}: pixel {i}");
        }
    }
}

#[test]
fn every_declared_layout_codes_as_documented() {
    for &pf in ACCEPTED_PIXEL_FORMATS {
        for pad in [0usize, 5] {
            let (frame, expect) = frame_for(pf, pad);
            let bytes = encode_one(pf, frame);
            check(pf, &bytes, &expect);
        }
    }
    assert_eq!(ACCEPTED_PIXEL_FORMATS.len(), 21);
}

#[test]
fn registry_capabilities_declare_the_set_and_both_compression_flags() {
    let ctx = ctx();
    let impls = ctx.codecs.implementations(&CodecId::new(CODEC_ID_STR));
    let caps = &impls[0].caps;
    assert_eq!(caps.accepted_pixel_formats, ACCEPTED_PIXEL_FORMATS);
    assert!(caps.lossy && caps.lossless, "quantised or exact, by input");
    assert!(caps.encode && caps.decode && caps.intra_only);
    // The native layout leads; every alpha-capable 8-bit layout
    // precedes the opaque ones; the deep layouts come last.
    assert_eq!(ACCEPTED_PIXEL_FORMATS[0], PixelFormat::Pal8);
    let pos = |pf: PixelFormat| {
        ACCEPTED_PIXEL_FORMATS
            .iter()
            .position(|&f| f == pf)
            .unwrap()
    };
    assert!(pos(PixelFormat::Rgba) < pos(PixelFormat::Rgb24));
    assert!(pos(PixelFormat::Gray8) < pos(PixelFormat::Rgb48Le));
    assert!(pos(PixelFormat::Rgb24) < pos(PixelFormat::Gray8));
}

#[test]
fn undeclared_layouts_are_typed_unsupported_refusals() {
    for pf in [
        PixelFormat::Yuv420P,
        PixelFormat::Yuv444P,
        PixelFormat::Cmyk,
        PixelFormat::RgbF32Le,
        PixelFormat::Nv12,
    ] {
        let mut enc = make_encoder(&params(pf)).unwrap();
        let frame = VideoFrame {
            pts: None,
            planes: vec![VideoPlane {
                stride: 64,
                data: vec![0; 64 * 8],
            }],
        };
        let err = enc.send_frame(&CoreFrame::Video(frame)).unwrap_err();
        match err {
            CoreError::Unsupported(msg) => {
                assert!(msg.contains("accepted"), "{pf:?}: {msg}");
                assert!(
                    msg.contains("Pal8") && msg.contains("Gray10Le"),
                    "{pf:?}: {msg}"
                );
            }
            other => panic!("{pf:?}: expected Unsupported, got {other:?}"),
        }
        assert!(matches!(enc.receive_packet(), Err(CoreError::NeedMore)));
    }
}

#[test]
fn short_or_missing_planes_are_invalid_data() {
    // Gbrp8 with one plane.
    let mut enc = make_encoder(&params(PixelFormat::Gbrp8)).unwrap();
    let one = VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: 3,
            data: vec![0; 6],
        }],
    };
    assert!(matches!(
        enc.send_frame(&CoreFrame::Video(one)),
        Err(CoreError::InvalidData(_))
    ));
    // Rgb48Le plane that is one row short.
    let mut enc = make_encoder(&params(PixelFormat::Rgb48Le)).unwrap();
    let short = VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: 18,
            data: vec![0; 18],
        }],
    };
    assert!(matches!(
        enc.send_frame(&CoreFrame::Video(short)),
        Err(CoreError::InvalidData(_))
    ));
    // Mono row of 3 pixels needs one byte; a zero stride is short.
    let mut enc = make_encoder(&params(PixelFormat::MonoBlack)).unwrap();
    let zero = VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: 0,
            data: vec![],
        }],
    };
    assert!(matches!(
        enc.send_frame(&CoreFrame::Video(zero)),
        Err(CoreError::InvalidData(_))
    ));
}

#[test]
fn output_label_is_the_coded_layout() {
    for pf in [PixelFormat::Rgba, PixelFormat::Gray16Le, PixelFormat::Pal8] {
        let (frame, _) = frame_for(pf, 0);
        let mut enc = make_encoder(&params(pf)).unwrap();
        assert_eq!(
            enc.output_params().pixel_format,
            Some(PixelFormat::Pal8),
            "{pf:?}: coded label, not the input layout"
        );
        assert_eq!(enc.output_params().width, Some(W));
        enc.send_frame(&CoreFrame::Video(frame)).unwrap();
        let pkt = enc.receive_packet().unwrap();
        // Decoding with the output params yields exactly that layout.
        let mut dec = make_decoder(enc.output_params()).unwrap();
        dec.send_packet(&pkt).unwrap();
        let CoreFrame::Video(v) = dec.receive_frame().unwrap() else {
            panic!("non-video");
        };
        assert_eq!(v.planes[0].stride, W as usize, "{pf:?}: Pal8 plane");
        assert!(v.palette().is_some());
    }
}

#[test]
fn registry_and_layer1_produce_identical_bytes_for_the_same_request() {
    // `Rgba` through the registry == `encode` of the same `GifImage`.
    let (frame, _) = frame_for(PixelFormat::Rgba, 0);
    let bytes = encode_one(PixelFormat::Rgba, frame);
    let img = oxideav_gif::GifImage::from_rgba8(W, H, REF.concat()).unwrap();
    let direct = oxideav_gif::encode(&img, &oxideav_gif::EncodeOptions::default()).unwrap();
    assert_eq!(bytes, direct);
    // A reordered layout normalises to the same `Rgba` → same bytes.
    let (frame, _) = frame_for(PixelFormat::Abgr, 0);
    assert_eq!(encode_one(PixelFormat::Abgr, frame), direct);
    let (frame, _) = frame_for(PixelFormat::Rgba64Le, 3);
    assert_eq!(encode_one(PixelFormat::Rgba64Le, frame), direct);
}
