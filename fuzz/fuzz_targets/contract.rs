#![no_main]

//! Image-crate contract (`IMAGE_CRATE_API`) surface fuzz harness:
//! `probe` / `info` / `decode` / `decode_with` / `decode_rgb8` /
//! `decode_rgba8` / `decode_all` on arbitrary bytes, then the contract
//! encoder on whatever decoded.
//!
//! Contract: every called function returns to its caller — a `panic!`,
//! slice-OOB, integer overflow or OOM abort is a finding. Two
//! equivalences are asserted on top of panic-freedom:
//!
//! * `info` predicts `decode`: same dimensions, same layout, same
//!   `has_alpha`, and `frames` equals `decode_all(..).len()` whenever
//!   both succeed.
//! * the `Pal8` round trip is lossless: `decode(encode(img)) == img`
//!   for every image `decode` produced.
//!
//! `DecodeOptions::max_bytes` keeps the canvases small so the harness
//! spends its budget on parsing, not on allocating 65535² screens.

use libfuzzer_sys::fuzz_target;
use oxideav_gif::{
    decode_all_with, decode_rgb8, decode_rgba8, decode_with, encode, info, probe, DecodeOptions,
    EncodeOptions, PixelFormat,
};

// 1 MiB of decoded data per call: enough for every realistic fixture,
// small enough that `frames × canvas` can never OOM the fuzzer.
const MAX_BYTES: u64 = 1 << 20;

fuzz_target!(|data: &[u8]| {
    let _ = probe(data);
    let info = info(data);

    let opts = DecodeOptions::default().with_max_bytes(MAX_BYTES);
    let strict = DecodeOptions::default()
        .with_max_bytes(MAX_BYTES)
        .with_strict(true);
    let lenient = DecodeOptions::default()
        .with_max_bytes(MAX_BYTES)
        .with_lenient(true);
    let _ = decode_with(data, &strict);
    let _ = decode_with(data, &lenient);
    let _ = decode_all_with(data, &lenient);

    let img = decode_with(data, &opts);
    let frames = decode_all_with(data, &opts);

    if let (Ok(i), Ok(img)) = (&info, &img) {
        assert_eq!(
            (i.width, i.height),
            (img.width, img.height),
            "info vs decode size"
        );
        assert_eq!(i.format, img.format, "info vs decode layout");
        assert_eq!(i.has_alpha, img.has_alpha(), "info vs decode alpha");
        if let Ok(frames) = &frames {
            assert_eq!(i.frames as usize, frames.len(), "info.frames vs decode_all");
        }
    }

    if let Ok(img) = img {
        // The raw paths are the image kernels; both must agree with the
        // decoded image (bounded by the same limits via `decode`, so
        // skip them when the default limits would differ).
        if img.width as u64 * img.height as u64 * 4 <= MAX_BYTES {
            if let Ok(rgba) = decode_rgba8(data) {
                assert_eq!(rgba.data, img.to_rgba8());
            }
            if let Ok(rgb) = decode_rgb8(data) {
                assert_eq!(rgb.data, img.to_rgb8());
            }
        }
        match encode(&img, &EncodeOptions::default()) {
            Ok(bytes) => {
                let again = decode_with(&bytes, &opts).expect("re-decode of encoder output");
                if img.format == PixelFormat::Pal8 {
                    assert_eq!(again, img, "Pal8 round trip is lossless");
                } else {
                    // Rgba (256 opaque entries + transparent pixels) is
                    // quantised on encode; the geometry survives.
                    assert_eq!((again.width, again.height), (img.width, img.height));
                }
            }
            Err(e) => panic!("encode refused a decoder-produced image: {e}"),
        }
    }

    if let Ok(frames) = frames {
        for f in &frames {
            let _ = f.image.to_rgba8();
        }
        // Re-encoding every composited frame as an animation must work
        // for any decodable stream (bounded by the frame cap above).
        if !frames.is_empty() && frames.len() <= 16 {
            let _ = oxideav_gif::encode_animation(&frames, &EncodeOptions::default());
        }
    }
});
