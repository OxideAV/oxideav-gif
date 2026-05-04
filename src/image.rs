//! Standalone image type for `oxideav-gif`'s framework-free decode /
//! encode API.
//!
//! GIF is always paletted (`Pal8`), so the standalone surface is
//! simpler than PNG: the [`GifImage`] / [`GifFrame`] pair carries the
//! 8-bit index plane plus the matching RGBA palette. When the
//! `registry` feature is enabled, the gated [`crate::registry`]
//! module provides conversions to / from `oxideav_core::VideoFrame`.

/// One decoded GIF frame: a single canvas-sized `Pal8` raster plus the
/// palette that interprets it and the per-frame display delay.
#[derive(Clone, Debug)]
pub struct GifFrame {
    /// Canvas width in pixels.
    pub width: u32,
    /// Canvas height in pixels.
    pub height: u32,
    /// Tightly packed `width * height` bytes of palette indices.
    pub indices: Vec<u8>,
    /// Active palette for this frame: up to 256 RGBA quads. May be the
    /// global palette inherited from the file header or a frame-local
    /// override (composited by the decoder).
    pub palette: Vec<[u8; 4]>,
    /// Display duration in centiseconds (1/100 s — GIF's native unit).
    pub delay_cs: u16,
}

impl GifFrame {
    /// Convert the frame's `Pal8` indices to a contiguous `RGBA` byte
    /// buffer (`width * height * 4` bytes). Pixels with palette index
    /// past the palette's length come out as fully-transparent black.
    pub fn to_rgba(&self) -> Vec<u8> {
        let n = (self.width as usize) * (self.height as usize);
        let mut out = vec![0u8; n * 4];
        for (i, &idx) in self.indices.iter().take(n).enumerate() {
            let entry = self
                .palette
                .get(idx as usize)
                .copied()
                .unwrap_or([0, 0, 0, 0]);
            out[i * 4..i * 4 + 4].copy_from_slice(&entry);
        }
        out
    }
}

/// Decoded GIF file: canvas dimensions, the global palette, and one
/// composited [`GifFrame`] per animation step. Static GIFs come out
/// as a `frames` list of length 1.
#[derive(Clone, Debug)]
pub struct GifImage {
    /// Logical screen width in pixels.
    pub width: u32,
    /// Logical screen height in pixels.
    pub height: u32,
    /// Global colour table (may be empty if the file only carries
    /// per-frame local palettes).
    pub global_palette: Vec<[u8; 4]>,
    /// Composited frames in playback order. Each frame is sized to the
    /// canvas (the decoder applies disposal + transparency).
    pub frames: Vec<GifFrame>,
    /// NETSCAPE2.0 loop count (`0` = infinite). `None` if the file
    /// carries no NETSCAPE extension.
    pub loop_count: Option<u16>,
}
