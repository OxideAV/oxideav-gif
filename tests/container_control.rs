//! Caller control over the GIF container structure (#14): header
//! version, Logical Screen size, Background Color Index, Pixel Aspect
//! Ratio, global vs local colour tables, per-frame Graphic Control
//! (disposal, delay, Transparency Index, User Input Flag), placement and
//! interlace, NETSCAPE2.0 iterations (none written for one pass) and §24
//! comments — on write, and read back from the bytes.

use oxideav_gif::{
    AnimationBuilder, Block, ColorTableMode, DisposalMethod, EncodeOptions, Error, Frame,
    FrameSpec, GifFile, GifImage, GraphicControl, QuantizeOptions, Rgb, Version, encode,
    encode_all, encode_file, info,
};

fn pal(n: u8) -> Vec<Rgb> {
    (0..n).map(|i| Rgb::new(i * 30, 255 - i * 30, i)).collect()
}

/// Bytes 0x00..0x0D: header + Logical Screen Descriptor fields.
fn header_fields(bytes: &[u8]) -> (&[u8], u16, u16, u8, u8, u8) {
    (
        &bytes[..6],
        u16::from_le_bytes([bytes[6], bytes[7]]),
        u16::from_le_bytes([bytes[8], bytes[9]]),
        bytes[10],
        bytes[11],
        bytes[12],
    )
}

fn netscape_blocks(bytes: &[u8]) -> usize {
    bytes.windows(11).filter(|w| w == b"NETSCAPE2.0").count()
}

#[test]
fn builder_controls_every_header_and_per_frame_field() {
    let file = AnimationBuilder::new(10, 8, pal(4))
        .version(Version::Gif89a)
        .background_index(3)
        .pixel_aspect_ratio(49)
        .iterations(4)
        .comment("made by oxideav")
        .comment(b"second".to_vec())
        .add_frame(FrameSpec::new(10, 8, vec![1; 80]).with_delay_centis(7))
        .unwrap()
        .add_frame(
            FrameSpec::new(3, 2, vec![0, 1, 2, 3, 4, 5])
                .with_offset(4, 5)
                .with_local_palette(pal(8))
                .with_palette_sorted(true)
                .with_interlaced(true)
                .with_disposal(DisposalMethod::RestorePrevious)
                .with_transparent_index(5)
                .with_user_input(true)
                .with_delay_centis(250),
        )
        .unwrap()
        .add_frame(FrameSpec::new(2, 2, vec![2; 4]).without_graphic_control())
        .unwrap()
        .build()
        .unwrap();
    let bytes = encode_file(&file).unwrap();

    let (sig, w, h, packed, bg, par) = header_fields(&bytes);
    assert_eq!(sig, b"GIF89a");
    assert_eq!((w, h, bg, par), (10, 8, 3, 49));
    assert_ne!(packed & 0x80, 0, "Global Color Table Flag");
    assert_eq!(netscape_blocks(&bytes), 1);

    let back = GifFile::parse(&bytes).unwrap();
    assert_eq!(back, file, "byte round trip keeps every field");
    assert_eq!(back.iterations(), 4);
    assert_eq!(back.loop_count(), Some(3));
    assert_eq!(
        back.comments().collect::<Vec<_>>(),
        vec![&b"made by oxideav"[..], &b"second"[..]]
    );
    let frames: Vec<_> = back.frames().collect();
    assert_eq!(frames.len(), 3);
    assert!(frames[0].local_palette.is_none());
    let f1 = frames[1];
    assert_eq!((f1.left, f1.top, f1.width, f1.height), (4, 5, 3, 2));
    assert_eq!(f1.local_palette.as_deref(), Some(&pal(8)[..]));
    assert!(f1.palette_sorted && f1.interlaced);
    assert_eq!(
        f1.graphic_control,
        Some(GraphicControl {
            disposal: DisposalMethod::RestorePrevious,
            user_input: true,
            transparent_index: Some(5),
            delay_centis: 250,
        })
    );
    assert_eq!(frames[2].graphic_control, None);

    let i = info(&bytes).unwrap();
    assert_eq!(i.background_index, 3);
    assert_eq!(i.pixel_aspect_ratio, 49);
    assert_eq!(i.loop_count, Some(3));
    assert_eq!(i.comments.len(), 2);
}

#[test]
fn gif87a_streams_are_buildable_and_89a_needs_are_refused() {
    // No GCE, no extension: GIF87a by default (§7 earliest version).
    let still = AnimationBuilder::new(2, 1, pal(2))
        .add_frame(FrameSpec::new(2, 1, vec![0, 1]).without_graphic_control())
        .unwrap()
        .add_frame(FrameSpec::new(1, 1, vec![1]).without_graphic_control())
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(still.version, Version::Gif87a);
    assert_eq!(&encode_file(&still).unwrap()[..6], b"GIF87a");

    // Forcing 89a on the same blocks is honoured.
    let forced = AnimationBuilder::new(2, 1, pal(2))
        .version(Version::Gif89a)
        .add_frame(FrameSpec::new(2, 1, vec![0, 1]).without_graphic_control())
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(&encode_file(&forced).unwrap()[..6], b"GIF89a");

    // A GCE (or a comment / loop block) cannot be written as 87a.
    let err = AnimationBuilder::new(2, 1, pal(2))
        .version(Version::Gif87a)
        .add_frame(FrameSpec::new(2, 1, vec![0, 1]))
        .unwrap()
        .build();
    assert!(matches!(err, Err(Error::InvalidInput(_))));
    let err = AnimationBuilder::new(2, 1, pal(2))
        .version(Version::Gif87a)
        .comment("x")
        .add_frame(FrameSpec::new(2, 1, vec![0, 1]).without_graphic_control())
        .unwrap()
        .build();
    assert!(matches!(err, Err(Error::InvalidInput(_))));
}

#[test]
fn local_tables_only_stream() {
    let file = AnimationBuilder::new(2, 2, Vec::new())
        .global_palette(None)
        .add_frame(FrameSpec::new(2, 2, vec![0, 1, 1, 0]).with_local_palette(pal(2)))
        .unwrap()
        .build()
        .unwrap();
    let bytes = encode_file(&file).unwrap();
    assert_eq!(bytes[10] & 0x80, 0, "no Global Color Table");
    assert!(GifFile::parse(&bytes).unwrap().global_palette.is_none());

    // A frame without any colour table is refused.
    let err = AnimationBuilder::new(2, 2, Vec::new())
        .global_palette(None)
        .add_frame(FrameSpec::new(2, 2, vec![0; 4]));
    assert!(matches!(err, Err(Error::InvalidInput(_))));
    // Transparency Index past the table is refused.
    let err = AnimationBuilder::new(1, 1, pal(2))
        .add_frame(FrameSpec::new(1, 1, vec![0]).with_transparent_index(2));
    assert!(matches!(err, Err(Error::InvalidInput(_))));
}

#[test]
fn rgba_frame_spec_carries_its_own_table_and_transparency() {
    let rgba = [255, 0, 0, 255, 0, 0, 0, 0, 0, 255, 0, 255, 0, 0, 255, 255];
    let spec = FrameSpec::from_rgba8(2, 2, &rgba, QuantizeOptions::default()).unwrap();
    let t = spec.graphic_control.unwrap().transparent_index.unwrap();
    assert_eq!(spec.indices[1], t);
    let file = AnimationBuilder::new(4, 4, Vec::new())
        .global_palette(None)
        .add_frame(spec.with_offset(2, 1))
        .unwrap()
        .build()
        .unwrap();
    let canvas = &file.frames_composited().unwrap()[0].image;
    let px = canvas.to_rgba8();
    let at = |x: usize, y: usize| &px[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
    assert_eq!(at(2, 1), [255, 0, 0, 255]);
    assert_eq!(at(3, 1)[3], 0);
    assert_eq!(at(3, 2), [0, 0, 255, 255]);
    assert!(FrameSpec::from_rgba8(2, 2, &rgba[..12], QuantizeOptions::default()).is_err());
}

#[test]
fn iterations_convention_and_play_once_writes_no_extension() {
    let frame = |c: u8| {
        Frame::new(
            GifImage::from_rgb8(2, 2, vec![c; 12]).unwrap(),
            Some(core::time::Duration::from_millis(100)),
        )
    };
    let frames = [frame(10), frame(200)];
    for (iterations, want) in [(0, Some(0)), (1, None), (2, Some(1)), (5, Some(4))] {
        let bytes = encode_all(&frames, &EncodeOptions::new().with_iterations(iterations)).unwrap();
        let file = GifFile::parse(&bytes).unwrap();
        assert_eq!(file.loop_count(), want, "iterations {iterations}");
        assert_eq!(file.iterations(), iterations);
        assert_eq!(netscape_blocks(&bytes), usize::from(want.is_some()));
    }
    // Saturates at one pass + 65535 repeats.
    assert_eq!(
        EncodeOptions::new().with_iterations(1 << 20).loop_count,
        Some(u16::MAX)
    );
}

#[test]
fn encode_options_set_the_stream_fields_of_a_still_image() {
    let img = GifImage::from_rgb8(2, 1, vec![1, 2, 3, 4, 5, 6]).unwrap();
    // Default: plain still image is GIF87a with a Global Color Table.
    let plain = encode(&img, &EncodeOptions::new()).unwrap();
    assert_eq!(&plain[..6], b"GIF87a");

    let opts = EncodeOptions::new()
        .with_background_index(1)
        .with_pixel_aspect_ratio(15)
        .with_comment("hello")
        .with_color_table(ColorTableMode::Local);
    let bytes = encode(&img, &opts).unwrap();
    let (sig, _, _, packed, bg, par) = header_fields(&bytes);
    assert_eq!((sig, bg, par), (&b"GIF89a"[..], 1, 15));
    assert_eq!(packed & 0x80, 0, "local mode writes no Global Color Table");
    let file = GifFile::parse(&bytes).unwrap();
    assert!(file.frames().next().unwrap().local_palette.is_some());
    assert_eq!(file.comments().collect::<Vec<_>>(), vec![&b"hello"[..]]);
    assert!(file.comments_in_recommended_position());
    assert_eq!(
        oxideav_gif::decode_rgb8(&bytes).unwrap().data,
        img.to_rgb8()
    );

    // Forced 89a without any extension.
    let v89 = encode(&img, &EncodeOptions::new().with_version(Version::Gif89a)).unwrap();
    assert_eq!(&v89[..6], b"GIF89a");
    // 87a with a comment is refused.
    let err = encode(
        &img,
        &EncodeOptions::new()
            .with_version(Version::Gif87a)
            .with_comment("x"),
    );
    assert!(matches!(err, Err(Error::InvalidInput(_))));
}

#[test]
fn encode_all_global_table_mode_shares_one_palette() {
    let frames: Vec<Frame> = [[255u8, 0, 0], [0, 0, 255]]
        .iter()
        .map(|c| Frame::new(GifImage::from_rgb8(1, 1, c.to_vec()).unwrap(), None))
        .collect();
    let local = GifFile::parse(&encode_all(&frames, &EncodeOptions::new()).unwrap()).unwrap();
    assert!(local.frames().all(|f| f.local_palette.is_some()));
    let opts = EncodeOptions::new()
        .with_color_table(ColorTableMode::Global)
        .with_comments(["a", "b"]);
    let bytes = encode_all(&frames, &opts).unwrap();
    let global = GifFile::parse(&bytes).unwrap();
    assert!(global.global_palette.is_some());
    assert!(global.frames().all(|f| f.local_palette.is_none()));
    assert_eq!(global.comments().count(), 2);
    let decoded = oxideav_gif::decode_all(&bytes).unwrap();
    assert_eq!(decoded[0].image.to_rgb8(), vec![255, 0, 0]);
    assert_eq!(decoded[1].image.to_rgb8(), vec![0, 0, 255]);
}

#[test]
fn editing_a_parsed_file_rewrites_loop_and_comments() {
    let src = AnimationBuilder::new(1, 1, pal(2))
        .loop_forever()
        .comment("old")
        .add_full_frame(vec![0], 5, DisposalMethod::None)
        .unwrap()
        .add_full_frame(vec![1], 5, DisposalMethod::None)
        .unwrap()
        .build()
        .unwrap();
    let mut file = GifFile::parse(&encode_file(&src).unwrap()).unwrap();
    file.set_iterations(1).unwrap();
    file.set_comments(["new 1", "new 2"]);
    file.screen_width = 3;
    file.background_index = 1;
    let bytes = encode_file(&file).unwrap();
    assert_eq!(netscape_blocks(&bytes), 0);
    let back = GifFile::parse(&bytes).unwrap();
    assert_eq!(back.iterations(), 1);
    assert_eq!(
        back.comments().collect::<Vec<_>>(),
        vec![&b"new 1"[..], &b"new 2"[..]]
    );
    assert_eq!((back.screen_width, back.background_index), (3, 1));
    assert!(matches!(back.blocks[0], Block::Comment(_)));
}
