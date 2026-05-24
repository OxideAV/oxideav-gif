//! High-level assembly of an animated [`GifImage`].
//!
//! The codec's [`crate::encoder`] serialises a fully-built
//! [`GifImage`], but assembling a *valid animated* one by hand is
//! fiddly: every frame needs a §23 Graphic Control Extension carrying
//! its Delay Time and Disposal Method, the looping behaviour rides on
//! the ecosystem NETSCAPE2.0 Application Extension (§26) rather than on
//! anything the CompuServe spec itself defines, and the §18 Logical
//! Screen has to be wide/tall enough to hold every frame's placement
//! rectangle.
//!
//! [`AnimationBuilder`] folds those pieces into one fluent API. It is
//! the encode-side counterpart to the timeline accessors on
//! [`GifImage`] ([`GifImage::frame_delays`],
//! [`GifImage::single_pass_duration`],
//! [`GifImage::total_play_duration`]): what you set here is exactly what
//! those readers report back after a decode round-trip.
//!
//! # Spec references
//!
//! * §18 "Logical Screen Descriptor" — the virtual canvas every frame
//!   composites onto, plus the Background Color Index that
//!   Restore-Background disposal reverts to.
//! * §20 "Image Descriptor" — each frame's `(left, top, width, height)`
//!   placement rectangle.
//! * §23 "Graphic Control Extension" — Delay Time (§23.c.vii, in
//!   hundredths of a second) and Disposal Method (§23.c.iv).
//! * §26 "Application Extension" — the NETSCAPE2.0 *Looping* sub-block
//!   (de-facto, documented in
//!   `docs/image/gif/netscape2.0-loop-extension.md`) is how a stream
//!   declares "loop forever" / "loop N times".
//!
//! # Example
//!
//! ```
//! use oxideav_gif::builder::AnimationBuilder;
//! use oxideav_gif::{DisposalMethod, Rgb};
//!
//! // A 2×2 two-frame flasher: red, then green, looping forever.
//! let palette = vec![Rgb::new(0xFF, 0, 0), Rgb::new(0, 0xFF, 0)];
//! let img = AnimationBuilder::new(2, 2, palette)
//!     .loop_forever()
//!     .add_full_frame(vec![0; 4], 50, DisposalMethod::None)
//!     .unwrap()
//!     .add_full_frame(vec![1; 4], 50, DisposalMethod::None)
//!     .unwrap()
//!     .build()
//!     .unwrap();
//!
//! assert!(img.is_animated());
//! assert_eq!(img.loop_count(), Some(0)); // loop forever
//! ```

use crate::app_ext::LoopControl;
use crate::error::{Error, Result};
use crate::image::{Block, DisposalMethod, Frame, GifImage, GraphicControl, Rgb, Version};

/// How a freshly-built animation should loop, mapped to the de-facto
/// NETSCAPE2.0 *Looping* sub-block convention documented in
/// `docs/image/gif/netscape2.0-loop-extension.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LoopBehaviour {
    /// No NETSCAPE2.0 block is emitted; per the de-facto convention an
    /// animation with no *Looping* sub-block plays exactly one pass.
    #[default]
    Once,
    /// Emit a NETSCAPE2.0 *Looping* sub-block with count `0` ("loop
    /// forever").
    Forever,
    /// Emit a NETSCAPE2.0 *Looping* sub-block with count `n`; the
    /// animation plays `n + 1` total passes.
    Repeat(u16),
}

/// Fluent assembler for an animated [`GifImage`].
///
/// Construct with [`AnimationBuilder::new`], push frames with
/// [`add_full_frame`](Self::add_full_frame) (a frame that fills the
/// whole logical screen) or [`add_placed_frame`](Self::add_placed_frame)
/// (a sub-rectangle), optionally pick the looping behaviour, then
/// [`build`](Self::build) to get a validated [`GifImage`] ready for
/// [`crate::encode`].
///
/// Every frame is stored against the single Global Color Table supplied
/// at construction (§18 / §21: a frame with the Local Color Table flag
/// clear uses the GCT), so the builder never emits per-frame Local Color
/// Tables. Callers that need per-frame palettes can construct
/// [`GifImage`] directly.
#[derive(Debug, Clone)]
pub struct AnimationBuilder {
    screen_width: u16,
    screen_height: u16,
    palette: Vec<Rgb>,
    background_index: u8,
    loop_behaviour: LoopBehaviour,
    frames: Vec<Frame>,
}

impl AnimationBuilder {
    /// Start a new animation targeting a `screen_width × screen_height`
    /// §18 Logical Screen, sharing `palette` as the §18 Global Color
    /// Table across every frame.
    ///
    /// The background index defaults to `0` and the loop behaviour to
    /// "play once" (no NETSCAPE2.0 block emitted). Use
    /// [`background_index`](Self::background_index),
    /// [`loop_forever`](Self::loop_forever), or
    /// [`loop_count`](Self::loop_count) to change those.
    pub fn new(screen_width: u16, screen_height: u16, palette: Vec<Rgb>) -> Self {
        Self {
            screen_width,
            screen_height,
            palette,
            background_index: 0,
            loop_behaviour: LoopBehaviour::Once,
            frames: Vec::new(),
        }
    }

    /// Set the §18.c.vii Background Color Index — the colour
    /// Restore-Background disposal clears a frame's rectangle to.
    pub fn background_index(mut self, index: u8) -> Self {
        self.background_index = index;
        self
    }

    /// Loop forever: emit a NETSCAPE2.0 *Looping* sub-block with count
    /// `0`. [`GifImage::loop_count`] reports `Some(0)` and
    /// [`GifImage::total_play_duration`] reports `None` (never
    /// terminates) for the result.
    pub fn loop_forever(mut self) -> Self {
        self.loop_behaviour = LoopBehaviour::Forever;
        self
    }

    /// Repeat the animation `count` times *after* the initial pass, for
    /// `count + 1` total passes (the de-facto NETSCAPE2.0 convention).
    /// A NETSCAPE2.0 *Looping* sub-block with that count is emitted.
    pub fn loop_count(mut self, count: u16) -> Self {
        self.loop_behaviour = LoopBehaviour::Repeat(count);
        self
    }

    /// Play exactly one pass: no NETSCAPE2.0 block is emitted. This is
    /// the default and is provided for symmetry / explicitness.
    pub fn play_once(mut self) -> Self {
        self.loop_behaviour = LoopBehaviour::Once;
        self
    }

    /// Append a frame that fills the entire logical screen at
    /// `(0, 0)`, with the given §23.c.vii Delay Time (hundredths of a
    /// second) and §23.c.iv Disposal Method.
    ///
    /// `indices` must contain exactly `screen_width × screen_height`
    /// palette indices in row-major top-to-bottom order. Each index
    /// must address an entry in the shared palette.
    ///
    /// Consumes and returns `self` so calls chain with `?`; a
    /// validation failure is reported immediately (fail-fast).
    pub fn add_full_frame(
        self,
        indices: Vec<u8>,
        delay_centis: u16,
        disposal: DisposalMethod,
    ) -> Result<Self> {
        let w = self.screen_width;
        let h = self.screen_height;
        self.add_placed_frame(0, 0, w, h, indices, delay_centis, disposal)
    }

    /// Append a frame placed at `(left, top)` covering `width × height`
    /// pixels inside the logical screen, with the given §23.c.vii Delay
    /// Time and §23.c.iv Disposal Method.
    ///
    /// Validates that:
    /// * the placement rectangle stays within the §18 Logical Screen
    ///   (the compositor rejects out-of-bounds frames, so the builder
    ///   refuses to produce one);
    /// * `indices.len() == width × height`;
    /// * every index addresses an entry in the shared palette.
    #[allow(clippy::too_many_arguments)]
    pub fn add_placed_frame(
        mut self,
        left: u16,
        top: u16,
        width: u16,
        height: u16,
        indices: Vec<u8>,
        delay_centis: u16,
        disposal: DisposalMethod,
    ) -> Result<Self> {
        self.validate_frame(left, top, width, height, &indices)?;
        self.frames.push(Frame {
            left,
            top,
            width,
            height,
            local_palette: None,
            palette_sorted: false,
            interlaced: false,
            indices,
            graphic_control: Some(GraphicControl {
                disposal,
                user_input: false,
                transparent_index: None,
                delay_centis,
            }),
        });
        Ok(self)
    }

    fn validate_frame(
        &self,
        left: u16,
        top: u16,
        width: u16,
        height: u16,
        indices: &[u8],
    ) -> Result<()> {
        // §18 / §20 — the placement rectangle must fit inside the
        // Logical Screen. Compute in u32 so no u16 add overflows.
        let right = left as u32 + width as u32;
        let bottom = top as u32 + height as u32;
        if right > self.screen_width as u32 || bottom > self.screen_height as u32 {
            return Err(Error::InvalidInput(format!(
                "frame placement ({left},{top},{width}×{height}) escapes logical screen \
                 ({}×{})",
                self.screen_width, self.screen_height
            )));
        }
        let expected = width as usize * height as usize;
        if indices.len() != expected {
            return Err(Error::InvalidInput(format!(
                "frame indices length {} != width*height {expected}",
                indices.len()
            )));
        }
        // §21 — with no Local Color Table the frame resolves indices
        // against the shared Global Color Table; an index past the end
        // of it has no colour.
        if let Some(&bad) = indices
            .iter()
            .find(|&&i| (i as usize) >= self.palette.len())
        {
            return Err(Error::InvalidInput(format!(
                "pixel index {bad} >= palette size {}",
                self.palette.len()
            )));
        }
        Ok(())
    }

    /// Finalise the builder into a validated, animation-ready
    /// [`GifImage`].
    ///
    /// The result is always [`Version::Gif89a`] (every frame carries a
    /// §23 GCE, which requires 89a per §7). When the loop behaviour is
    /// anything other than "play once" a NETSCAPE2.0 *Looping*
    /// Application Extension is inserted ahead of the first frame.
    ///
    /// # Errors
    ///
    /// * `InvalidInput` when no frame was added (an empty animation is
    ///   not a valid Data Stream — §B requires at least the Trailer, but
    ///   a frameless "animation" is almost certainly a caller bug).
    /// * `InvalidInput` when the palette is empty or larger than 256
    ///   entries (§19 colour-table size limits).
    pub fn build(self) -> Result<GifImage> {
        if self.frames.is_empty() {
            return Err(Error::InvalidInput(
                "animation has no frames; add at least one before build()".into(),
            ));
        }
        if self.palette.is_empty() {
            return Err(Error::InvalidInput(
                "global colour table must contain at least one entry".into(),
            ));
        }
        if self.palette.len() > 256 {
            return Err(Error::InvalidInput(format!(
                "global colour table has {} entries; max is 256",
                self.palette.len()
            )));
        }

        let mut blocks: Vec<Block> = Vec::with_capacity(self.frames.len() + 1);

        // §26 — emit the NETSCAPE2.0 *Looping* block ahead of the first
        // frame for any non-"play once" behaviour. The de-facto reader
        // convention is that the block precedes the frames.
        match self.loop_behaviour {
            LoopBehaviour::Once => {}
            LoopBehaviour::Forever => {
                blocks.push(Block::Application(
                    LoopControl {
                        loop_count: Some(0),
                        buffer_size: None,
                    }
                    .to_application(),
                ));
            }
            LoopBehaviour::Repeat(n) => {
                blocks.push(Block::Application(
                    LoopControl {
                        loop_count: Some(n),
                        buffer_size: None,
                    }
                    .to_application(),
                ));
            }
        }

        for frame in self.frames {
            blocks.push(Block::Image(frame));
        }

        Ok(GifImage {
            // §7 — every frame carries a §23 GCE, so the minimum
            // covering version is 89a.
            version: Version::Gif89a,
            screen_width: self.screen_width,
            screen_height: self.screen_height,
            // §18.c.iv — Color Resolution describes the *original*
            // palette's bit depth. With a shared GCT, "bits to address
            // the table minus 1" is the honest value.
            color_resolution: color_resolution_for(self.palette.len()),
            global_palette_sorted: false,
            background_index: self.background_index,
            pixel_aspect_ratio: 0,
            global_palette: Some(self.palette),
            blocks,
        })
    }
}

/// §18.c.iv Color Resolution = "bits per primary minus 1". With a
/// shared palette the most honest value is the number of bits needed to
/// address the table, minus one. A 1-entry palette still needs 1 bit
/// (Color Resolution 0); a 256-entry palette needs 8 bits (Color
/// Resolution 7). The field is a 3-bit subfield, so the result is
/// clamped to `0..=7`.
fn color_resolution_for(palette_len: usize) -> u8 {
    // Bits to address `palette_len` distinct entries (≥ 1).
    let mut bits = 1u8;
    while (1usize << bits as u32) < palette_len {
        bits += 1;
    }
    (bits - 1).min(7)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::Block;

    fn pal2() -> Vec<Rgb> {
        vec![Rgb::new(0xFF, 0, 0), Rgb::new(0, 0xFF, 0)]
    }

    /// Two full-screen frames + loop-forever → a valid animated stream
    /// whose timeline accessors report exactly what was set.
    #[test]
    fn builds_two_frame_looping_animation() {
        let img = AnimationBuilder::new(2, 2, pal2())
            .loop_forever()
            .add_full_frame(vec![0; 4], 50, DisposalMethod::None)
            .unwrap()
            .add_full_frame(vec![1; 4], 50, DisposalMethod::None)
            .unwrap()
            .build()
            .unwrap();

        assert_eq!(img.version, Version::Gif89a);
        assert!(img.is_animated());
        assert_eq!(img.loop_count(), Some(0));
        assert_eq!(img.frames().count(), 2);
        // Two frames at 0.50 s each → 1.0 s per pass; loop forever →
        // total is None.
        assert_eq!(
            img.single_pass_duration(),
            core::time::Duration::from_secs(1)
        );
        assert_eq!(img.total_play_duration(), None);
    }

    /// The result round-trips through encode → decode, and the decoded
    /// timeline matches the source.
    #[test]
    fn builder_output_encode_decode_roundtrips() {
        let img = AnimationBuilder::new(2, 1, pal2())
            .loop_count(2)
            .add_full_frame(vec![0, 1], 10, DisposalMethod::RestoreBackground)
            .unwrap()
            .add_full_frame(vec![1, 0], 20, DisposalMethod::None)
            .unwrap()
            .build()
            .unwrap();

        let bytes = crate::encode(&img).unwrap();
        let decoded = crate::decode(&bytes).unwrap();
        assert_eq!(decoded, img, "encode → decode is byte-stable for the build");
        assert_eq!(decoded.loop_count(), Some(2));
        // Some(2) → 3 passes; 0.30 s per pass → 0.90 s total.
        assert_eq!(
            decoded.total_play_duration(),
            Some(core::time::Duration::from_millis(900))
        );
    }

    /// "Play once" emits no NETSCAPE2.0 block, so loop_count is None.
    #[test]
    fn play_once_emits_no_netscape_block() {
        let img = AnimationBuilder::new(1, 1, pal2())
            .add_full_frame(vec![0], 5, DisposalMethod::None)
            .unwrap()
            .add_full_frame(vec![1], 5, DisposalMethod::None)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(img.loop_count(), None);
        assert_eq!(img.application_extensions().count(), 0);
        // One pass: total == single pass.
        assert_eq!(img.total_play_duration(), Some(img.single_pass_duration()));
    }

    /// A placed sub-rectangle frame composites within the logical
    /// screen and survives the compositor's bounds check.
    #[test]
    fn placed_frame_within_screen_composites() {
        let img = AnimationBuilder::new(4, 4, pal2())
            .add_full_frame(vec![0; 16], 10, DisposalMethod::None)
            .unwrap()
            .add_placed_frame(2, 2, 2, 2, vec![1; 4], 10, DisposalMethod::None)
            .unwrap()
            .build()
            .unwrap();
        // The compositor accepts the build (placement in-bounds).
        let frames = crate::compose(&img).unwrap();
        assert_eq!(frames.len(), 2);
    }

    /// A placement rectangle escaping the logical screen is rejected at
    /// add time (fail-fast Result path).
    #[test]
    fn placement_out_of_bounds_rejected() {
        let err = AnimationBuilder::new(2, 2, pal2())
            .add_placed_frame(1, 1, 2, 2, vec![0; 4], 10, DisposalMethod::None)
            .unwrap_err();
        assert!(matches!(err, Error::InvalidInput(s) if s.contains("escapes logical screen")));
    }

    /// Wrong index count is rejected at add time.
    #[test]
    fn wrong_index_count_rejected() {
        let err = AnimationBuilder::new(2, 2, pal2())
            .add_full_frame(vec![0; 3], 10, DisposalMethod::None)
            .unwrap_err();
        assert!(matches!(err, Error::InvalidInput(s) if s.contains("indices length")));
    }

    /// An index outside the palette is rejected.
    #[test]
    fn index_out_of_palette_rejected() {
        let err = AnimationBuilder::new(1, 1, pal2())
            .add_full_frame(vec![5], 10, DisposalMethod::None)
            .unwrap_err();
        assert!(matches!(err, Error::InvalidInput(s) if s.contains("palette size")));
    }

    /// build() with no frames is an error.
    #[test]
    fn empty_animation_rejected() {
        let err = AnimationBuilder::new(2, 2, pal2()).build().unwrap_err();
        assert!(matches!(err, Error::InvalidInput(s) if s.contains("no frames")));
    }

    /// A palette larger than the §19 limit (256 entries) is rejected at
    /// build time. The frame indices stay in range (all `0`) so the
    /// only thing that can fail is the palette-size check.
    #[test]
    fn oversize_palette_rejected() {
        let big = vec![Rgb::new(0, 0, 0); 257];
        let err = AnimationBuilder::new(1, 1, big)
            .add_full_frame(vec![0], 0, DisposalMethod::None)
            .unwrap()
            .build()
            .unwrap_err();
        assert!(matches!(err, Error::InvalidInput(s) if s.contains("max is 256")));
    }

    /// An empty palette can never carry a valid frame (the index check
    /// needs at least one entry), so the first thing `build()` reports
    /// for a frameless empty-palette builder is "no frames".
    #[test]
    fn empty_palette_with_no_frames_reports_no_frames_first() {
        let err = AnimationBuilder::new(1, 1, Vec::new()).build().unwrap_err();
        assert!(matches!(err, Error::InvalidInput(s) if s.contains("no frames")));
    }

    /// background_index is threaded into the §18 Logical Screen.
    #[test]
    fn background_index_is_applied() {
        let img = AnimationBuilder::new(1, 1, pal2())
            .background_index(1)
            .add_full_frame(vec![0], 0, DisposalMethod::None)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(img.background_index, 1);
    }

    /// The NETSCAPE2.0 loop block is inserted *before* the first image
    /// block (de-facto reader convention).
    #[test]
    fn loop_block_precedes_first_frame() {
        let img = AnimationBuilder::new(1, 1, pal2())
            .loop_forever()
            .add_full_frame(vec![0], 0, DisposalMethod::None)
            .unwrap()
            .build()
            .unwrap();
        assert!(matches!(img.blocks.first(), Some(Block::Application(_))));
        assert!(matches!(img.blocks.get(1), Some(Block::Image(_))));
    }

    /// color_resolution_for maps palette size to the §18.c.iv 3-bit
    /// field (bits-to-address minus one, clamped to 0..=7).
    #[test]
    fn color_resolution_table() {
        assert_eq!(color_resolution_for(1), 0);
        assert_eq!(color_resolution_for(2), 0);
        assert_eq!(color_resolution_for(3), 1);
        assert_eq!(color_resolution_for(4), 1);
        assert_eq!(color_resolution_for(5), 2);
        assert_eq!(color_resolution_for(16), 3);
        assert_eq!(color_resolution_for(256), 7);
    }
}
