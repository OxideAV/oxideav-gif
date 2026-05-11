//! Animation playback iterators over a parsed [`crate::GifImage`].
//!
//! [`crate::compose::compose`] eagerly composites every image-bearing
//! block in the stream into a `Vec<ComposedFrame>`, which is fine for
//! short loops but burns `screen_width * screen_height * 4 * N` bytes
//! up front for an N-frame animation. The iterators here lazily walk
//! the same §23 disposal-method state machine, yielding one
//! [`ComposedFrame`] per call and freeing the previous canvas as the
//! consumer drops it.
//!
//! Two playback shapes are exposed:
//!
//! * [`Playback::frames`] — yields each image-bearing block in source
//!   order, exactly once. Equivalent to `compose(image)?.into_iter()`
//!   but lazy. Returns `Some(Err)` on the first malformed frame, then
//!   `None`.
//!
//! * [`Playback::looping_frames`] — yields the same sequence honouring
//!   the NETSCAPE2.0 *Looping* sub-block (see
//!   [`crate::GifImage::loop_count`]):
//!     - no NETSCAPE2.0 block → play exactly once (one pass).
//!     - `loop_count == Some(0)` → play forever.
//!     - `loop_count == Some(N)` → play `N + 1` times in total per the
//!       de-facto convention documented in
//!       `docs/image/gif/netscape2.0-loop-extension.md`.
//!
//! Both iterators yield `(canvas, delay)` tuples so a downstream player
//! can `sleep(delay)` between presentations without re-parsing the
//! delay from the underlying [`crate::GraphicControl`].

use core::time::Duration;

use crate::compose::RgbaCanvas;
use crate::error::{Error, Result};
use crate::image::{Block, DisposalMethod, Frame, GifImage, Rgb};

/// Playback handle for a parsed [`GifImage`]. Cheap to construct;
/// holds a borrow of the image plus enough state to walk the §23
/// disposal-method state machine without re-allocating per iteration.
pub struct Playback<'a> {
    image: &'a GifImage,
}

impl<'a> Playback<'a> {
    /// Bind a [`Playback`] to `image`. The image is borrowed for the
    /// lifetime of every iterator subsequently spawned.
    pub fn new(image: &'a GifImage) -> Self {
        Self { image }
    }

    /// Yield each image-bearing block in source order, exactly once,
    /// composited per the §23 disposal-method state machine. Plain
    /// Text Extensions are skipped (see [`crate::compose`] for the
    /// rationale).
    ///
    /// The iterator yields `Some(Err(_))` on the first malformed
    /// frame and `None` after that — it does not attempt to recover.
    pub fn frames(&self) -> FrameIter<'a> {
        FrameIter::new(self.image)
    }

    /// Yield frames honouring the NETSCAPE2.0 loop-count sub-block.
    /// See module docs for the loop-count semantics.
    pub fn looping_frames(&self) -> LoopingFrameIter<'a> {
        LoopingFrameIter::new(self.image)
    }
}

/// One frame yielded by [`FrameIter`] / [`LoopingFrameIter`]. Same
/// shape as [`ComposedFrame`] but with the delay surfaced as a
/// [`Duration`] for ergonomic `thread::sleep` calls. The 0.01 s base
/// unit is exact in `Duration` (10 ms = 10_000_000 ns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackFrame {
    pub canvas: RgbaCanvas,
    /// Delay before the next frame should be presented. Zero when the
    /// frame had no Graphic Control Extension, or when the GCE
    /// specified zero centiseconds.
    pub delay: Duration,
}

/// Convert a §23.c.vii hundredths-of-a-second value to a [`Duration`].
fn centis_to_duration(centis: u16) -> Duration {
    // 1 centisecond = 10 milliseconds = 10_000_000 nanoseconds.
    Duration::from_millis(centis as u64 * 10)
}

// ---------------------------------------------------------------------
// Single-pass frame iterator.
// ---------------------------------------------------------------------

/// Walks `image.blocks` once, advancing the canvas state machine on
/// each call to [`Iterator::next`].
pub struct FrameIter<'a> {
    image: &'a GifImage,
    canvas: RgbaCanvas,
    background_rgba: [u8; 4],
    /// Cursor into `image.blocks`. Bumped past every block, not just
    /// image blocks, so non-image blocks transparently skip past.
    cursor: usize,
    /// Set on the first error so subsequent calls return `None`
    /// rather than re-emitting the error or re-attempting parsing.
    finished: bool,
}

impl<'a> FrameIter<'a> {
    fn new(image: &'a GifImage) -> Self {
        let canvas = blank_canvas(image.screen_width, image.screen_height);
        let background_rgba = compute_background_rgba(image);
        Self {
            image,
            canvas,
            background_rgba,
            cursor: 0,
            finished: false,
        }
    }
}

impl<'a> Iterator for FrameIter<'a> {
    type Item = Result<PlaybackFrame>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        // Find the next image-bearing block.
        let frame = loop {
            let block = self.image.blocks.get(self.cursor)?;
            self.cursor += 1;
            match block {
                Block::Image(f) => break f,
                // §25 Plain Text and the §24 / §26 special-purpose
                // blocks have no rendered output: skip without
                // touching the canvas.
                _ => continue,
            }
        };

        if let Err(e) = check_frame_in_screen(self.image, frame) {
            self.finished = true;
            return Some(Err(e));
        }

        // §23.f.i — snapshot the pre-render canvas so a later
        // RestorePrevious can revert to exactly the image the user
        // was looking at before this frame appeared.
        let pre_render_snapshot = self.canvas.pixels.clone();

        if let Err(e) = render_frame(
            &mut self.canvas,
            frame,
            self.image.global_palette.as_deref(),
        ) {
            self.finished = true;
            return Some(Err(e));
        }

        let delay_centis = frame
            .graphic_control
            .as_ref()
            .map(|gce| gce.delay_centis)
            .unwrap_or(0);

        let yielded = PlaybackFrame {
            canvas: self.canvas.clone(),
            delay: centis_to_duration(delay_centis),
        };

        // Apply this frame's disposal method to *prepare* the canvas
        // for the next frame. Identical to compose.rs.
        let disposal = frame
            .graphic_control
            .as_ref()
            .map(|gce| gce.disposal)
            .unwrap_or(DisposalMethod::None);
        match disposal {
            DisposalMethod::None | DisposalMethod::Keep => {}
            DisposalMethod::RestoreBackground => {
                clear_rect_to_color(&mut self.canvas, frame, self.background_rgba);
            }
            DisposalMethod::RestorePrevious => {
                self.canvas.pixels = pre_render_snapshot;
            }
        }

        Some(Ok(yielded))
    }
}

// ---------------------------------------------------------------------
// Looping frame iterator (NETSCAPE2.0-aware).
// ---------------------------------------------------------------------

/// Walks `image.blocks` repeatedly per the NETSCAPE2.0 loop-count
/// sub-block. Lazily restarts the underlying [`FrameIter`] when one
/// pass ends.
pub struct LoopingFrameIter<'a> {
    image: &'a GifImage,
    inner: FrameIter<'a>,
    /// Total passes the consumer should see: `Some(n)` for finite
    /// playback (consumer should see exactly `n` complete passes),
    /// `None` for infinite. Decoded from `loop_count`:
    ///   * absent (no NETSCAPE2.0 block)        → `Some(1)` — one pass.
    ///   * `Some(0)` (loop forever)             → `None`.
    ///   * `Some(n)` where `n >= 1`             → `Some(n + 1)` — one
    ///     initial pass plus `n` repeats per the NETSCAPE2.0
    ///     de-facto convention.
    total_passes: Option<u32>,
    passes_completed: u32,
    /// `true` when the image carries no image-bearing blocks. With
    /// loop-forever semantics the iterator would otherwise spin
    /// completing zero-frame passes; this flag short-circuits to
    /// `None` immediately.
    image_block_free: bool,
}

impl<'a> LoopingFrameIter<'a> {
    fn new(image: &'a GifImage) -> Self {
        let total_passes = match image.loop_count() {
            None => Some(1),
            Some(0) => None,
            Some(n) => Some((n as u32) + 1),
        };
        let image_block_free = !image.blocks.iter().any(|b| matches!(b, Block::Image(_)));
        Self {
            image,
            inner: FrameIter::new(image),
            total_passes,
            passes_completed: 0,
            image_block_free,
        }
    }

    /// Reset the inner [`FrameIter`] to start a fresh playback pass.
    /// Used after the previous pass yielded `None`.
    fn restart(&mut self) {
        self.inner = FrameIter::new(self.image);
    }

    /// `true` when no further passes should be attempted.
    fn passes_exhausted(&self) -> bool {
        match self.total_passes {
            None => false,
            Some(total) => self.passes_completed >= total,
        }
    }
}

impl<'a> Iterator for LoopingFrameIter<'a> {
    type Item = Result<PlaybackFrame>;

    fn next(&mut self) -> Option<Self::Item> {
        // Short-circuit: a stream with zero image-bearing blocks
        // contributes nothing per pass, so even loop-forever should
        // terminate immediately rather than spin restarting empty
        // inner iterators.
        if self.image_block_free {
            return None;
        }
        loop {
            if self.passes_exhausted() {
                return None;
            }
            match self.inner.next() {
                Some(item) => return Some(item),
                None => {
                    // Inner pass complete. Either start the next
                    // pass or signal end-of-playback.
                    self.passes_completed += 1;
                    if self.passes_exhausted() {
                        return None;
                    }
                    self.restart();
                }
            }
        }
    }
}

// ---------------------------------------------------------------------
// Helpers shared with compose.rs.
//
// These are duplicated rather than re-exported because compose.rs is
// the eager-mode reference implementation and its helpers are private
// to that module. Keeping a separate copy here lets the iterators
// evolve independently — e.g. the lazy iterator could in future skip
// a `clone()` when the consumer only reads the canvas borrow.
// ---------------------------------------------------------------------

fn compute_background_rgba(image: &GifImage) -> [u8; 4] {
    // §18.c.iii — background colour index is meaningful only with a
    // Global Color Table. Without one, dispose-to-background clears
    // to fully-transparent black.
    match &image.global_palette {
        Some(palette) => {
            let idx = image.background_index as usize;
            if idx >= palette.len() {
                [0, 0, 0, 0]
            } else {
                let Rgb { r, g, b } = palette[idx];
                [r, g, b, 0xFF]
            }
        }
        None => [0, 0, 0, 0],
    }
}

fn check_frame_in_screen(image: &GifImage, frame: &Frame) -> Result<()> {
    let right = (frame.left as u32) + (frame.width as u32);
    let bottom = (frame.top as u32) + (frame.height as u32);
    if right > image.screen_width as u32 || bottom > image.screen_height as u32 {
        return Err(Error::InvalidData(format!(
            "frame placement ({},{},{}×{}) escapes logical screen ({}×{})",
            frame.left,
            frame.top,
            frame.width,
            frame.height,
            image.screen_width,
            image.screen_height
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
            // the display device.
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

fn clear_rect_to_color(canvas: &mut RgbaCanvas, frame: &Frame, rgba: [u8; 4]) {
    let canvas_w = canvas.width as usize;
    let frame_w = frame.width as usize;
    let frame_h = frame.height as usize;
    let frame_left = frame.left as usize;
    let frame_top = frame.top as usize;
    for fy in 0..frame_h {
        let dst_row_start = ((frame_top + fy) * canvas_w + frame_left) * 4;
        for fx in 0..frame_w {
            let dst = dst_row_start + fx * 4;
            canvas.pixels[dst..dst + 4].copy_from_slice(&rgba);
        }
    }
}

/// Build a fresh transparent-black canvas of the given size. Mirrors
/// the private `RgbaCanvas::new` in [`crate::compose`] without
/// widening the public API surface.
fn blank_canvas(width: u16, height: u16) -> RgbaCanvas {
    let n = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .expect("canvas dimensions overflow usize");
    RgbaCanvas {
        width,
        height,
        pixels: vec![0u8; n],
    }
}

// ---------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_ext::LoopControl;
    use crate::image::{Block, Frame, GifImage, GraphicControl, Rgb, Version};

    fn palette_4() -> Vec<Rgb> {
        vec![
            Rgb::new(0, 0, 0),    // 0 - black (also background)
            Rgb::new(0xFF, 0, 0), // 1 - red
            Rgb::new(0, 0xFF, 0), // 2 - green
            Rgb::new(0, 0, 0xFF), // 3 - blue
        ]
    }

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
            background_index: 0,
            pixel_aspect_ratio: 0,
            global_palette: Some(palette_4()),
            blocks,
        }
    }

    fn px(c: &RgbaCanvas, x: u16, y: u16) -> [u8; 4] {
        let off = ((y as usize) * (c.width as usize) + (x as usize)) * 4;
        [
            c.pixels[off],
            c.pixels[off + 1],
            c.pixels[off + 2],
            c.pixels[off + 3],
        ]
    }

    /// FrameIter yields the same sequence as eager `compose`.
    #[test]
    fn frame_iter_matches_eager_compose() {
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
        let eager = crate::compose::compose(&img).unwrap();
        let lazy: Vec<PlaybackFrame> = Playback::new(&img)
            .frames()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(eager.len(), lazy.len());
        for (e, l) in eager.iter().zip(lazy.iter()) {
            assert_eq!(e.canvas, l.canvas);
            assert_eq!(centis_to_duration(e.delay_centis), l.delay);
        }
    }

    /// FrameIter respects the §23 disposal state machine end-to-end.
    #[test]
    fn frame_iter_restore_previous_reverts() {
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
        let frames: Vec<_> = Playback::new(&img)
            .frames()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(frames.len(), 3);
        assert_eq!(
            px(&frames[1].canvas, 0, 0),
            [0, 0, 0xFF, 0xFF],
            "blue shows"
        );
        let f3 = &frames[2].canvas;
        assert_eq!(px(f3, 0, 0), [0xFF, 0, 0, 0xFF], "red restored");
        assert_eq!(px(f3, 2, 2), [0, 0xFF, 0, 0xFF], "green drawn");
    }

    /// FrameIter delay is exposed as a Duration with the §23.c.vii
    /// 10 ms-per-centisecond conversion applied.
    #[test]
    fn delay_converts_centiseconds_to_duration() {
        let img = base_image(vec![Block::Image(small_frame(
            0,
            0,
            1,
            Some(GraphicControl {
                disposal: DisposalMethod::None,
                user_input: false,
                transparent_index: None,
                delay_centis: 50, // 0.5 s
            }),
        ))]);
        let frame = Playback::new(&img).frames().next().unwrap().unwrap();
        assert_eq!(frame.delay, Duration::from_millis(500));
    }

    /// Looping with no NETSCAPE2.0 block plays exactly one pass.
    #[test]
    fn looping_no_extension_plays_once() {
        let img = base_image(vec![
            Block::Image(small_frame(0, 0, 1, None)),
            Block::Image(small_frame(2, 2, 2, None)),
        ]);
        let frames: Vec<_> = Playback::new(&img)
            .looping_frames()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(frames.len(), 2);
    }

    /// `loop_count = Some(2)` plays 1 + 2 = 3 passes total.
    #[test]
    fn looping_finite_count_replays_n_plus_one() {
        let netscape = LoopControl {
            loop_count: Some(2),
            buffer_size: None,
        }
        .to_application();
        let img = base_image(vec![
            Block::Application(netscape),
            Block::Image(small_frame(0, 0, 1, None)),
            Block::Image(small_frame(2, 2, 2, None)),
        ]);
        // Two image blocks per pass, three passes → six frames yielded.
        let frames: Vec<_> = Playback::new(&img)
            .looping_frames()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(frames.len(), 6);
    }

    /// `loop_count = Some(0)` is the de-facto "loop forever" sentinel.
    /// Verify by yanking 1000 frames via `take()` — none should be
    /// `None` and they should cycle through the same two-frame
    /// sequence.
    #[test]
    fn looping_zero_means_forever() {
        let netscape = LoopControl {
            loop_count: Some(0),
            buffer_size: None,
        }
        .to_application();
        let img = base_image(vec![
            Block::Application(netscape),
            Block::Image(small_frame(0, 0, 1, None)),
            Block::Image(small_frame(2, 2, 2, None)),
        ]);
        let frames: Vec<_> = Playback::new(&img)
            .looping_frames()
            .take(1000)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(frames.len(), 1000);
    }

    /// An image with no image-bearing blocks but a "loop forever"
    /// extension must NOT spin: the iterator should immediately
    /// return `None`.
    #[test]
    fn looping_forever_with_no_image_blocks_terminates() {
        let netscape = LoopControl {
            loop_count: Some(0),
            buffer_size: None,
        }
        .to_application();
        let img = base_image(vec![Block::Application(netscape)]);
        let mut it = Playback::new(&img).looping_frames();
        assert!(it.next().is_none());
    }

    /// FrameIter returns the malformed-frame error on first encounter
    /// then yields `None`.
    #[test]
    fn frame_iter_terminates_after_error() {
        // Frame placement escapes the 4×4 logical screen.
        let mut bad = small_frame(3, 3, 1, None);
        bad.width = 4;
        bad.height = 4;
        bad.indices = vec![1; 16];
        let img = base_image(vec![Block::Image(bad)]);
        let mut it = Playback::new(&img).frames();
        assert!(matches!(it.next(), Some(Err(Error::InvalidData(_)))));
        assert!(it.next().is_none());
    }
}
