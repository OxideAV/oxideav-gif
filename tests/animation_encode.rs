//! The animation encoder: what it shows must be what it was given, in
//! far fewer bytes than one full image per frame.

#[path = "support/synth.rs"]
mod synth;

use std::time::Duration;

use oxideav_gif::quantize::Dither;
use oxideav_gif::{
    Block, ColorTableMode, DisposalMethod, EncodeOptions, Frame, GifFile, GifImage, decode_all,
    encode_all,
};

const W: usize = 96;
const H: usize = 72;

fn frames_of(raw: &[Vec<u8>], centis: u64) -> Vec<Frame> {
    raw.iter()
        .map(|f| {
            Frame::new(
                GifImage::from_rgba8(W as u32, H as u32, f.clone()).unwrap(),
                Some(Duration::from_millis(centis * 10)),
            )
        })
        .collect()
}

/// The picture on screen at each source frame's start time.
fn shown_for_each(source: &[Frame], out: &[Frame]) -> Vec<Vec<u8>> {
    let total = |fs: &[Frame]| -> Duration { fs.iter().map(|f| f.delay.unwrap_or_default()).sum() };
    assert_eq!(total(source), total(out), "total duration");
    let mut at = Duration::ZERO;
    let (mut k, mut until) = (0usize, out[0].delay.unwrap_or_default());
    let mut shown = Vec::new();
    for f in source {
        while at >= until && k + 1 < out.len() {
            k += 1;
            until += out[k].delay.unwrap_or_default();
        }
        shown.push(out[k].image.to_rgba8());
        at += f.delay.unwrap_or_default();
    }
    shown
}

fn assert_exact(source: &[Frame], bytes: &[u8]) {
    let out = decode_all(bytes).unwrap();
    for (i, (src, got)) in source.iter().zip(shown_for_each(source, &out)).enumerate() {
        assert_eq!(src.image.to_rgba8(), got, "frame {i}");
    }
}

fn images(bytes: &[u8]) -> Vec<oxideav_gif::GifFrameData> {
    GifFile::parse(bytes).unwrap().frames().cloned().collect()
}

#[test]
fn flat_animation_is_exact_and_much_smaller_than_full_frames() {
    let frames = frames_of(&synth::anim_cartoon(W, H, 24), 4);
    let bytes = encode_all(&frames, &EncodeOptions::default()).unwrap();
    assert_exact(&frames, &bytes);
    let as_given = EncodeOptions::default().with_optimize_animation(false);
    let naive = encode_all(&frames, &as_given).unwrap();
    assert!(
        bytes.len() * 2 < naive.len(),
        "{} vs {}",
        bytes.len(),
        naive.len()
    );
    // One shared table, nothing local, no "restore to background".
    let file = GifFile::parse(&bytes).unwrap();
    assert!(file.global_palette.is_some());
    assert!(!file.uses_disposal(DisposalMethod::RestoreBackground));
    // Later images are deltas: smaller rectangles than the screen.
    let imgs = images(&bytes);
    assert_eq!((imgs[0].width, imgs[0].height), (W as u16, H as u16));
    assert!(
        imgs[1..]
            .iter()
            .all(|f| (f.width as usize) * (f.height as usize) < W * H)
    );
}

#[test]
fn duplicate_frames_become_delays() {
    let raw = synth::anim_screen(W, H, 16);
    let frames = frames_of(&raw, 5);
    let bytes = encode_all(&frames, &EncodeOptions::default()).unwrap();
    let out = decode_all(&bytes).unwrap();
    // Every fourth source frame repeats the one before it.
    assert_eq!(out.len(), 12);
    assert_eq!(out[2].delay, Some(Duration::from_millis(100)));
    assert_eq!(out[0].delay, Some(Duration::from_millis(50)));
    shown_for_each(&frames, &out);

    // A frame that waits for user input is never merged away.
    let mut held = frames_of(&[raw[0].clone(), raw[0].clone(), raw[0].clone()], 5);
    held[1].user_input = true;
    let out = decode_all(&encode_all(&held, &EncodeOptions::default()).unwrap()).unwrap();
    assert_eq!(out.len(), 3);
    assert!(out[1].user_input && !out[0].user_input);

    // Delays that would overflow the 16-bit field stay separate.
    let mut long = frames_of(&[raw[0].clone(), raw[0].clone()], 1);
    long[0].delay = Some(Duration::from_millis(400_000));
    long[1].delay = Some(Duration::from_millis(400_000));
    let out = decode_all(&encode_all(&long, &EncodeOptions::default()).unwrap()).unwrap();
    assert_eq!(out.len(), 2);
}

/// A sprite moving over an empty (transparent) screen: every frame makes
/// pixels of the previous one transparent again, which no drawing can do.
#[test]
fn pixels_that_turn_transparent_are_handled_by_disposal() {
    let mut raw = Vec::new();
    for t in 0..10usize {
        let mut f = vec![0u8; W * H * 4];
        for y in 20..40 {
            for x in (5 + t * 7)..(25 + t * 7) {
                let o = (y * W + x) * 4;
                f[o..o + 4].copy_from_slice(&[200, (x % 8 * 30) as u8, (y % 4 * 60) as u8, 255]);
            }
        }
        // A fixed opaque bar that never vanishes.
        for x in 0..W {
            let o = ((H - 4) * W + x) * 4;
            f[o..o + 4].copy_from_slice(&[10, 10, 10, 255]);
        }
        raw.push(f);
    }
    let frames: Vec<Frame> = frames_of(&raw, 6)
        .into_iter()
        .map(|f| f.with_disposal(DisposalMethod::RestoreBackground))
        .collect();
    let bytes = encode_all(&frames, &EncodeOptions::default()).unwrap();
    assert_exact(&frames, &bytes);
    let file = GifFile::parse(&bytes).unwrap();
    assert!(file.uses_disposal(DisposalMethod::RestorePrevious));
    assert!(!file.uses_disposal(DisposalMethod::RestoreBackground));
}

#[test]
fn a_full_table_of_colours_stays_exact_across_frames() {
    // Exactly 256 colours over two frames: no room for a transparent
    // slot, the encoder still reproduces every pixel.
    let mut a = vec![0u8; W * H * 4];
    for (i, px) in a.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        *px = [(i % 256) as u8, 77, 200, 255];
    }
    let mut b = a.clone();
    for px in b.as_chunks_mut::<4>().0.iter_mut().take(W * 10) {
        px[0] = 255 - px[0];
    }
    let frames = frames_of(&[a, b], 10);
    let bytes = encode_all(&frames, &EncodeOptions::default()).unwrap();
    assert_exact(&frames, &bytes);
    let imgs = images(&bytes);
    assert_eq!(imgs[1].height, 10, "the second image is the changed band");
}

#[test]
fn colour_table_modes() {
    let frames = frames_of(&synth::anim_video(W, H, 4), 4);
    let table = |mode| {
        let bytes = encode_all(&frames, &EncodeOptions::default().with_color_table(mode)).unwrap();
        let file = GifFile::parse(&bytes).unwrap();
        let locals = file.frames().filter(|f| f.local_palette.is_some()).count();
        (file.global_palette.is_some(), locals)
    };
    assert_eq!(table(ColorTableMode::Global), (true, 0));
    assert_eq!(table(ColorTableMode::Local), (false, 4));

    // Left to the encoder: a scene cut gets a table of its own.
    let mut cut = synth::anim_video(W, H, 3);
    cut.push(synth::rgba(&synth::gradient(W, H)));
    let cut = frames_of(&cut, 4);
    let bytes = encode_all(&cut, &EncodeOptions::default()).unwrap();
    let imgs = images(&bytes);
    assert!(
        imgs[3].local_palette.is_some(),
        "the cut elects a local table"
    );
}

#[test]
fn few_colours_under_a_large_table_elect_a_small_one() {
    // 200 colours in the first frame; each later frame adds a two-colour
    // marker. Their images use two or three entries of a 256-entry table.
    let mut base = vec![0u8; W * H * 4];
    for (i, px) in base.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        *px = [((i / 7) % 200) as u8, 90, 30, 255];
    }
    let mut raw = vec![base.clone()];
    for t in 1..6usize {
        let mut f = raw[t - 1].clone();
        for y in 10..50 {
            for x in (t * 12)..(t * 12 + 10) {
                let o = (y * W + x) * 4;
                let c = if (x + y) % 2 == 0 {
                    [255, 255, 255, 255]
                } else {
                    [0, 0, 255, 255]
                };
                f[o..o + 4].copy_from_slice(&c);
            }
        }
        raw.push(f);
    }
    let frames = frames_of(&raw, 3);
    let bytes = encode_all(&frames, &EncodeOptions::default()).unwrap();
    assert_exact(&frames, &bytes);
    let imgs = images(&bytes);
    let small = imgs[1..]
        .iter()
        .filter(|f| f.local_palette.as_ref().is_some_and(|p| p.len() <= 8))
        .count();
    assert!(small >= 4, "{small} images elected a small local table");
    let forced = EncodeOptions::default().with_color_table(ColorTableMode::Global);
    let global_only = encode_all(&frames, &forced).unwrap();
    assert_exact(&frames, &global_only);
    assert!(bytes.len() < global_only.len());
}

/// Static regions keep the indices they were first given, whatever the
/// dither: the bytes of the regions that do not change are identical in
/// every decoded frame.
#[test]
fn static_regions_do_not_shimmer_under_any_dither() {
    let still = synth::photo(W, H, 9);
    let mut raw = Vec::new();
    for t in 0..6usize {
        let mut f = synth::rgba(&still);
        // A moving block in the top-left quarter.
        for y in 4..20 {
            for x in (4 + t * 5)..(16 + t * 5) {
                let o = (y * W + x) * 4;
                f[o..o + 4].copy_from_slice(&[250, 40, (t * 40) as u8, 255]);
            }
        }
        raw.push(f);
    }
    let frames = frames_of(&raw, 4);
    for dither in [Dither::None, Dither::BlueNoise, Dither::FloydSteinberg] {
        let bytes = encode_all(&frames, &EncodeOptions::default().with_dither(dither)).unwrap();
        let out = decode_all(&bytes).unwrap();
        assert_eq!(out.len(), 6);
        let first = out[0].image.to_rgba8();
        for (i, f) in out.iter().enumerate().skip(1) {
            let now = f.image.to_rgba8();
            // Rows below the block's band never change in the source.
            assert_eq!(
                now[24 * W * 4..],
                first[24 * W * 4..],
                "{dither:?} frame {i}"
            );
        }
        // And the later images do not cover those rows at all.
        assert!(images(&bytes)[1..].iter().all(|f| f.top + f.height <= 24));
    }
}

#[test]
fn temporal_tolerance_leaves_near_static_pixels_alone() {
    // A flat field with ±1 noise that changes every frame, plus a moving
    // marker.
    let mut rng = synth::Rng::new(4);
    let mut raw = Vec::new();
    for t in 0..8usize {
        let mut f = vec![0u8; W * H * 4];
        for px in f.as_chunks_mut::<4>().0.iter_mut() {
            let n = rng.below(3) as u8;
            *px = [99 + n, 120 + n, 140 + n, 255];
        }
        for y in 30..40 {
            for x in (t * 9)..(t * 9 + 9) {
                let o = (y * W + x) * 4;
                f[o..o + 4].copy_from_slice(&[255, 255, 0, 255]);
            }
        }
        raw.push(f);
    }
    let frames = frames_of(&raw, 4);
    let exact = encode_all(&frames, &EncodeOptions::default()).unwrap();
    assert_exact(&frames, &exact);
    let tolerant = encode_all(
        &frames,
        &EncodeOptions::default().with_temporal_tolerance(2),
    )
    .unwrap();
    assert!(
        tolerant.len() * 3 < exact.len(),
        "{} vs {}",
        tolerant.len(),
        exact.len()
    );
    // Never further than the tolerance from the source.
    let out = decode_all(&tolerant).unwrap();
    for (src, got) in frames.iter().zip(shown_for_each(&frames, &out)) {
        for (a, b) in src.image.to_rgba8().iter().zip(&got) {
            assert!((i32::from(*a) - i32::from(*b)).abs() <= 2);
        }
    }
}

#[test]
fn interlace_and_stream_fields_reach_every_image() {
    let frames = frames_of(&synth::anim_cartoon(W, H, 5), 4);
    let opts = EncodeOptions::default()
        .with_interlace(true)
        .with_loop_count(2u16)
        .with_comment("made by a test");
    let bytes = encode_all(&frames, &opts).unwrap();
    assert_exact(&frames, &bytes);
    let file = GifFile::parse(&bytes).unwrap();
    assert!(file.all_frames_interlaced());
    assert_eq!(file.loop_count(), Some(2));
    assert_eq!(file.comments().count(), 1);
    assert!(matches!(file.blocks[0], Block::Application(_)));
}

#[test]
fn a_single_frame_is_a_plain_still() {
    let frames = frames_of(&synth::anim_cartoon(W, H, 1), 0);
    let bytes = encode_all(&frames, &EncodeOptions::default().with_loop_count(None)).unwrap();
    assert_exact(&frames, &bytes);
    let imgs = images(&bytes);
    assert_eq!(imgs.len(), 1);
    assert!(
        imgs[0]
            .graphic_control
            .is_none_or(|g| g.transparent_index.is_none())
    );
}

// ---- lossless re-optimisation of an existing stream -----------------------

fn composed(bytes: &[u8]) -> Vec<(Vec<u8>, u16)> {
    let file = GifFile::parse(bytes).unwrap();
    oxideav_gif::compose(&file)
        .unwrap()
        .into_iter()
        .map(|f| (f.canvas.pixels, f.delay_centis))
        .collect()
}

/// Screens over time (one entry per centisecond tick that starts a
/// source frame).
fn assert_same_show(before: &[u8], after: &[u8]) {
    let (a, b) = (composed(before), composed(after));
    let total = |f: &[(Vec<u8>, u16)]| f.iter().map(|x| u32::from(x.1)).sum::<u32>();
    assert_eq!(total(&a), total(&b));
    let (mut k, mut until, mut at) = (0usize, u32::from(b[0].1), 0u32);
    for (i, (screen, delay)) in a.iter().enumerate() {
        while at >= until && k + 1 < b.len() {
            k += 1;
            until += u32::from(b[k].1);
        }
        assert_eq!(*screen, b[k].0, "source frame {i}");
        at += u32::from(*delay);
    }
}

#[test]
fn an_existing_animation_is_reoptimised_without_changing_what_it_shows() {
    // (The screen recording is quantised frame by frame when written as
    // given, so its frames differ a little everywhere; the cartoon is
    // exact and its frames differ only where something moves.)
    for (name, raw, gain) in [
        ("screen", synth::anim_screen(W, H, 16), 5),
        ("cartoon", synth::anim_cartoon(W, H, 16), 8),
    ] {
        let frames = frames_of(&raw, 4);
        // One full image per frame, each with its own table.
        let as_given = EncodeOptions::default().with_optimize_animation(false);
        let before = encode_all(&frames, &as_given.with_comment("kept")).unwrap();
        let after = oxideav_gif::optimize(&before).unwrap();
        assert!(
            after.len() * gain < before.len() * 4,
            "{name}: {} → {}",
            before.len(),
            after.len()
        );
        assert_same_show(&before, &after);
        let file = GifFile::parse(&after).unwrap();
        assert_eq!(file.loop_count(), Some(0), "{name}: looping kept");
        assert_eq!(file.comments().next(), Some(&b"kept"[..]));
        // A second pass finds nothing more.
        assert_eq!(oxideav_gif::optimize(&after).unwrap(), after);
    }
}

#[test]
fn reoptimisation_never_returns_something_larger_or_different() {
    // Already-minimal input: a one-pixel still.
    let tiny = oxideav_gif::encode_rgb8(1, 1, &[1, 2, 3], &EncodeOptions::default()).unwrap();
    assert_eq!(oxideav_gif::optimize(&tiny).unwrap(), tiny);

    // A stream the reader accepts and the writer would refuse as it
    // stands — an 87a header over 89a blocks — comes back unchanged, or
    // smaller and showing the same (fuzz finding).
    let frames = frames_of(&synth::anim_cartoon(W, H, 3), 4);
    for as_given in [true, false] {
        let opts = EncodeOptions::default().with_optimize_animation(!as_given);
        let mut mislabelled = encode_all(&frames, &opts).unwrap();
        mislabelled[..6].copy_from_slice(b"GIF87a");
        let out = oxideav_gif::optimize(&mislabelled).unwrap();
        assert!(out.len() <= mislabelled.len());
        assert_same_show(&mislabelled, &out);
        if as_given {
            assert!(out.len() < mislabelled.len());
            assert_eq!(&out[..6], b"GIF89a");
        }
    }

    // Streams built with every disposal method, sub-rectangles and
    // transparency (the random-animation generator of the fuzz seeds is
    // not available here; build a few by hand).
    use oxideav_gif::{AnimationBuilder, FrameSpec, Rgb};
    let palette = vec![
        Rgb::new(0, 0, 0),
        Rgb::new(255, 0, 0),
        Rgb::new(0, 255, 0),
        Rgb::new(0, 0, 255),
    ];
    for disposal in [
        DisposalMethod::None,
        DisposalMethod::Keep,
        DisposalMethod::RestoreBackground,
        DisposalMethod::RestorePrevious,
    ] {
        let mut b = AnimationBuilder::new(16, 12, palette.clone()).loop_forever();
        for t in 0..6u16 {
            let idx: Vec<u8> = (0..8 * 6).map(|i| ((i + t as usize) % 4) as u8).collect();
            b = b
                .add_frame(
                    FrameSpec::new(8, 6, idx)
                        .with_offset(t, t)
                        .with_delay_centis(5)
                        .with_disposal(disposal)
                        .with_transparent_index(3u8),
                )
                .unwrap();
        }
        let before = oxideav_gif::encode_file(&b.build().unwrap()).unwrap();
        let after = oxideav_gif::optimize(&before).unwrap();
        assert!(after.len() <= before.len(), "{disposal:?}");
        assert_same_show(&before, &after);
    }
}
