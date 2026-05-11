//! Multi-frame compositing per GIF89a §18 + §23.
//!
//! The decoder in [`crate::decoder`] returns each frame's raw palette
//! indices, intentionally leaving the question of "what does the
//! display device actually show after frame N?" to a higher layer.
//! This module is that layer.
//!
//! # Spec references
//!
//! * §18 "Logical Screen Descriptor" — defines the virtual canvas
//!   (`screen_width × screen_height`) on which every frame is drawn,
//!   plus the Background Color Index that the Restore-Background
//!   disposal mode reverts to.
//! * §20.c "Image Descriptor" — every frame carries a `(left, top,
//!   width, height)` placement rectangle inside the logical screen.
//! * §23 "Graphic Control Extension" — the Disposal Method byte
//!   (3-bit subfield, values 0–7) governs what happens to the
//!   rectangle a frame just drew, *before the next frame renders*.
//! * §23.c.viii "Transparency Index" — when set, the matching pixel
//!   indices are not modified during rendering.
//! * §23.f.i — "Restore To Previous" recommends falling back to
//!   "Restore to background color" when a decoder cannot save the
//!   prior state. We always implement the proper save/restore.
//!
//! # Disposal-method semantics (§23.c.iv values 0–3)
//!
//! ```text
//! 0  No disposal specified.    Decoder is not required to take any action.
//! 1  Do not dispose.           The graphic is left in place.
//! 2  Restore to background.    The rectangle drawn by this frame is
//!                              cleared to the Background Color Index.
//! 3  Restore to previous.      The rectangle drawn by this frame is
//!                              reverted to whatever was visible there
//!                              immediately before this frame rendered.
//! ```
//!
//! Reserved values 4–7 are treated identically to 0 (the decoder maps
//! them to [`DisposalMethod::None`] in [`crate::DisposalMethod::from_bits`]).
//!
//! # Background-color handling
//!
//! §18.c.iii: "if the [Global Color Table] flag is set, the value of
//! the Background Color Index field should be used as the table index
//! of the background color." When no Global Color Table is present,
//! the background-color index is meaningless; we substitute fully
//! transparent black (RGBA = `0x00000000`) for the cleared region.
//!
//! # Output
//!
//! Each composed frame is an `RgbaCanvas` with one byte per channel,
//! row-major top-to-bottom, premultiplied-alpha *not* applied (callers
//! get straight RGBA so they can choose their own blend convention).

use crate::error::{Error, Result};
use crate::font;
use crate::image::{Block, DisposalMethod, Frame, GifImage, PlainText, Rgb};

/// One composited canvas — `screen_width × screen_height` RGBA pixels.
///
/// Layout: `pixels.len() == 4 * width * height`, row-major, top-to-bottom,
/// `[R, G, B, A, R, G, B, A, …]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaCanvas {
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<u8>,
}

impl RgbaCanvas {
    fn new(width: u16, height: u16) -> Self {
        let n = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .expect("canvas dimensions overflow usize");
        Self {
            width,
            height,
            pixels: vec![0u8; n],
        }
    }
}

/// One composited frame in playback order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposedFrame {
    /// Snapshot of the logical screen *after* this frame has been
    /// rendered onto the canvas (and before its disposal method is
    /// applied to prepare the next frame).
    pub canvas: RgbaCanvas,
    /// Hundredths-of-a-second delay before continuing to the next
    /// frame (mirrors [`crate::GraphicControl::delay_centis`]; 0 when
    /// the frame had no GCE).
    pub delay_centis: u16,
}

/// Compose every graphic-rendering block in `image` into a sequence
/// of fully rendered RGBA canvases per the §23 disposal-method state
/// machine.
///
/// Plain Text Extensions (§25) are rendered against the crate-local
/// 8×8 monospace bitmap font ([`crate::font`]) — §25.e leaves the
/// font choice to the decoder, and this routine picks the minimal
/// stylised font supplied here. Plain Text blocks require an active
/// Global Color Table (§25.a "This block requires a Global Color
/// Table to be available"); when none is present the block is
/// skipped (visible behaviour matches the older no-render path).
///
/// # Errors
///
/// * Frame placement rectangle escapes the logical screen bounds.
/// * A frame uses a palette index that is out of range for its
///   active colour table.
/// * A frame has neither a local palette nor a global one to fall
///   back on.
pub fn compose(image: &GifImage) -> Result<Vec<ComposedFrame>> {
    let mut canvas = RgbaCanvas::new(image.screen_width, image.screen_height);

    // §18.c.iii — background colour is meaningful only if a Global
    // Color Table is present. Otherwise dispose-to-background clears
    // to fully-transparent black.
    let background_rgba: [u8; 4] = match &image.global_palette {
        Some(palette) => {
            let idx = image.background_index as usize;
            if idx >= palette.len() {
                // Spec is silent on out-of-range; clamp to transparent
                // black, matching the no-palette case, so the composer
                // never returns an error from a stream the decoder
                // accepted.
                [0, 0, 0, 0]
            } else {
                let Rgb { r, g, b } = palette[idx];
                [r, g, b, 0xFF]
            }
        }
        None => [0, 0, 0, 0],
    };

    let mut out = Vec::new();

    for block in &image.blocks {
        // Identify graphic-rendering blocks (§20 image + §25 plain
        // text) and pull their bounding rectangle + GCE; everything
        // else (§24 comments, §26 application extensions) doesn't
        // render and is skipped without touching the canvas.
        let (rect, gce, kind): (Rect, Option<&crate::image::GraphicControl>, BlockKind) =
            match block {
                Block::Image(f) => (
                    Rect {
                        left: f.left,
                        top: f.top,
                        width: f.width,
                        height: f.height,
                    },
                    f.graphic_control.as_ref(),
                    BlockKind::Image(f),
                ),
                Block::PlainText {
                    params,
                    graphic_control,
                } => (
                    Rect {
                        left: params.left,
                        top: params.top,
                        width: params.width,
                        height: params.height,
                    },
                    graphic_control.as_ref(),
                    BlockKind::PlainText(params),
                ),
                _ => continue,
            };

        check_rect_in_screen(image, &rect)?;

        // §23.f.i — snapshot the *pre-render* canvas so a later
        // RestorePrevious can revert to exactly the image the user was
        // looking at before this frame appeared.
        let pre_render_snapshot = canvas.pixels.clone();

        match kind {
            BlockKind::Image(f) => {
                render_frame(&mut canvas, f, image.global_palette.as_deref())?;
            }
            BlockKind::PlainText(p) => {
                // §25.a — "This block requires a Global Color Table to
                // be available". Without one the block has no defined
                // fg/bg colour mapping; treat it as a no-op.
                if let Some(gct) = image.global_palette.as_deref() {
                    render_plain_text(&mut canvas, p, gct);
                }
            }
        }

        let delay_centis = gce.map(|g| g.delay_centis).unwrap_or(0);

        out.push(ComposedFrame {
            canvas: canvas.clone(),
            delay_centis,
        });

        // Apply this block's disposal method to *prepare* the canvas
        // for the next frame. Plain Text is a §25 graphic-rendering
        // block so it participates in §23 disposal just like §20
        // images do (§23.d "scope is the graphic rendering block that
        // follows it").
        let disposal = gce.map(|g| g.disposal).unwrap_or(DisposalMethod::None);

        match disposal {
            // §23.c.iv values 0 + 1 — "no disposal" / "do not dispose"
            // both leave the canvas alone. Spec splits hairs on intent,
            // but the visible behaviour is identical.
            DisposalMethod::None | DisposalMethod::Keep => {}
            DisposalMethod::RestoreBackground => {
                // §23.c.iv value 2 — "the area used by the graphic
                // must be restored to the background color." Only the
                // rectangle the block just drew is cleared.
                clear_rect_to_color(&mut canvas, &rect, background_rgba);
            }
            DisposalMethod::RestorePrevious => {
                // §23.c.iv value 3 — restore what was visible before
                // this block rendered.
                canvas.pixels = pre_render_snapshot;
            }
        }
    }

    Ok(out)
}

/// Axis-aligned rectangle on the logical screen.
struct Rect {
    left: u16,
    top: u16,
    width: u16,
    height: u16,
}

enum BlockKind<'a> {
    Image(&'a Frame),
    PlainText(&'a PlainText),
}

// ---------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------

fn check_rect_in_screen(image: &GifImage, rect: &Rect) -> Result<()> {
    let right = (rect.left as u32) + (rect.width as u32);
    let bottom = (rect.top as u32) + (rect.height as u32);
    if right > image.screen_width as u32 || bottom > image.screen_height as u32 {
        return Err(Error::InvalidData(format!(
            "block placement ({},{},{}×{}) escapes logical screen ({}×{})",
            rect.left, rect.top, rect.width, rect.height, image.screen_width, image.screen_height
        )));
    }
    Ok(())
}

fn render_frame(
    canvas: &mut RgbaCanvas,
    frame: &Frame,
    global_palette: Option<&[Rgb]>,
) -> Result<()> {
    let palette: &[Rgb] = frame
        .local_palette
        .as_deref()
        .or(global_palette)
        .ok_or_else(|| {
            Error::InvalidData("frame has no local palette and stream has no global palette".into())
        })?;

    let transparent_index = frame
        .graphic_control
        .as_ref()
        .and_then(|gce| gce.transparent_index);

    let canvas_w = canvas.width as usize;
    let frame_w = frame.width as usize;
    let frame_h = frame.height as usize;
    let frame_left = frame.left as usize;
    let frame_top = frame.top as usize;

    for fy in 0..frame_h {
        let src_row = fy * frame_w;
        let dst_row_start = ((frame_top + fy) * canvas_w + frame_left) * 4;
        for fx in 0..frame_w {
            let idx = frame.indices[src_row + fx];
            // §23.c.viii — transparent pixels are *not modified* on
            // the display device; skip the write entirely.
            if Some(idx) == transparent_index {
                continue;
            }
            let entry_idx = idx as usize;
            if entry_idx >= palette.len() {
                return Err(Error::InvalidData(format!(
                    "frame pixel index {idx} out of range for palette of {} entries",
                    palette.len()
                )));
            }
            let Rgb { r, g, b } = palette[entry_idx];
            let dst = dst_row_start + fx * 4;
            canvas.pixels[dst] = r;
            canvas.pixels[dst + 1] = g;
            canvas.pixels[dst + 2] = b;
            canvas.pixels[dst + 3] = 0xFF;
        }
    }
    Ok(())
}

fn clear_rect_to_color(canvas: &mut RgbaCanvas, rect: &Rect, rgba: [u8; 4]) {
    let canvas_w = canvas.width as usize;
    let rw = rect.width as usize;
    let rh = rect.height as usize;
    let rl = rect.left as usize;
    let rt = rect.top as usize;
    for fy in 0..rh {
        let dst_row_start = ((rt + fy) * canvas_w + rl) * 4;
        for fx in 0..rw {
            let dst = dst_row_start + fx * 4;
            canvas.pixels[dst..dst + 4].copy_from_slice(&rgba);
        }
    }
}

/// Render a §25 Plain Text block against `canvas` using the active
/// Global Color Table. The text grid is `params.width × params.height`
/// pixels at `(params.left, params.top)` and contains
/// `floor(width/cell_w) × floor(height/cell_h)` character cells per
/// §25.a ("fractional cells must be discarded").
///
/// Each cell paints a `params.cell_width × params.cell_height`
/// background-coloured rectangle, then stamps the cell's glyph from
/// [`crate::font`] in foreground colour. Glyphs are anchored to the
/// cell's top-left; pixels beyond the glyph's 8×8 footprint stay
/// background-coloured (no scaling — over-sized cells are read as
/// "more padding around the same 8×8 glyph"). Characters outside
/// `0x20..=0x7E` (or the spec's wider 0x20..=0xF7 with no glyph
/// available) render as space per §25.e.
fn render_plain_text(canvas: &mut RgbaCanvas, params: &PlainText, gct: &[Rgb]) {
    // §25.a — fractional cells are discarded. Integer division here
    // is exactly that.
    if params.cell_width == 0 || params.cell_height == 0 {
        // Degenerate cell size; nothing to render.
        return;
    }
    let cols = params.width / (params.cell_width as u16);
    let rows = params.height / (params.cell_height as u16);
    let cell_w = params.cell_width as u16;
    let cell_h = params.cell_height as u16;

    // §25.c.x / xi reference the *Global Color Table*. Out-of-range
    // indices are not specified by the spec; clamp to fully
    // transparent black to avoid panicking and to make the block
    // visually disappear, which matches the conservative behaviour the
    // earlier "no Plain Text rendering" path effectively provided.
    let fg_rgba = palette_index_to_rgba(gct, params.fg_color_index);
    let bg_rgba = palette_index_to_rgba(gct, params.bg_color_index);

    let canvas_w = canvas.width as usize;
    let mut text_iter = params.text.iter().copied();
    for cell_row in 0..rows {
        for cell_col in 0..cols {
            let ch = text_iter.next().unwrap_or(b' ');
            // §25.e — bytes outside the legal Plain Text range render
            // as space. Anything our minimal font doesn't have a glyph
            // for falls back to space via `font::glyph`.
            let g = font::glyph(ch);
            let cell_left = params.left + cell_col * cell_w;
            let cell_top = params.top + cell_row * cell_h;
            for dy in 0..cell_h {
                let py = (cell_top + dy) as usize;
                for dx in 0..cell_w {
                    let px = (cell_left + dx) as usize;
                    let dst = (py * canvas_w + px) * 4;
                    // Bit-blit: glyph pixels where the bitmap is set
                    // use the foreground colour; everything else
                    // (cell padding around the 8×8 glyph + glyph
                    // background pixels) uses the cell background
                    // colour. §25 makes no distinction between
                    // "background" and "no pixel" — every cell pixel
                    // must resolve to either fg or bg.
                    let painted = if dx < font::GLYPH_WIDTH as u16 && dy < font::GLYPH_HEIGHT as u16
                    {
                        font::pixel(&g, dx as u8, dy as u8)
                    } else {
                        false
                    };
                    let rgba = if painted { fg_rgba } else { bg_rgba };
                    canvas.pixels[dst..dst + 4].copy_from_slice(&rgba);
                }
            }
        }
    }
}

fn palette_index_to_rgba(palette: &[Rgb], idx: u8) -> [u8; 4] {
    let i = idx as usize;
    if i >= palette.len() {
        [0, 0, 0, 0]
    } else {
        let Rgb { r, g, b } = palette[i];
        [r, g, b, 0xFF]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{Block, Frame, GifImage, GraphicControl, Rgb, Version};

    /// Pull the RGBA quadruple at canvas pixel `(x, y)`.
    fn px(c: &RgbaCanvas, x: u16, y: u16) -> [u8; 4] {
        let off = ((y as usize) * (c.width as usize) + (x as usize)) * 4;
        [
            c.pixels[off],
            c.pixels[off + 1],
            c.pixels[off + 2],
            c.pixels[off + 3],
        ]
    }

    fn palette_4() -> Vec<Rgb> {
        vec![
            Rgb::new(0, 0, 0),    // 0 - black (also background)
            Rgb::new(0xFF, 0, 0), // 1 - red
            Rgb::new(0, 0xFF, 0), // 2 - green
            Rgb::new(0, 0, 0xFF), // 3 - blue
        ]
    }

    /// Build a 2×2 single-colour frame placed inside a larger canvas.
    fn small_frame(left: u16, top: u16, fill: u8, gce: Option<GraphicControl>) -> Frame {
        Frame {
            left,
            top,
            width: 2,
            height: 2,
            local_palette: None,
            palette_sorted: false,
            interlaced: false,
            indices: vec![fill; 4],
            graphic_control: gce,
        }
    }

    fn base_image(blocks: Vec<Block>) -> GifImage {
        GifImage {
            version: Version::Gif89a,
            screen_width: 4,
            screen_height: 4,
            color_resolution: 1,
            global_palette_sorted: false,
            background_index: 0, // black per palette_4()
            pixel_aspect_ratio: 0,
            global_palette: Some(palette_4()),
            blocks,
        }
    }

    /// Frame 1 fills top-left quadrant with red, disposal=None.
    /// Frame 2 fills bottom-right quadrant with green.
    /// Final canvas: top-left red, bottom-right green, rest = background black.
    #[test]
    fn disposal_none_keeps_previous_pixels() {
        let img = base_image(vec![
            Block::Image(small_frame(
                0,
                0,
                1,
                Some(GraphicControl {
                    disposal: DisposalMethod::None,
                    user_input: false,
                    transparent_index: None,
                    delay_centis: 10,
                }),
            )),
            Block::Image(small_frame(2, 2, 2, None)),
        ]);
        let frames = compose(&img).unwrap();
        assert_eq!(frames.len(), 2);
        let final_canvas = &frames[1].canvas;
        // Top-left red survives.
        assert_eq!(px(final_canvas, 0, 0), [0xFF, 0, 0, 0xFF]);
        assert_eq!(px(final_canvas, 1, 1), [0xFF, 0, 0, 0xFF]);
        // Bottom-right green just rendered.
        assert_eq!(px(final_canvas, 2, 2), [0, 0xFF, 0, 0xFF]);
        assert_eq!(px(final_canvas, 3, 3), [0, 0xFF, 0, 0xFF]);
        // Frame-1 timing preserved.
        assert_eq!(frames[0].delay_centis, 10);
    }

    /// disposal=Keep behaves identically to None for the visible
    /// pixel test (§23.c.iv distinguishes intent, not pixel state).
    #[test]
    fn disposal_keep_leaves_pixels_in_place() {
        let img = base_image(vec![
            Block::Image(small_frame(
                0,
                0,
                1,
                Some(GraphicControl {
                    disposal: DisposalMethod::Keep,
                    user_input: false,
                    transparent_index: None,
                    delay_centis: 0,
                }),
            )),
            Block::Image(small_frame(2, 2, 2, None)),
        ]);
        let frames = compose(&img).unwrap();
        let final_canvas = &frames[1].canvas;
        assert_eq!(px(final_canvas, 0, 0), [0xFF, 0, 0, 0xFF], "red kept");
        assert_eq!(px(final_canvas, 2, 2), [0, 0xFF, 0, 0xFF], "green drawn");
    }

    /// Frame 1 (red 2×2 top-left) with RestoreBackground disposal must
    /// clear that 2×2 region to background colour (palette index 0 =
    /// black, alpha 0xFF) before frame 2 renders.
    #[test]
    fn disposal_restore_background_clears_drawn_rect() {
        let img = base_image(vec![
            Block::Image(small_frame(
                0,
                0,
                1,
                Some(GraphicControl {
                    disposal: DisposalMethod::RestoreBackground,
                    user_input: false,
                    transparent_index: None,
                    delay_centis: 0,
                }),
            )),
            // Frame 2: 2×2 green at bottom-right; does not overlap frame 1.
            Block::Image(small_frame(2, 2, 2, None)),
        ]);
        let frames = compose(&img).unwrap();
        // Frame-1 snapshot still shows the red rect.
        assert_eq!(px(&frames[0].canvas, 0, 0), [0xFF, 0, 0, 0xFF]);
        // Frame-2 final state: red rect was wiped to bg-black before
        // frame 2 rendered its non-overlapping green quadrant.
        let f2 = &frames[1].canvas;
        assert_eq!(px(f2, 0, 0), [0, 0, 0, 0xFF], "wiped to bg-black");
        assert_eq!(px(f2, 1, 1), [0, 0, 0, 0xFF], "wiped to bg-black");
        assert_eq!(px(f2, 2, 2), [0, 0xFF, 0, 0xFF], "green drawn");
    }

    /// RestoreBackground with no global palette → clear to transparent.
    #[test]
    fn disposal_restore_background_no_palette_clears_transparent() {
        // Frame must carry its own local palette since stream has none.
        let local = palette_4();
        let f1 = Frame {
            left: 0,
            top: 0,
            width: 2,
            height: 2,
            local_palette: Some(local.clone()),
            palette_sorted: false,
            interlaced: false,
            indices: vec![1; 4], // red
            graphic_control: Some(GraphicControl {
                disposal: DisposalMethod::RestoreBackground,
                user_input: false,
                transparent_index: None,
                delay_centis: 0,
            }),
        };
        let f2 = Frame {
            left: 2,
            top: 2,
            width: 2,
            height: 2,
            local_palette: Some(local),
            palette_sorted: false,
            interlaced: false,
            indices: vec![2; 4], // green
            graphic_control: None,
        };
        let img = GifImage {
            version: Version::Gif89a,
            screen_width: 4,
            screen_height: 4,
            color_resolution: 1,
            global_palette_sorted: false,
            background_index: 0,
            pixel_aspect_ratio: 0,
            global_palette: None,
            blocks: vec![Block::Image(f1), Block::Image(f2)],
        };
        let frames = compose(&img).unwrap();
        // After restore-bg with no palette, area is transparent.
        let f2_canvas = &frames[1].canvas;
        assert_eq!(
            px(f2_canvas, 0, 0),
            [0, 0, 0, 0],
            "transparent (no palette)"
        );
        assert_eq!(px(f2_canvas, 2, 2), [0, 0xFF, 0, 0xFF]);
    }

    /// RestorePrevious must revert exactly to the canvas state visible
    /// before the frame rendered. Verify by stacking three frames:
    ///   F1: red 2×2 at (0,0), disposal=Keep   (becomes the saved baseline)
    ///   F2: blue 2×2 at (0,0), disposal=RestorePrevious
    ///   F3: green 2×2 at (2,2)
    /// After F2 disposes, the canvas should look like the post-F1
    /// state, and F3 then draws green into its quadrant without
    /// disturbing the red one.
    #[test]
    fn disposal_restore_previous_reverts_to_pre_render_canvas() {
        let img = base_image(vec![
            Block::Image(small_frame(
                0,
                0,
                1,
                Some(GraphicControl {
                    disposal: DisposalMethod::Keep,
                    user_input: false,
                    transparent_index: None,
                    delay_centis: 0,
                }),
            )),
            Block::Image(small_frame(
                0,
                0,
                3,
                Some(GraphicControl {
                    disposal: DisposalMethod::RestorePrevious,
                    user_input: false,
                    transparent_index: None,
                    delay_centis: 0,
                }),
            )),
            Block::Image(small_frame(2, 2, 2, None)),
        ]);
        let frames = compose(&img).unwrap();
        assert_eq!(frames.len(), 3);
        // F2 snapshot: blue temporarily covers red.
        assert_eq!(px(&frames[1].canvas, 0, 0), [0, 0, 0xFF, 0xFF]);
        // F3 final state: blue gone, red restored, green drawn.
        let f3 = &frames[2].canvas;
        assert_eq!(
            px(f3, 0, 0),
            [0xFF, 0, 0, 0xFF],
            "red restored after F2 revert"
        );
        assert_eq!(
            px(f3, 1, 1),
            [0xFF, 0, 0, 0xFF],
            "red restored after F2 revert"
        );
        assert_eq!(px(f3, 2, 2), [0, 0xFF, 0, 0xFF], "green drawn");
    }

    /// Transparent index in the GCE must skip writes (§23.c.viii):
    /// the previous canvas pixel value shows through.
    #[test]
    fn transparent_index_preserves_underlying_pixel() {
        // F1 fills top-left 2×2 with red (no transparency), Keep.
        // F2 overlaps F1 with a 2×2 frame whose pixels are [t, 2, 2, 2]
        //    where t is the transparent index → top-left of overlap
        //    keeps red, the other three show green.
        let f1 = small_frame(
            0,
            0,
            1,
            Some(GraphicControl {
                disposal: DisposalMethod::Keep,
                user_input: false,
                transparent_index: None,
                delay_centis: 0,
            }),
        );
        let mut f2 = small_frame(0, 0, 2, None);
        // Set first pixel of F2 to a transparent palette index (3).
        f2.indices[0] = 3;
        f2.graphic_control = Some(GraphicControl {
            disposal: DisposalMethod::None,
            user_input: false,
            transparent_index: Some(3),
            delay_centis: 0,
        });
        let img = base_image(vec![Block::Image(f1), Block::Image(f2)]);
        let frames = compose(&img).unwrap();
        let f2_canvas = &frames[1].canvas;
        // Pixel (0,0) — F2 said "transparent", red shows through.
        assert_eq!(px(f2_canvas, 0, 0), [0xFF, 0, 0, 0xFF]);
        // Pixel (1,0) — F2 said green.
        assert_eq!(px(f2_canvas, 1, 0), [0, 0xFF, 0, 0xFF]);
    }

    /// §25 Plain Text Extension renders glyphs from the crate-local
    /// 8×8 bitmap font in the foreground colour against a background-
    /// coloured cell rectangle. Verify by composing a single
    /// "A"-character cell at the canvas origin and checking that
    /// pixel coverage matches the font table.
    #[test]
    fn plain_text_renders_glyph_against_background() {
        // Build a stream whose only graphic block is a §25 Plain Text
        // extension placing one 8×8 cell at (0,0) with text "A".
        // Foreground = red (idx 1), Background = green (idx 2).
        let pt = crate::image::PlainText {
            left: 0,
            top: 0,
            width: 8,
            height: 8,
            cell_width: 8,
            cell_height: 8,
            fg_color_index: 1,
            bg_color_index: 2,
            text: b"A".to_vec(),
        };
        let img = GifImage {
            version: Version::Gif89a,
            screen_width: 8,
            screen_height: 8,
            color_resolution: 1,
            global_palette_sorted: false,
            background_index: 0,
            pixel_aspect_ratio: 0,
            global_palette: Some(palette_4()),
            blocks: vec![Block::PlainText {
                params: pt,
                graphic_control: None,
            }],
        };
        let frames = compose(&img).unwrap();
        assert_eq!(frames.len(), 1);
        let canvas = &frames[0].canvas;
        let glyph_a = crate::font::glyph(b'A');
        // Cross-check: for every pixel inside the 8×8 cell, font-bit
        // set → red, clear → green.
        for row in 0u8..8 {
            for col in 0u8..8 {
                let expected = if crate::font::pixel(&glyph_a, col, row) {
                    [0xFF, 0, 0, 0xFF]
                } else {
                    [0, 0xFF, 0, 0xFF]
                };
                assert_eq!(
                    px(canvas, col as u16, row as u16),
                    expected,
                    "mismatch at ({col},{row})"
                );
            }
        }
    }

    /// Plain Text participates in the §23 disposal state machine
    /// exactly like an image block — `RestoreBackground` after
    /// rendering clears the text-grid rectangle before the next
    /// frame.
    #[test]
    fn plain_text_disposal_restore_background_clears_grid() {
        let pt = crate::image::PlainText {
            left: 0,
            top: 0,
            width: 8,
            height: 8,
            cell_width: 8,
            cell_height: 8,
            fg_color_index: 1,
            bg_color_index: 2,
            text: b"#".to_vec(),
        };
        let img = GifImage {
            version: Version::Gif89a,
            screen_width: 8,
            screen_height: 8,
            color_resolution: 1,
            global_palette_sorted: false,
            background_index: 0, // black
            pixel_aspect_ratio: 0,
            global_palette: Some(palette_4()),
            blocks: vec![
                Block::PlainText {
                    params: pt,
                    graphic_control: Some(GraphicControl {
                        disposal: DisposalMethod::RestoreBackground,
                        user_input: false,
                        transparent_index: None,
                        delay_centis: 0,
                    }),
                },
                Block::Image(Frame {
                    left: 0,
                    top: 0,
                    width: 8,
                    height: 8,
                    local_palette: None,
                    palette_sorted: false,
                    interlaced: false,
                    indices: vec![3; 64], // all blue
                    graphic_control: None,
                }),
            ],
        };
        let frames = compose(&img).unwrap();
        assert_eq!(frames.len(), 2);
        // After RestoreBackground wipes the text-grid rect to bg-black,
        // the image frame then paints blue everywhere → final canvas
        // is solid blue (no leftover plain-text glyph pixels).
        let f2 = &frames[1].canvas;
        for row in 0..8 {
            for col in 0..8 {
                assert_eq!(px(f2, col, row), [0, 0, 0xFF, 0xFF]);
            }
        }
    }

    /// §25.a — "This block requires a Global Color Table to be
    /// available". Without a GCT the Plain Text block must be a
    /// no-op on the canvas (the cell rectangle stays whatever it
    /// was before).
    #[test]
    fn plain_text_with_no_global_palette_is_noop() {
        let pt = crate::image::PlainText {
            left: 0,
            top: 0,
            width: 8,
            height: 8,
            cell_width: 8,
            cell_height: 8,
            fg_color_index: 1,
            bg_color_index: 2,
            text: b"A".to_vec(),
        };
        let img = GifImage {
            version: Version::Gif89a,
            screen_width: 8,
            screen_height: 8,
            color_resolution: 1,
            global_palette_sorted: false,
            background_index: 0,
            pixel_aspect_ratio: 0,
            global_palette: None, // No GCT
            blocks: vec![Block::PlainText {
                params: pt,
                graphic_control: None,
            }],
        };
        let frames = compose(&img).unwrap();
        // Canvas remains transparent-black throughout (the §25.a
        // requirement is unmet so render_plain_text declines to
        // touch the canvas).
        for row in 0..8 {
            for col in 0..8 {
                assert_eq!(px(&frames[0].canvas, col, row), [0, 0, 0, 0]);
            }
        }
    }

    /// A frame whose placement rectangle escapes the logical screen
    /// must be rejected (the spec doesn't define wrap-around).
    #[test]
    fn out_of_bounds_placement_rejected() {
        let mut f = small_frame(3, 3, 1, None);
        f.width = 4;
        f.height = 4;
        f.indices = vec![1; 16];
        let img = base_image(vec![Block::Image(f)]);
        let err = compose(&img).unwrap_err();
        match err {
            Error::InvalidData(s) => assert!(s.contains("escapes logical screen")),
            _ => panic!("unexpected error: {err:?}"),
        }
    }
}
