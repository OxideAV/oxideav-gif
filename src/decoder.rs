//! GIF frame decoder.
//!
//! The standalone API ([`decode_gif`]) takes a full GIF file buffer
//! and returns a [`GifImage`] with one composited [`GifFrame`] per
//! animation step. Compositing follows the GIF disposal model:
//!
//! * Disposal 0/1 — keep the rendered pixels.
//! * Disposal 2  — restore the frame area to the background (transparent
//!   if a transparent index is set, otherwise index 0).
//! * Disposal 3  — restore to the previous canvas state.
//!
//! Transparent pixels skip the composite (classic "don't touch what's
//! already there"). Interlaced frames are unwoven from GIF's 4-pass
//! order into progressive row storage before compositing.
//!
//! When the `registry` feature is enabled, a thin
//! [`Decoder`](oxideav_core::Decoder) trait wrapper consumes packets
//! produced by the matching `Demuxer` and emits `Pal8` `VideoFrame`s
//! sized to the logical canvas.

use crate::container::parse_gif;
use crate::error::{GifError as Error, Result};
use crate::image::{GifFrame, GifImage};
use crate::lzw::Lzw;

// Backward-compat re-exports: existing callers reach for
// `oxideav_gif::decoder::make_decoder`. Keep that path live by
// re-exporting the registry-side factory.
#[cfg(feature = "registry")]
pub use crate::registry::make_decoder;

/// Decode an entire GIF file into its composited per-frame canvases.
/// Standalone (no `oxideav-core`) entry point: walks the parsed
/// container directly without going through the framework's
/// `Demuxer` / `Decoder` traits.
pub fn decode_gif(buf: &[u8]) -> Result<GifImage> {
    let parsed = parse_gif(buf)?;
    let mut canvas = vec![0u8; (parsed.canvas_w * parsed.canvas_h) as usize];
    let mut prev_canvas: Option<Vec<u8>> = None;
    let mut frames: Vec<GifFrame> = Vec::with_capacity(parsed.frames.len());

    for f in &parsed.frames {
        // Decode LZW into the frame's sub-rect indices.
        let lzw = Lzw::decoder(f.min_code_size)?;
        let decoded = lzw.read(&f.lzw_data)?;
        let frame_area = (f.w as usize) * (f.h as usize);
        if decoded.len() < frame_area {
            return Err(Error::invalid(format!(
                "GIF: LZW output {} < expected {}",
                decoded.len(),
                frame_area
            )));
        }
        let indices = if f.interlaced {
            deinterlace(&decoded, f.w as usize, f.h as usize)
        } else {
            decoded[..frame_area].to_vec()
        };

        // Save snapshot if this frame's disposal is "restore to previous".
        if f.disposal == 3 {
            prev_canvas = Some(canvas.clone());
        }

        // Composite indices into the canvas, skipping transparent pixels.
        let canvas_w = parsed.canvas_w as usize;
        let canvas_h = parsed.canvas_h as usize;
        let fw = f.w as usize;
        let fh = f.h as usize;
        let fx = f.x as usize;
        let fy = f.y as usize;
        let transp = f.transparent_index;
        for row in 0..fh {
            let dst_y = fy + row;
            if dst_y >= canvas_h {
                break;
            }
            for col in 0..fw {
                let dst_x = fx + col;
                if dst_x >= canvas_w {
                    break;
                }
                let px = indices[row * fw + col];
                if let Some(t) = transp {
                    if px == t {
                        continue;
                    }
                }
                canvas[dst_y * canvas_w + dst_x] = px;
            }
        }

        // Pick the active palette: local override wins over global.
        let palette = if !f.local_palette.is_empty() {
            f.local_palette.clone()
        } else {
            parsed.global_palette.clone()
        };

        frames.push(GifFrame {
            width: parsed.canvas_w,
            height: parsed.canvas_h,
            indices: canvas.clone(),
            palette,
            delay_cs: f.delay_cs,
        });

        // Apply this frame's disposal to prepare the canvas for the
        // *next* frame.
        match f.disposal {
            2 => {
                // Restore frame area to background (transparent = 0).
                let clear_idx = transp.unwrap_or(0);
                for row in 0..fh {
                    let dst_y = fy + row;
                    if dst_y >= canvas_h {
                        break;
                    }
                    for col in 0..fw {
                        let dst_x = fx + col;
                        if dst_x >= canvas_w {
                            break;
                        }
                        canvas[dst_y * canvas_w + dst_x] = clear_idx;
                    }
                }
            }
            3 => {
                if let Some(prev) = prev_canvas.take() {
                    canvas = prev;
                }
            }
            _ => {}
        }
    }

    Ok(GifImage {
        width: parsed.canvas_w,
        height: parsed.canvas_h,
        global_palette: parsed.global_palette,
        frames,
        loop_count: parsed.loop_count,
    })
}

/// Expand the GIF interlace pass order into progressive row storage.
///
/// GIF's interlace transmits rows in four passes:
///     pass 1: every 8th row starting at 0
///     pass 2: every 8th row starting at 4
///     pass 3: every 4th row starting at 2
///     pass 4: every 2nd row starting at 1
pub(crate) fn deinterlace(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    let mut src_row = 0usize;
    for &(start, step) in &[(0usize, 8usize), (4, 8), (2, 4), (1, 2)] {
        let mut dst_row = start;
        while dst_row < h {
            let src_off = src_row * w;
            if src_off + w > src.len() {
                // Source exhausted — leave rest as zeros.
                return out;
            }
            out[dst_row * w..dst_row * w + w].copy_from_slice(&src[src_off..src_off + w]);
            src_row += 1;
            dst_row += step;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deinterlace_roundtrip() {
        // Build a pattern where every row is filled with its row index.
        let w = 4usize;
        let h = 8usize;
        let mut progressive = Vec::with_capacity(w * h);
        for y in 0..h {
            for _ in 0..w {
                progressive.push(y as u8);
            }
        }
        // Interlace it.
        let mut interlaced = vec![0u8; w * h];
        let mut src_row = 0usize;
        for &(start, step) in &[(0usize, 8usize), (4, 8), (2, 4), (1, 2)] {
            let mut r = start;
            while r < h {
                interlaced[src_row * w..src_row * w + w]
                    .copy_from_slice(&progressive[r * w..r * w + w]);
                src_row += 1;
                r += step;
            }
        }
        let restored = deinterlace(&interlaced, w, h);
        assert_eq!(restored, progressive);
    }
}
