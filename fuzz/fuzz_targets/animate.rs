#![no_main]

//! The inter-frame animation paths.
//!
//! 1. `optimize` of any parseable stream must show exactly the same
//!    screens for the same times (per `compose`) and never be larger
//!    than its input.
//! 2. `encode_all` of frames built from the fuzz bytes must decode, keep
//!    the total duration, and — when the frames hold few enough colours
//!    for one table — show every frame exactly.

use std::time::Duration;

use libfuzzer_sys::fuzz_target;
use oxideav_gif::{
    DisposalMethod, EncodeOptions, Frame, GifImage, compose, decode_all, encode_all, optimize,
    parse,
};

/// Screens × pixels above which a stream is skipped (bounds the work).
const MAX_WORK: usize = 1 << 20;

/// A show reduced to what a viewer can tell apart: consecutive equal
/// screens run together, their delays added.
fn collapse(frames: Vec<(Vec<u8>, u64)>) -> Vec<(Vec<u8>, u64)> {
    let mut out: Vec<(Vec<u8>, u64)> = Vec::new();
    for (screen, delay) in frames {
        match out.last_mut() {
            Some((last, total)) if *last == screen => *total += delay,
            _ => out.push((screen, delay)),
        }
    }
    out
}

fn screens(bytes: &[u8]) -> Option<Vec<(Vec<u8>, u64)>> {
    let file = parse(bytes).ok()?;
    let frames = compose(&file).ok()?;
    Some(
        frames
            .into_iter()
            .map(|f| (f.canvas.pixels, u64::from(f.delay_centis)))
            .collect(),
    )
}

fuzz_target!(|data: &[u8]| {
    // ---- 1. lossless re-optimisation of an arbitrary stream ----
    if let Ok(file) = parse(data) {
        let work = file.screen_width as usize * file.screen_height as usize * file.frame_count();
        if work <= MAX_WORK
            && let Some(before) = screens(data)
            && !before.is_empty()
        {
            let smaller = optimize(data).expect("a parseable stream optimises");
            assert!(smaller.len() <= data.len(), "optimize grew the stream");
            let after = screens(&smaller).expect("the optimised stream composes");
            assert!(
                collapse(before) == collapse(after),
                "the optimised stream shows something else"
            );
        }
    }

    // ---- 2. encode_all from fuzz-built frames ----
    if data.len() < 8 {
        return;
    }
    let w = 1 + (data[0] as usize % 24);
    let h = 1 + (data[1] as usize % 24);
    let count = 1 + (data[2] as usize % 6);
    // Few colours (exact) or many (quantised), with or without alpha.
    let colour_mask = if data[3] & 1 == 0 { 0xC0 } else { 0xFF };
    let alpha = data[3] & 2 != 0;
    let body = &data[4..];
    let mut frames = Vec::with_capacity(count);
    for f in 0..count {
        let mut rgba = Vec::with_capacity(w * h * 4);
        for p in 0..w * h {
            let at = |k: usize| body[(f * 7 + p * 3 + k + (p / w) * f) % body.len()];
            let a = if alpha && at(3) & 3 == 0 { 0 } else { 255 };
            rgba.extend_from_slice(&[
                at(0) & colour_mask,
                at(1) & colour_mask,
                at(2) & colour_mask,
                a,
            ]);
        }
        let disposal = match body[f % body.len()] & 3 {
            0 => DisposalMethod::None,
            1 => DisposalMethod::Keep,
            2 => DisposalMethod::RestoreBackground,
            _ => DisposalMethod::RestorePrevious,
        };
        let delay = Duration::from_millis(10 * u64::from(body[(f + 1) % body.len()] % 8));
        let image = GifImage::from_rgba8(w as u32, h as u32, rgba).expect("frame");
        frames.push(Frame::new(image, Some(delay)).with_disposal(disposal));
    }
    let opts = EncodeOptions::default()
        .with_interlace(data[3] & 4 != 0)
        .with_temporal_tolerance(if data[3] & 8 != 0 { data[2] % 6 } else { 0 });
    let bytes = encode_all(&frames, &opts).expect("encode_all");
    let back = decode_all(&bytes).expect("decode of own animation");
    let total = |fs: &[Frame]| -> Duration { fs.iter().map(|f| f.delay.unwrap_or_default()).sum() };
    assert_eq!(total(&frames), total(&back), "total duration changed");

    // With the frames written as given the stream decodes too, and the
    // optimised one is the pictures' own (exact) rendering whenever the
    // colours fit: both show the same screens.
    if colour_mask == 0xC0 && opts.temporal_tolerance == 0 {
        let as_given = encode_all(&frames, &opts.clone().with_optimize_animation(false))
            .expect("encode_all as given");
        if let (Some(a), Some(b)) = (screens(&as_given), screens(&bytes)) {
            assert!(
                collapse(a) == collapse(b),
                "the optimised animation differs from the frames as given"
            );
        }
    }
});
