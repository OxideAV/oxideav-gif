//! High-level assembly of an animated [`GifFile`].
//!
//! The codec's [`crate::encoder`] serialises a fully-built
//! [`GifFile`], but assembling a *valid animated* one by hand is
//! fiddly: every frame needs a §23 Graphic Control Extension carrying
//! its Delay Time and Disposal Method, the looping behaviour rides on
//! the ecosystem NETSCAPE2.0 Application Extension (§26) rather than on
//! anything the CompuServe spec itself defines, and the §18 Logical
//! Screen has to be wide/tall enough to hold every frame's placement
//! rectangle.
//!
//! [`AnimationBuilder`] folds those pieces into one fluent API. It is
//! the encode-side counterpart to the timeline accessors on
//! [`GifFile`] ([`GifFile::frame_delays`],
//! [`GifFile::single_pass_duration`],
//! [`GifFile::total_play_duration`]): what you set here is exactly what
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

use core::time::Duration;

use crate::app_ext::LoopControl;
use crate::error::{Error, Result};
use crate::image::{Block, DisposalMethod, GifFile, GifFrameData, GraphicControl, Rgb, Version};
use crate::quantize::QuantizeOptions;

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

/// One image of an [`AnimationBuilder`] stream with every per-image
/// container field under the caller's control: the §20 placement
/// rectangle and Interlace Flag, an optional §21 Local Color Table (and
/// its §20.c.viii Sort Flag), and the §23 Graphic Control Extension —
/// Disposal Method, User Input Flag, Transparency Index, Delay Time — or
/// no Graphic Control Extension at all.
///
/// [`FrameSpec::new`] starts a frame at `(0, 0)` that uses the Global
/// Color Table and carries a default Graphic Control Extension (no
/// disposal, no delay, no transparency, no user input);
/// [`FrameSpec::from_rgba8`] quantises truecolour pixels into a frame
/// with its own Local Color Table. Adjust with the `with_*` builders and
/// hand it to [`AnimationBuilder::add_frame`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FrameSpec {
    /// §20.c.ii Image Left Position.
    pub left: u16,
    /// §20.c.iii Image Top Position.
    pub top: u16,
    /// §20.c.iv Image Width.
    pub width: u16,
    /// §20.c.v Image Height.
    pub height: u16,
    /// One colour-table index per pixel, row-major, `width × height`.
    pub indices: Vec<u8>,
    /// `Some` writes a §21 Local Color Table (1..=256 entries) for this
    /// image; `None` uses the builder's Global Color Table.
    pub local_palette: Option<Vec<Rgb>>,
    /// §20.c.viii Sort Flag of the Local Color Table.
    pub palette_sorted: bool,
    /// §20.c.vii Interlace Flag; `None` takes the builder's current
    /// [`AnimationBuilder::interlaced`] setting.
    pub interlaced: Option<bool>,
    /// The §23 Graphic Control Extension written ahead of the image;
    /// `None` writes none (the only GIF87a-compatible choice).
    pub graphic_control: Option<GraphicControl>,
}

impl FrameSpec {
    /// A `width × height` frame at `(0, 0)` against the Global Color
    /// Table, with a default Graphic Control Extension.
    pub fn new(width: u16, height: u16, indices: Vec<u8>) -> Self {
        Self {
            left: 0,
            top: 0,
            width,
            height,
            indices,
            local_palette: None,
            palette_sorted: false,
            interlaced: None,
            graphic_control: Some(GraphicControl::default()),
        }
    }

    /// Quantise tightly packed RGBA (`4 × width × height` bytes) with
    /// `quantize` into a frame carrying its own §21 Local Color Table.
    /// Pixels with alpha below
    /// [`crate::quantize::ALPHA_OPAQUE_THRESHOLD`] map to one entry,
    /// which becomes the frame's §23.c.viii Transparency Index.
    ///
    /// # Errors
    ///
    /// `InvalidInput` when `rgba` is not exactly `4 × width × height`
    /// bytes or the quantiser options are out of range.
    pub fn from_rgba8(
        width: u16,
        height: u16,
        rgba: &[u8],
        quantize: QuantizeOptions,
    ) -> Result<Self> {
        let (w, h) = (usize::from(width), usize::from(height));
        if rgba.len() != w * h * 4 {
            return Err(Error::InvalidInput(format!(
                "FrameSpec::from_rgba8: {width}x{height} needs {} RGBA bytes, got {}",
                w * h * 4,
                rgba.len()
            )));
        }
        let q = crate::quantize::quantize_rgba_with_options(rgba, w, h, quantize)?;
        Ok(Self {
            local_palette: Some(q.palette),
            graphic_control: Some(GraphicControl {
                transparent_index: q.transparent_index,
                ..GraphicControl::default()
            }),
            ..Self::new(width, height, q.indices)
        })
    }

    /// Place the frame at `(left, top)` on the Logical Screen.
    pub fn with_offset(mut self, left: u16, top: u16) -> Self {
        self.left = left;
        self.top = top;
        self
    }

    /// Give the frame its own Local Color Table (`Some`) or make it use
    /// the Global Color Table (`None`).
    pub fn with_local_palette(mut self, palette: impl Into<Option<Vec<Rgb>>>) -> Self {
        self.local_palette = palette.into();
        self
    }

    /// Set the Local Color Table's §20.c.viii Sort Flag.
    pub fn with_palette_sorted(mut self, sorted: bool) -> Self {
        self.palette_sorted = sorted;
        self
    }

    /// Set this frame's Interlace Flag (overriding the builder's).
    pub fn with_interlaced(mut self, interlaced: bool) -> Self {
        self.interlaced = Some(interlaced);
        self
    }

    /// Replace the whole Graphic Control Extension (`None` = write none).
    pub fn with_graphic_control(mut self, gce: impl Into<Option<GraphicControl>>) -> Self {
        self.graphic_control = gce.into();
        self
    }

    /// Write no Graphic Control Extension for this frame.
    pub fn without_graphic_control(self) -> Self {
        self.with_graphic_control(None)
    }

    fn gce(&mut self) -> &mut GraphicControl {
        self.graphic_control
            .get_or_insert_with(GraphicControl::default)
    }

    /// Set the §23.c.vii Delay Time in hundredths of a second (adds a
    /// Graphic Control Extension when the frame had none).
    pub fn with_delay_centis(mut self, delay_centis: u16) -> Self {
        self.gce().delay_centis = delay_centis;
        self
    }

    /// Set the Delay Time from a [`Duration`], rounded to the nearest
    /// hundredth of a second and saturating at 655.35 s.
    pub fn with_delay(self, delay: Duration) -> Self {
        let centis = u16::try_from((delay.as_millis() + 5) / 10).unwrap_or(u16::MAX);
        self.with_delay_centis(centis)
    }

    /// Set the §23.c.iv Disposal Method.
    pub fn with_disposal(mut self, disposal: DisposalMethod) -> Self {
        self.gce().disposal = disposal;
        self
    }

    /// Set (or clear) the §23.c.viii Transparency Index.
    pub fn with_transparent_index(mut self, index: impl Into<Option<u8>>) -> Self {
        self.gce().transparent_index = index.into();
        self
    }

    /// Set the §23.c.v User Input Flag.
    pub fn with_user_input(mut self, user_input: bool) -> Self {
        self.gce().user_input = user_input;
        self
    }
}

/// Fluent assembler for an animated [`GifFile`].
///
/// Construct with [`AnimationBuilder::new`], push frames with
/// [`add_full_frame`](Self::add_full_frame) (a frame that fills the
/// whole logical screen), [`add_placed_frame`](Self::add_placed_frame)
/// (a sub-rectangle) or [`add_frame`](Self::add_frame) (a [`FrameSpec`]
/// with every per-image field: Local Color Table, Transparency Index,
/// User Input Flag, Interlace Flag, or no Graphic Control Extension),
/// optionally pick the looping behaviour, header version, Pixel Aspect
/// Ratio and comments, then [`build`](Self::build) to get a validated
/// [`GifFile`] ready for [`crate::encode_file`].
///
/// [`add_full_frame`](Self::add_full_frame) /
/// [`add_placed_frame`](Self::add_placed_frame) frames use the Global
/// Color Table supplied at construction; [`global_palette`](Self::global_palette)
/// replaces or removes it (a stream without one needs a Local Color
/// Table on every frame).
#[derive(Debug, Clone)]
pub struct AnimationBuilder {
    screen_width: u16,
    screen_height: u16,
    palette: Option<Vec<Rgb>>,
    background_index: u8,
    pixel_aspect_ratio: u8,
    version: Option<Version>,
    comments: Vec<Vec<u8>>,
    loop_behaviour: LoopBehaviour,
    /// §20.c.vii Interlace Flag stamped onto frames added *after* the
    /// most recent [`interlaced`](Self::interlaced) call. Captured per
    /// frame at add time so a caller can mix interlaced and progressive
    /// frames in one stream (§20.c.vii is a per-frame flag).
    interlace: bool,
    frames: Vec<GifFrameData>,
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
            palette: Some(palette),
            background_index: 0,
            pixel_aspect_ratio: 0,
            version: None,
            comments: Vec::new(),
            loop_behaviour: LoopBehaviour::Once,
            interlace: false,
            frames: Vec::new(),
        }
    }

    /// Replace the §19 Global Color Table (`Some`) or write none
    /// (`None`; §18 Global Color Table Flag clear — every frame must
    /// then bring a Local Color Table). Frames already added are
    /// re-checked at [`build`](Self::build).
    pub fn global_palette(mut self, palette: impl Into<Option<Vec<Rgb>>>) -> Self {
        self.palette = palette.into();
        self
    }

    /// Set the §17.c.ii header version. Without this call the builder
    /// writes the earliest version covering every block (§7): `GIF89a`
    /// as soon as any frame carries a Graphic Control Extension or the
    /// stream carries a loop count or comment, `GIF87a` otherwise.
    /// [`build`](Self::build) refuses a `Gif87a` request for a stream
    /// that needs 89a.
    pub fn version(mut self, version: Version) -> Self {
        self.version = Some(version);
        self
    }

    /// Set the §18.c.viii Pixel Aspect Ratio raw byte (`0` = none; see
    /// [`GifFile::raw_pixel_aspect_ratio_for`]).
    pub fn pixel_aspect_ratio(mut self, raw: u8) -> Self {
        self.pixel_aspect_ratio = raw;
        self
    }

    /// Append a §24 Comment Extension, written after the looping block
    /// and before the first frame (§24.e.ii).
    pub fn comment(mut self, text: impl Into<Vec<u8>>) -> Self {
        self.comments.push(text.into());
        self
    }

    /// Set the total play count: `0` loops forever, `1` plays once (no
    /// NETSCAPE2.0 block), `n` repeats `n - 1` times after the first
    /// pass; values above 65 536 saturate there. See
    /// [`GifFile::set_iterations`].
    pub fn iterations(mut self, iterations: u32) -> Self {
        self.loop_behaviour = match crate::edit::loop_count_for_iterations(iterations) {
            None => LoopBehaviour::Once,
            Some(0) => LoopBehaviour::Forever,
            Some(n) => LoopBehaviour::Repeat(n),
        };
        self
    }

    /// Set the §20.c.vii Interlace Flag applied to every frame added
    /// *after* this call. Frames added while the flag is `on` are stored
    /// in Appendix E four-pass order by [`crate::encode_file`]; the composited
    /// output is unaffected (interlacing is a storage-order choice).
    ///
    /// The flag is captured per frame at add time, so toggling it between
    /// [`add_full_frame`](Self::add_full_frame) /
    /// [`add_placed_frame`](Self::add_placed_frame) calls produces a
    /// stream that mixes interlaced and progressive frames — §20.c.vii is
    /// a per-frame flag. Defaults to `false` (progressive). This is the
    /// fluent counterpart to [`crate::GifFile::set_frames_interlaced`],
    /// which stamps a whole already-built stream at once. A
    /// [`FrameSpec::interlaced`] value overrides it for that frame.
    pub fn interlaced(mut self, on: bool) -> Self {
        self.interlace = on;
        self
    }

    /// Set the §18.c.vii Background Color Index — the colour
    /// Restore-Background disposal clears a frame's rectangle to.
    pub fn background_index(mut self, index: u8) -> Self {
        self.background_index = index;
        self
    }

    /// Loop forever: emit a NETSCAPE2.0 *Looping* sub-block with count
    /// `0`. [`GifFile::loop_count`] reports `Some(0)` and
    /// [`GifFile::total_play_duration`] reports `None` (never
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
        self,
        left: u16,
        top: u16,
        width: u16,
        height: u16,
        indices: Vec<u8>,
        delay_centis: u16,
        disposal: DisposalMethod,
    ) -> Result<Self> {
        self.add_frame(
            FrameSpec::new(width, height, indices)
                .with_offset(left, top)
                .with_delay_centis(delay_centis)
                .with_disposal(disposal),
        )
    }

    /// Append a frame described by a [`FrameSpec`].
    ///
    /// Validates that the placement rectangle stays within the §18
    /// Logical Screen, `indices.len() == width × height`, the frame has
    /// a colour table (its Local Color Table, else the builder's Global
    /// Color Table, §21) of 1..=256 entries, and every index and the
    /// Transparency Index address an entry of it.
    pub fn add_frame(mut self, spec: FrameSpec) -> Result<Self> {
        self.validate_frame(&spec)?;
        let interlaced = spec.interlaced.unwrap_or(self.interlace);
        self.frames.push(GifFrameData {
            left: spec.left,
            top: spec.top,
            width: spec.width,
            height: spec.height,
            local_palette: spec.local_palette,
            palette_sorted: spec.palette_sorted,
            interlaced,
            indices: spec.indices,
            graphic_control: spec.graphic_control,
        });
        Ok(self)
    }

    fn validate_frame(&self, spec: &FrameSpec) -> Result<()> {
        let (left, top, width, height) = (spec.left, spec.top, spec.width, spec.height);
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
        if spec.indices.len() != expected {
            return Err(Error::InvalidInput(format!(
                "frame indices length {} != width*height {expected}",
                spec.indices.len()
            )));
        }
        if let Some(local) = &spec.local_palette
            && (local.is_empty() || local.len() > 256)
        {
            return Err(Error::InvalidInput(format!(
                "local colour table has {} entries; expected 1..=256",
                local.len()
            )));
        }
        check_indices(
            spec.local_palette.as_deref().or(self.palette.as_deref()),
            &spec.indices,
            spec.graphic_control.and_then(|g| g.transparent_index),
        )
    }

    /// Finalise the builder into a validated, animation-ready
    /// [`GifFile`].
    ///
    /// The version is the one set with [`version`](Self::version), else
    /// the earliest that covers every block (§7) — `GIF89a` whenever a
    /// frame carries a §23 GCE (every [`add_full_frame`](Self::add_full_frame) /
    /// [`add_placed_frame`](Self::add_placed_frame) frame does). When the
    /// loop behaviour is anything other than "play once" a NETSCAPE2.0
    /// *Looping* Application Extension is inserted ahead of the first
    /// frame, followed by the [`comment`](Self::comment)s.
    ///
    /// # Errors
    ///
    /// * `InvalidInput` when no frame was added (an empty animation is
    ///   not a valid Data Stream — §B requires at least the Trailer, but
    ///   a frameless "animation" is almost certainly a caller bug).
    /// * `InvalidInput` when the Global Color Table is empty or larger
    ///   than 256 entries (§19 colour-table size limits), or a frame
    ///   without a Local Color Table is left with no table at all.
    /// * `InvalidInput` when [`Version::Gif87a`] was requested for a
    ///   stream that needs 89a blocks.
    pub fn build(self) -> Result<GifFile> {
        if self.frames.is_empty() {
            return Err(Error::InvalidInput(
                "animation has no frames; add at least one before build()".into(),
            ));
        }
        if let Some(palette) = &self.palette {
            if palette.is_empty() {
                return Err(Error::InvalidInput(
                    "global colour table must contain at least one entry".into(),
                ));
            }
            if palette.len() > 256 {
                return Err(Error::InvalidInput(format!(
                    "global colour table has {} entries; max is 256",
                    palette.len()
                )));
            }
        }
        // `global_palette` may have changed after frames were added.
        for f in &self.frames {
            check_indices(
                f.local_palette.as_deref().or(self.palette.as_deref()),
                &f.indices,
                f.graphic_control.and_then(|g| g.transparent_index),
            )?;
        }

        let mut blocks: Vec<Block> = Vec::with_capacity(self.frames.len() + 1);

        // §26 — emit the NETSCAPE2.0 *Looping* block ahead of the first
        // frame for any non-"play once" behaviour. The de-facto reader
        // convention is that the block precedes the frames.
        let loop_count = match self.loop_behaviour {
            LoopBehaviour::Once => None,
            LoopBehaviour::Forever => Some(0),
            LoopBehaviour::Repeat(n) => Some(n),
        };
        if let Some(n) = loop_count {
            blocks.push(Block::Application(
                LoopControl {
                    loop_count: Some(n),
                    buffer_size: None,
                }
                .to_application(),
            ));
        }
        // §24.e.ii — comments at the beginning of the Data Stream.
        blocks.extend(self.comments.into_iter().map(Block::Comment));

        // §18.c.iv — Color Resolution describes the *original*
        // palette's bit depth; "bits to address the largest table minus
        // 1" is the honest value.
        let largest = self
            .frames
            .iter()
            .filter_map(|f| f.local_palette.as_ref().map(Vec::len))
            .chain(self.palette.as_ref().map(Vec::len))
            .max()
            .unwrap_or(1);
        for frame in self.frames {
            blocks.push(Block::Image(frame));
        }

        let mut file = GifFile {
            version: Version::Gif87a,
            screen_width: self.screen_width,
            screen_height: self.screen_height,
            color_resolution: color_resolution_for(largest),
            global_palette_sorted: false,
            background_index: self.background_index,
            pixel_aspect_ratio: self.pixel_aspect_ratio,
            global_palette: self.palette,
            blocks,
        };
        let required = file.required_version();
        file.version = match self.version {
            Some(v) if v < required => {
                return Err(Error::InvalidInput(format!(
                    "GIF87a requested but the stream needs GIF89a blocks (Graphic Control, \
                     comment or loop extension) — §7; got required {required:?}"
                )));
            }
            Some(v) => v,
            None => required,
        };
        Ok(file)
    }
}

/// §21 / §23.c.viii — every index (and the Transparency Index) must
/// address an entry of the frame's active colour table.
fn check_indices(table: Option<&[Rgb]>, indices: &[u8], transparent: Option<u8>) -> Result<()> {
    let table = table.ok_or_else(|| {
        Error::InvalidInput(
            "frame has no Local Color Table and the stream has no Global Color Table".into(),
        )
    })?;
    if let Some(&bad) = indices.iter().find(|&&i| (i as usize) >= table.len()) {
        return Err(Error::InvalidInput(format!(
            "pixel index {bad} >= palette size {}",
            table.len()
        )));
    }
    if let Some(t) = transparent
        && usize::from(t) >= table.len()
    {
        return Err(Error::InvalidInput(format!(
            "transparency index {t} >= palette size {}",
            table.len()
        )));
    }
    Ok(())
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

        let bytes = crate::encode_file(&img).unwrap();
        let decoded = crate::parse(&bytes).unwrap();
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

    /// `interlaced(true)` stamps the §20.c.vii flag onto frames added
    /// afterward; the built stream reports them via the interlace
    /// accessors and survives an encode → decode round-trip.
    #[test]
    fn interlaced_toggle_stamps_frames() {
        let img = AnimationBuilder::new(4, 4, pal2())
            .interlaced(true)
            .add_full_frame(vec![0; 16], 10, DisposalMethod::None)
            .unwrap()
            .add_full_frame(vec![1; 16], 10, DisposalMethod::None)
            .unwrap()
            .build()
            .unwrap();
        assert!(img.all_frames_interlaced());

        let decoded = crate::parse(&crate::encode_file(&img).unwrap()).unwrap();
        assert_eq!(decoded, img, "interlaced builder output round-trips");
    }

    /// The flag is captured per frame at add time, so toggling it between
    /// adds mixes interlaced and progressive frames in one stream.
    #[test]
    fn interlaced_toggle_is_per_frame() {
        let img = AnimationBuilder::new(2, 2, pal2())
            .interlaced(true)
            .add_full_frame(vec![0; 4], 0, DisposalMethod::None)
            .unwrap()
            .interlaced(false)
            .add_full_frame(vec![1; 4], 0, DisposalMethod::None)
            .unwrap()
            .build()
            .unwrap();
        let flags: Vec<bool> = img.frames().map(|f| f.interlaced).collect();
        assert_eq!(flags, vec![true, false]);
        assert!(img.has_interlaced_frames());
        assert!(!img.all_frames_interlaced());
    }

    /// The default is progressive (no Interlace Flag), preserving the
    /// historical builder behaviour.
    #[test]
    fn builder_default_is_progressive() {
        let img = AnimationBuilder::new(2, 2, pal2())
            .add_full_frame(vec![0; 4], 0, DisposalMethod::None)
            .unwrap()
            .build()
            .unwrap();
        assert!(!img.has_interlaced_frames());
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
