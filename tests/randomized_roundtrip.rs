//! Pseudorandom round-trip closure tests.
//!
//! For a wide range of image shapes and palette sizes, encode the
//! image and verify that decoding the result reproduces the original
//! pixel raster, palette and header.

use oxideav_gif::{decode, encode, Block, Frame, GifImage, Rgb, Version};

/// Reproducible xorshift32 — keeps the suite deterministic without a
/// crate-level RNG dependency.
struct Xs(u32);
impl Xs {
    fn new(seed: u32) -> Self {
        Self(seed | 1)
    }
    fn next_u8(&mut self) -> u8 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0 as u8
    }
}

fn make_palette(rng: &mut Xs, n: usize) -> Vec<Rgb> {
    (0..n)
        .map(|_| Rgb::new(rng.next_u8(), rng.next_u8(), rng.next_u8()))
        .collect()
}

fn make_gif(seed: u32, width: u16, height: u16, palette_size: usize, interlaced: bool) -> GifImage {
    let mut rng = Xs::new(seed);
    let palette = make_palette(&mut rng, palette_size);
    let mut indices = Vec::with_capacity((width as usize) * (height as usize));
    for _ in 0..(width as usize) * (height as usize) {
        indices.push((rng.next_u8() as usize % palette_size) as u8);
    }
    GifImage {
        version: Version::Gif89a,
        screen_width: width,
        screen_height: height,
        color_resolution: 7,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(palette),
        blocks: vec![Block::Image(Frame {
            left: 0,
            top: 0,
            width,
            height,
            local_palette: None,
            palette_sorted: false,
            interlaced,
            indices,
            graphic_control: None,
        })],
    }
}

#[test]
fn many_shapes_and_palettes_roundtrip() {
    // Palette sizes must be a power of two — the on-disk format
    // cannot record any other count (§19 / §21 store the size as
    // 2^(field+1) for field ∈ 0..=7).
    for (i, (w, h, pal)) in [
        (1u16, 1u16, 2usize),
        (1, 1, 256),
        (3, 3, 4),
        (16, 16, 16),
        (32, 32, 32),
        (37, 41, 64),
        (64, 64, 256),
        (100, 100, 8),
        (200, 100, 128),
        (1, 256, 4),
        (256, 1, 4),
    ]
    .iter()
    .enumerate()
    {
        let img = make_gif(0xCAFE_F00D + i as u32, *w, *h, *pal, false);
        let bytes = encode(&img).unwrap_or_else(|e| {
            panic!("encode {w}x{h} pal={pal} failed: {e}");
        });
        let img2 = decode(&bytes).unwrap_or_else(|e| {
            panic!("decode {w}x{h} pal={pal} failed: {e}");
        });
        assert_eq!(img2, img, "round-trip differs for {w}x{h} palette={pal}");
    }
}

#[test]
fn interlaced_variants_roundtrip() {
    for (i, (w, h)) in [(1u16, 1u16), (8, 1), (1, 8), (16, 16), (37, 53), (200, 80)]
        .iter()
        .enumerate()
    {
        let img = make_gif(0xFEED_FACE + i as u32, *w, *h, 16, true);
        let bytes = encode(&img).unwrap();
        let img2 = decode(&bytes).unwrap();
        assert_eq!(img2, img, "interlaced round-trip differs for {w}x{h}");
    }
}

/// Encode → decode → re-encode yields byte-identical output. Confirms
/// the encoder is deterministic over its inputs.
#[test]
fn re_encode_is_byte_identical() {
    let img = make_gif(0x1234_5678, 64, 48, 32, false);
    let bytes1 = encode(&img).unwrap();
    let img2 = decode(&bytes1).unwrap();
    let bytes2 = encode(&img2).unwrap();
    assert_eq!(bytes1, bytes2);
}
