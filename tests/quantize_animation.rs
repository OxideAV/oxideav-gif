//! Integration test for the truecolor RGBA → GIF encode path.
//!
//! Exercises the `quantize` module + the `GifImage::from_rgba_frame` /
//! `from_rgba_frames` constructors end-to-end: arbitrary RGBA frames in,
//! a conformant GIF89a byte stream out, decoded + composed back to RGBA,
//! and the composed output checked against the source colours.

use oxideav_gif::{compose, decode, encode, DisposalMethod, GifImage, Playback, Rgb};

/// Build a `width * height` solid-colour RGBA frame.
fn solid(width: usize, height: usize, r: u8, g: u8, b: u8, a: u8) -> Vec<u8> {
    [r, g, b, a].repeat(width * height)
}

#[test]
fn single_truecolor_still_round_trips_to_a_decodable_gif() {
    // A 12x8 horizontal gradient — well under 256 colours so the exact
    // palette is preserved losslessly.
    let (w, h) = (12usize, 8usize);
    let mut rgba = Vec::new();
    for _y in 0..h {
        for x in 0..w {
            let v = (x * 255 / (w - 1)) as u8;
            rgba.extend_from_slice(&[v, 0, 255 - v, 255]);
        }
    }
    let img = GifImage::from_rgba_frame(&rgba, w as u16, h as u16, 256).unwrap();
    let bytes = encode(&img).unwrap();
    assert!(bytes.starts_with(b"GIF89a") || bytes.starts_with(b"GIF87a"));

    let decoded = decode(&bytes).unwrap();
    let composed = compose(&decoded).unwrap();
    assert_eq!(composed.len(), 1);
    let canvas = &composed[0].canvas;
    assert_eq!(canvas.width as usize, w);
    assert_eq!(canvas.height as usize, h);
    // Every pixel composites back to its exact source colour (the
    // gradient has fewer than 256 distinct colours → lossless palette).
    for (i, px) in canvas.pixels.chunks_exact(4).enumerate() {
        let x = i % w;
        let v = (x * 255 / (w - 1)) as u8;
        assert_eq!(px, [v, 0, 255 - v, 255], "pixel {i}");
    }
}

#[test]
fn high_color_count_still_quantises_within_palette_limit() {
    // 24x16 = 384 unique-ish colours → forces median cut.
    let (w, h) = (24usize, 16usize);
    let mut rgba = Vec::new();
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[(x * 10) as u8, (y * 15) as u8, ((x + y) * 6) as u8, 255]);
        }
    }
    let img = GifImage::from_rgba_frame(&rgba, w as u16, h as u16, 256).unwrap();
    let pal_len = img.global_palette.as_ref().unwrap().len();
    assert!(pal_len <= 256, "palette {pal_len} exceeds §19 limit");

    let bytes = encode(&img).unwrap();
    let decoded = decode(&bytes).unwrap();
    let composed = compose(&decoded).unwrap();
    assert_eq!(composed.len(), 1);
    // Quantisation is lossy, but the composed canvas must still be the
    // right shape with every pixel opaque.
    let canvas = &composed[0].canvas;
    assert_eq!(canvas.pixels.len(), w * h * 4);
    for px in canvas.pixels.chunks_exact(4) {
        assert_eq!(px[3], 0xFF);
    }
}

#[test]
fn multi_frame_animation_composes_each_frame_to_its_source_colour() {
    // Three full-screen solid frames cycling R → G → B, each a distinct
    // solid colour so per-frame quantisation is lossless. Disposal
    // "Keep" means each frame fully overwrites the previous one.
    let (w, h) = (4usize, 4usize);
    let red = solid(w, h, 255, 0, 0, 255);
    let green = solid(w, h, 0, 255, 0, 255);
    let blue = solid(w, h, 0, 0, 255, 255);
    let frames: Vec<(&[u8], u16, DisposalMethod)> = vec![
        (&red, 10, DisposalMethod::Keep),
        (&green, 20, DisposalMethod::Keep),
        (&blue, 30, DisposalMethod::Keep),
    ];
    let img = GifImage::from_rgba_frames(&frames, w as u16, h as u16, 256, Some(0)).unwrap();
    // Loops forever (NETSCAPE2.0).
    assert_eq!(img.loop_count(), Some(0));

    let bytes = encode(&img).unwrap();
    let decoded = decode(&bytes).unwrap();
    assert_eq!(decoded.frame_count(), 3);
    assert_eq!(decoded.loop_count(), Some(0));

    let composed = compose(&decoded).unwrap();
    assert_eq!(composed.len(), 3);

    let expected = [[255, 0, 0], [0, 255, 0], [0, 0, 255]];
    let delays = [10u16, 20, 30];
    for (f, frame) in composed.iter().enumerate() {
        assert_eq!(frame.delay_centis, delays[f], "frame {f} delay");
        for px in frame.canvas.pixels.chunks_exact(4) {
            assert_eq!([px[0], px[1], px[2]], expected[f], "frame {f} pixel colour");
            assert_eq!(px[3], 0xFF, "frame {f} opaque");
        }
    }

    // The lazy Playback iterator agrees with eager compose on per-frame
    // colour + delay.
    let play = Playback::new(&decoded);
    let lazy: Vec<_> = play.frames().collect::<Result<_, _>>().unwrap();
    assert_eq!(lazy.len(), 3);
    for (f, pf) in lazy.iter().enumerate() {
        for px in pf.canvas.pixels.chunks_exact(4) {
            assert_eq!([px[0], px[1], px[2]], expected[f]);
        }
    }
}

#[test]
fn animated_frames_with_transparency_show_prior_canvas_through() {
    // Frame 0: solid red, disposal None (left in place).
    // Frame 1: a half-transparent overlay — left column opaque blue,
    //          right column transparent — so the right column must show
    //          frame 0's red through.
    let (w, h) = (2usize, 1usize);
    let red = solid(w, h, 255, 0, 0, 255);
    // left = blue opaque, right = transparent.
    let overlay = vec![0, 0, 255, 255, 0, 0, 0, 0];
    let frames: Vec<(&[u8], u16, DisposalMethod)> = vec![
        (&red, 10, DisposalMethod::None),
        (&overlay, 10, DisposalMethod::None),
    ];
    let img = GifImage::from_rgba_frames(&frames, w as u16, h as u16, 256, None).unwrap();
    let bytes = encode(&img).unwrap();
    let decoded = decode(&bytes).unwrap();
    assert!(decoded.has_transparency());

    let composed = compose(&decoded).unwrap();
    assert_eq!(composed.len(), 2);
    // Frame 1: left pixel = blue (opaque overlay), right pixel = red
    // (frame 0 showing through the transparent overlay pixel).
    let f1 = &composed[1].canvas;
    assert_eq!(
        [f1.pixels[0], f1.pixels[1], f1.pixels[2]],
        [0, 0, 255],
        "left = overlay blue"
    );
    assert_eq!(
        [f1.pixels[4], f1.pixels[5], f1.pixels[6]],
        [255, 0, 0],
        "right = frame-0 red showing through"
    );
}

#[test]
fn quantizer_keeps_a_repeated_palette_foldable_into_a_global_table() {
    // Two frames that use the *same* two colours. Per-frame LCTs are
    // emitted, but the colours match, so optimize_color_tables can hoist
    // them into one §18 Global Color Table — proving the constructor
    // output is amenable to the existing encoder optimisation pass.
    let (w, h) = (2usize, 2usize);
    // Both frames use only black + white (checkerboard, then inverse).
    let a = vec![
        0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 255,
    ];
    let b = vec![
        255, 255, 255, 255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255, 255,
    ];
    let frames: Vec<(&[u8], u16, DisposalMethod)> = vec![
        (&a, 10, DisposalMethod::None),
        (&b, 10, DisposalMethod::None),
    ];
    let mut img = GifImage::from_rgba_frames(&frames, w as u16, h as u16, 256, None).unwrap();

    // Both frames must quantise to an identical 2-entry palette for the
    // hoist to succeed; median cut over 2 distinct colours is exact.
    let pal0 = img.frames().next().unwrap().local_palette.clone().unwrap();
    assert_eq!(pal0.len(), 2);
    assert!(pal0.contains(&Rgb::new(0, 0, 0)));
    assert!(pal0.contains(&Rgb::new(255, 255, 255)));

    let hoisted = img.optimize_color_tables();
    assert!(hoisted, "identical palettes should hoist into a GCT");
    assert!(img.global_palette.is_some());
    for f in img.frames() {
        assert!(f.local_palette.is_none(), "LCT cleared after hoist");
    }

    // Still composes correctly after the optimisation.
    let bytes = encode(&img).unwrap();
    let decoded = decode(&bytes).unwrap();
    let composed = compose(&decoded).unwrap();
    assert_eq!(composed.len(), 2);
}

#[test]
fn dithered_still_round_trips_to_a_decodable_gif() {
    // A 24x24 smooth diagonal gradient quantised to 8 colours with
    // Floyd–Steinberg dithering. The dithered index plane must still
    // encode + decode + compose to a valid in-range RGBA canvas.
    use oxideav_gif::{Dither, QuantizeOptions};
    let (w, h) = (24usize, 24usize);
    let mut rgba = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let r = (x * 10) as u8;
            let g = (y * 10) as u8;
            let b = ((x + y) * 5) as u8;
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    let opts = QuantizeOptions::with_max_colors(8).dither(Dither::FloydSteinberg);
    let img = GifImage::from_rgba_frame_with_options(&rgba, w as u16, h as u16, opts).unwrap();
    // Median cut keeps at most 8 entries; dithering only stipples between
    // them, so the palette stays within budget.
    let frame = img.frames().next().unwrap();
    let pal = img.global_palette.as_ref().unwrap();
    assert!(pal.len() <= 8);
    assert!(frame.indices.iter().all(|&i| (i as usize) < pal.len()));

    let bytes = encode(&img).unwrap();
    let decoded = decode(&bytes).unwrap();
    let composed = compose(&decoded).unwrap();
    assert_eq!(composed.len(), 1);
    let canvas = &composed[0].canvas;
    assert_eq!(canvas.width as usize, w);
    assert_eq!(canvas.height as usize, h);
    // Every composed pixel must be one of the (opaque) palette entries.
    for px in canvas.pixels.chunks_exact(4) {
        assert_eq!(px[3], 255);
        let c = Rgb::new(px[0], px[1], px[2]);
        assert!(pal.contains(&c), "composed colour {c:?} not in palette");
    }
}

#[test]
fn every_dither_variant_round_trips_to_a_decodable_gif() {
    // Each dither variant (error-diffusion family + ordered Bayer) must
    // survive the full quantise → encode → decode → compose pipeline with
    // every composed pixel landing on a palette entry. This exercises the
    // new kernels through the real container bytes, not just the in-memory
    // quantiser.
    use oxideav_gif::{Dither, QuantizeOptions};
    let (w, h) = (24usize, 24usize);
    let mut rgba = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let r = (x * 10) as u8;
            let g = (y * 10) as u8;
            let b = ((x + y) * 5) as u8;
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    for d in [
        Dither::JarvisJudiceNinke,
        Dither::Stucki,
        Dither::Burkes,
        Dither::Sierra,
        Dither::Atkinson,
        Dither::OrderedBayer8x8,
    ] {
        let opts = QuantizeOptions::with_max_colors(8).dither(d);
        let img = GifImage::from_rgba_frame_with_options(&rgba, w as u16, h as u16, opts).unwrap();
        let pal = img.global_palette.as_ref().unwrap().clone();
        assert!(pal.len() <= 8, "{d:?} palette exceeds budget");
        let bytes = encode(&img).unwrap();
        let decoded = decode(&bytes).unwrap();
        let composed = compose(&decoded).unwrap();
        assert_eq!(composed.len(), 1, "{d:?}");
        let canvas = &composed[0].canvas;
        assert_eq!(canvas.width as usize, w);
        assert_eq!(canvas.height as usize, h);
        for px in canvas.pixels.chunks_exact(4) {
            assert_eq!(px[3], 255, "{d:?} opaque");
            let c = Rgb::new(px[0], px[1], px[2]);
            assert!(
                pal.contains(&c),
                "{d:?}: composed colour {c:?} not in palette"
            );
        }
    }
}

#[test]
fn serpentine_dither_round_trips_through_the_encode_constructor() {
    // The QuantizeOptions::serpentine flag must thread through the public
    // from_rgba_frame_with_options encode path unchanged: the serpentine
    // and raster runs both decode to a valid in-palette canvas, and the
    // two encoded streams differ (the flag actually reached the diffuser).
    use oxideav_gif::{Dither, QuantizeOptions};
    let (w, h) = (24usize, 24usize);
    let mut rgba = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let v = ((x + y) * 5) as u8;
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
    }
    let raster = QuantizeOptions::with_max_colors(4).dither(Dither::FloydSteinberg);
    let serp = raster.serpentine(true);

    let img_r = GifImage::from_rgba_frame_with_options(&rgba, w as u16, h as u16, raster).unwrap();
    let img_s = GifImage::from_rgba_frame_with_options(&rgba, w as u16, h as u16, serp).unwrap();
    let bytes_r = encode(&img_r).unwrap();
    let bytes_s = encode(&img_s).unwrap();
    assert_ne!(
        bytes_r, bytes_s,
        "serpentine flag must change the encoded LZW stream"
    );

    // Both decode + compose to an in-palette canvas.
    for (bytes, img) in [(&bytes_s, &img_s), (&bytes_r, &img_r)] {
        let pal = img.global_palette.as_ref().unwrap();
        let composed = compose(&decode(bytes).unwrap()).unwrap();
        assert_eq!(composed.len(), 1);
        for px in composed[0].canvas.pixels.chunks_exact(4) {
            assert!(pal.contains(&Rgb::new(px[0], px[1], px[2])));
        }
    }
}

#[test]
fn shared_palette_animation_dithers_with_each_kernel() {
    // The shared-palette animation path (one GCT, no LCTs) must accept
    // every dither variant and produce a decodable, composable animation.
    use oxideav_gif::{Dither, QuantizeOptions};
    let (w, h) = (16u16, 16u16);
    let mut f0 = Vec::new();
    let mut f1 = Vec::new();
    for y in 0..h {
        for x in 0..w {
            f0.extend_from_slice(&[(x * 14) as u8, (y * 14) as u8, 90, 255]);
            f1.extend_from_slice(&[90, (x * 14) as u8, (y * 14) as u8, 255]);
        }
    }
    let frames = [
        (&f0[..], 8u16, DisposalMethod::None),
        (&f1[..], 8u16, DisposalMethod::None),
    ];
    for d in [
        Dither::FloydSteinberg,
        Dither::Sierra,
        Dither::Atkinson,
        Dither::OrderedBayer8x8,
    ] {
        let opts = QuantizeOptions::with_max_colors(32).dither(d);
        let img = GifImage::from_rgba_frames_shared_palette(&frames, w, h, opts, Some(0)).unwrap();
        // Shared palette → one GCT, no per-frame LCTs.
        assert!(img.global_palette.is_some(), "{d:?} no GCT");
        for f in img.frames() {
            assert!(f.local_palette.is_none(), "{d:?} unexpected LCT");
        }
        let bytes = encode(&img).unwrap();
        let decoded = decode(&bytes).unwrap();
        let composed = compose(&decoded).unwrap();
        assert_eq!(composed.len(), 2, "{d:?}");
    }
}

#[test]
fn dithered_animation_round_trips_and_composes() {
    use oxideav_gif::{Dither, QuantizeOptions};
    let (w, h) = (8u16, 8u16);
    let f0 = solid(w as usize, h as usize, 200, 30, 30, 255);
    let f1 = solid(w as usize, h as usize, 30, 200, 30, 255);
    let frames = [
        (&f0[..], 10u16, DisposalMethod::None),
        (&f1[..], 10u16, DisposalMethod::None),
    ];
    let opts = QuantizeOptions::with_max_colors(16).dither(Dither::FloydSteinberg);
    let img = GifImage::from_rgba_frames_with_options(&frames, w, h, opts, Some(0)).unwrap();
    let bytes = encode(&img).unwrap();
    let decoded = decode(&bytes).unwrap();
    let eager = compose(&decoded).unwrap();
    assert_eq!(eager.len(), 2);
    // Lazy Playback parity with eager compose.
    let play = Playback::new(&decoded);
    let lazy: Vec<_> = play.frames().collect::<Result<_, _>>().unwrap();
    assert_eq!(lazy.len(), 2);
    for (l, e) in lazy.iter().zip(&eager) {
        assert_eq!(l.canvas.pixels, e.canvas.pixels);
    }
}

#[test]
fn shared_palette_animation_uses_one_gct_and_no_lcts() {
    use oxideav_gif::QuantizeOptions;
    // Three solid frames cycling R -> G -> B. A shared palette holds all
    // three exactly (well under budget), installed as one Global Color
    // Table with no per-frame Local Color Tables.
    let (w, h) = (4u16, 4u16);
    let red = solid(w as usize, h as usize, 255, 0, 0, 255);
    let green = solid(w as usize, h as usize, 0, 255, 0, 255);
    let blue = solid(w as usize, h as usize, 0, 0, 255, 255);
    let frames = [
        (&red[..], 10u16, DisposalMethod::Keep),
        (&green[..], 10u16, DisposalMethod::Keep),
        (&blue[..], 10u16, DisposalMethod::Keep),
    ];
    let img = GifImage::from_rgba_frames_shared_palette(
        &frames,
        w,
        h,
        QuantizeOptions::with_max_colors(256),
        Some(0),
    )
    .unwrap();

    // One GCT, no LCTs.
    let gct = img
        .global_palette
        .as_ref()
        .expect("shared palette in a GCT");
    assert!(gct.contains(&Rgb::new(255, 0, 0)));
    assert!(gct.contains(&Rgb::new(0, 255, 0)));
    assert!(gct.contains(&Rgb::new(0, 0, 255)));
    for f in img.frames() {
        assert!(f.local_palette.is_none(), "no LCT under a shared palette");
    }

    // Round-trips, and each frame composes back to its source colour.
    let bytes = encode(&img).unwrap();
    let decoded = decode(&bytes).unwrap();
    assert_eq!(decoded.frame_count(), 3);
    let composed = compose(&decoded).unwrap();
    assert_eq!(composed.len(), 3);
    let expected = [[255, 0, 0], [0, 255, 0], [0, 0, 255]];
    for (f, frame) in composed.iter().enumerate() {
        for px in frame.canvas.pixels.chunks_exact(4) {
            assert_eq!([px[0], px[1], px[2]], expected[f], "frame {f}");
            assert_eq!(px[3], 0xFF);
        }
    }
}

#[test]
fn shared_palette_is_smaller_than_per_frame_lcts() {
    use oxideav_gif::QuantizeOptions;
    // Two frames sharing the same broad colour set. The shared-palette
    // encode must be no larger than the per-frame-LCT encode (one table
    // vs two), demonstrating the size win.
    let (w, h) = (16u16, 16u16);
    let mut a = Vec::new();
    let mut b = Vec::new();
    for y in 0..h {
        for x in 0..w {
            a.extend_from_slice(&[(x * 16) as u8, (y * 16) as u8, 0, 255]);
            b.extend_from_slice(&[0, (x * 16) as u8, (y * 16) as u8, 255]);
        }
    }
    let frames = [
        (&a[..], 10u16, DisposalMethod::Keep),
        (&b[..], 10u16, DisposalMethod::Keep),
    ];
    let opts = QuantizeOptions::with_max_colors(64);
    let per_frame = GifImage::from_rgba_frames(&frames, w, h, 64, Some(0)).unwrap();
    let shared = GifImage::from_rgba_frames_shared_palette(&frames, w, h, opts, Some(0)).unwrap();
    let per_frame_bytes = encode(&per_frame).unwrap();
    let shared_bytes = encode(&shared).unwrap();
    assert!(
        shared_bytes.len() <= per_frame_bytes.len(),
        "shared {} should be <= per-frame {}",
        shared_bytes.len(),
        per_frame_bytes.len()
    );
    // And the shared one still decodes to two frames.
    let decoded = decode(&shared_bytes).unwrap();
    assert_eq!(decoded.frame_count(), 2);
    assert!(decoded.global_palette.is_some());
}
