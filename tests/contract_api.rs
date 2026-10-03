//! The image-crate contract (`IMAGE_CRATE_API`) surface, exercised
//! end-to-end against the depth API it is built on: `decode` must show
//! what `compose` shows for the first frame, `decode_all` must equal
//! `compose` frame for frame, `info` must predict `decode`'s layout and
//! `decode_all`'s length, and the `Pal8` round trip must be lossless.

use std::time::Duration;

use oxideav_gif::{
    compose, decode, decode_all, decode_rgb8, decode_rgba8, decode_with, encode, encode_all,
    encode_animation, encode_file, encode_rgb8, encode_rgba8, info, parse, probe, AnimationBuilder,
    DecodeOptions, DisposalMethod, EncodeOptions, Error, Frame, GifImage, Palette, PixelFormat,
    Rgb,
};

/// Tiny deterministic PRNG so the fixtures need no dev-dependency.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

/// A randomised animation: sub-rectangle frames, every disposal
/// method, optional transparency, optional interlace.
fn random_animation(seed: u64) -> Vec<u8> {
    let mut rng = Lcg(seed);
    let w = 2 + rng.below(14) as u16;
    let h = 2 + rng.below(14) as u16;
    let colours = 2 + rng.below(30) as usize;
    let palette: Vec<Rgb> = (0..colours)
        .map(|_| {
            Rgb::new(
                rng.below(256) as u8,
                rng.below(256) as u8,
                rng.below(256) as u8,
            )
        })
        .collect();
    let mut b = AnimationBuilder::new(w, h, palette)
        .loop_forever()
        .interlaced(rng.below(2) == 1);
    let frames = 1 + rng.below(4);
    let mut transparent: Vec<Option<u8>> = Vec::new();
    for _ in 0..frames {
        let fw = 1 + rng.below(u32::from(w)) as u16;
        let fh = 1 + rng.below(u32::from(h)) as u16;
        let left = rng.below(u32::from(w - fw) + 1) as u16;
        let top = rng.below(u32::from(h - fh) + 1) as u16;
        let indices: Vec<u8> = (0..usize::from(fw) * usize::from(fh))
            .map(|_| rng.below(colours as u32) as u8)
            .collect();
        let disposal = match rng.below(4) {
            0 => DisposalMethod::None,
            1 => DisposalMethod::Keep,
            2 => DisposalMethod::RestoreBackground,
            _ => DisposalMethod::RestorePrevious,
        };
        transparent.push(if rng.below(2) == 1 {
            Some(rng.below(colours as u32) as u8)
        } else {
            None
        });
        b = b
            .add_placed_frame(left, top, fw, fh, indices, rng.below(50) as u16, disposal)
            .expect("placed frame");
    }
    let mut file = b.build().expect("build");
    for (f, t) in file.frames_mut().zip(transparent) {
        if let Some(gce) = f.graphic_control.as_mut() {
            gce.transparent_index = t;
        }
    }
    encode_file(&file).expect("encode")
}

/// Alpha-aware equality: where the compositor left a pixel transparent
/// (alpha 0) only the alpha must match; elsewhere every channel must.
fn assert_rgba_shows_same(rgba: &[u8], canvas: &[u8]) {
    assert_eq!(rgba.len(), canvas.len());
    for (i, (a, b)) in rgba.chunks_exact(4).zip(canvas.chunks_exact(4)).enumerate() {
        assert_eq!(a[3], b[3], "pixel {i}: alpha differs ({a:?} vs {b:?})");
        if a[3] == 255 {
            assert_eq!(a, b, "pixel {i}");
        }
    }
}

#[test]
fn decode_shows_the_first_composed_frame_on_random_animations() {
    for seed in 0..64u64 {
        let bytes = random_animation(seed);
        assert!(probe(&bytes));
        let file = parse(&bytes).unwrap();
        let composed = compose(&file).unwrap();

        let img = decode(&bytes).unwrap();
        assert_eq!(img.width, u32::from(file.screen_width));
        assert_eq!(img.height, u32::from(file.screen_height));
        assert_rgba_shows_same(&img.to_rgba8(), &composed[0].canvas.pixels);
        assert_eq!(decode_rgba8(&bytes).unwrap().data, img.to_rgba8());
        assert_eq!(decode_rgb8(&bytes).unwrap().data, img.to_rgb8());

        let i = info(&bytes).unwrap();
        assert_eq!(
            i.format, img.format,
            "seed {seed}: info predicts the layout"
        );
        assert_eq!(
            i.has_alpha,
            img.has_alpha(),
            "seed {seed}: info predicts alpha"
        );
        assert_eq!(i.frames as usize, composed.len());
        assert_eq!(i.loop_count, Some(0));
        assert_eq!(i.interlaced, file.frames().next().unwrap().interlaced);

        let frames = decode_all(&bytes).unwrap();
        assert_eq!(frames.len(), composed.len());
        for (f, c) in frames.iter().zip(&composed) {
            assert_eq!(f.image.format, PixelFormat::Rgba);
            assert_eq!(f.image.as_bytes().unwrap(), c.canvas.pixels.as_slice());
            assert_eq!(
                f.delay,
                Some(Duration::from_millis(u64::from(c.delay_centis) * 10))
            );
        }

        // Lossless Pal8 round trip through the contract encoder.
        if img.format == PixelFormat::Pal8 {
            let again = decode(&encode(&img, &EncodeOptions::default()).unwrap()).unwrap();
            assert_eq!(again, img, "seed {seed}: decode(encode(img)) == img");
        }

        // Every composited frame re-encodes as an animation showing the
        // same pixels.
        let anim = encode_all(&frames, &EncodeOptions::default()).unwrap();
        assert_eq!(
            anim,
            encode_animation(&frames, &EncodeOptions::default()).unwrap(),
            "encode_animation is a byte-identical alias of encode_all"
        );
        let back = decode_all(&anim).unwrap();
        assert_eq!(back.len(), frames.len());
        for (a, b) in back.iter().zip(&frames) {
            // Every frame of `frames` is a full canvas painted over the
            // previous one; the re-encode quantises each independently
            // (lossless here: ≤ 256 colours), so pixels must match where
            // opaque and alpha must match everywhere.
            assert_rgba_shows_same(a.image.as_bytes().unwrap(), b.image.as_bytes().unwrap());
            assert_eq!(a.delay, b.delay);
            assert_eq!(a.disposal, b.disposal);
        }
    }
}

#[test]
fn raw_paths_quantise_deterministically() {
    // 300 distinct colours: must reduce to ≤ 256, identically each run.
    let w = 20u32;
    let h = 15u32;
    let rgb: Vec<u8> = (0..w * h)
        .flat_map(|i| [(i % 256) as u8, (i / 3 % 256) as u8, (255 - i % 256) as u8])
        .collect();
    let a = encode_rgb8(w, h, &rgb, &EncodeOptions::default()).unwrap();
    let b = encode_rgb8(w, h, &rgb, &EncodeOptions::default()).unwrap();
    assert_eq!(a, b, "quantiser is deterministic");
    let img = decode(&a).unwrap();
    assert_eq!(img.format, PixelFormat::Pal8);
    assert!(img.palette.as_ref().unwrap().len() <= 256);
    assert_eq!(img.to_rgb8().len(), rgb.len());

    // A 64-colour budget is honoured.
    let small = encode_rgb8(w, h, &rgb, &EncodeOptions::default().with_max_colors(64)).unwrap();
    assert!(decode(&small).unwrap().palette.unwrap().len() <= 64);

    // RGBA: alpha below 128 is transparent, at or above is opaque.
    let rgba: Vec<u8> = (0..w * h)
        .flat_map(|i| [(i % 256) as u8, 0, 0, if i % 2 == 0 { 0 } else { 200 }])
        .collect();
    let bytes = encode_rgba8(w, h, &rgba, &EncodeOptions::default()).unwrap();
    let out = decode_rgba8(&bytes).unwrap().data;
    for (i, px) in out.chunks_exact(4).enumerate() {
        assert_eq!(px[3], if i % 2 == 0 { 0 } else { 255 }, "pixel {i}");
    }
}

#[test]
fn encode_refuses_what_gif_cannot_carry() {
    let img = GifImage::from_rgb8(1, 1, vec![0, 0, 0]).unwrap();
    let mut wide = img.clone();
    wide.width = 65_536;
    assert!(matches!(
        encode(&wide, &EncodeOptions::default()),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        GifImage::from_indexed(1, 1, vec![3], Palette::new(vec![[0, 0, 0, 255]])),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        encode_rgba8(2, 2, &[0; 15], &EncodeOptions::default()),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn hostile_inputs_error_instead_of_panicking() {
    let samples: Vec<Vec<u8>> = vec![
        vec![],
        b"GIF".to_vec(),
        b"GIF89a".to_vec(),
        b"GIF89a\x10\x00\x10\x00\x80\x00\x00".to_vec(),
        b"GIF89a\x10\x00\x10\x00\x00\x00\x00\x2C".to_vec(),
        b"GIF89a\xff\xff\xff\xff\x00\x00\x00\x3B".to_vec(),
        b"GIF89a\x01\x00\x01\x00\x00\x00\x00\x21\xF9\x04\x01\x00\x00\x00\x00\x3B".to_vec(),
    ];
    for s in &samples {
        let _ = probe(s);
        let _ = info(s);
        let _ = decode(s);
        let _ = decode_all(s);
        let _ = decode_with(s, &DecodeOptions::default().with_strict(true));
        let _ = decode_with(s, &DecodeOptions::default().with_lenient(true));
    }
    // A screen bigger than the byte limit is refused before anything
    // is allocated.
    let huge = b"GIF89a\xff\xff\xff\xff\x00\x00\x00\x3B";
    assert!(matches!(
        decode_with(huge, &DecodeOptions::default().with_max_bytes(1_000u64)),
        Err(Error::LimitExceeded(_))
    ));
}

#[test]
fn frame_constructor_defaults() {
    let f = Frame::new(GifImage::from_rgba8(1, 1, vec![0; 4]).unwrap(), None);
    assert_eq!(f.disposal, DisposalMethod::None);
    assert!(!f.user_input);
    assert_eq!(f.delay, None);
}
