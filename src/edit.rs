//! In-place editing of a [`GifFile`]'s stream-level structure: the
//! NETSCAPE2.0 looping declaration and the §24 Comment Extensions.
//!
//! Every other container field (§17 version, §18 Logical Screen size,
//! Background Color Index, Pixel Aspect Ratio, Global Color Table, each
//! image's §20 placement / interlace / §21 Local Color Table and §23
//! Graphic Control) is a plain public field of [`GifFile`] /
//! [`crate::GifFrameData`]. The two items here live in the block list,
//! so they get helpers that keep it consistent.

use crate::app_ext::{AnimextsLoopControl, LoopControl};
use crate::error::{Error, Result};
use crate::image::{Block, GifFile};

/// Largest play count a NETSCAPE2.0 *Looping* sub-block can express:
/// one pass plus 65 535 repeats.
pub(crate) const MAX_ITERATIONS: u32 = u16::MAX as u32 + 1;

/// NETSCAPE2.0 loop count for a total play count (`0` = forever,
/// `1` = one pass, i.e. no extension, `n` = `n - 1` repeats), saturating
/// at [`MAX_ITERATIONS`].
pub(crate) fn loop_count_for_iterations(iterations: u32) -> Option<u16> {
    match iterations {
        0 => Some(0),
        1 => None,
        n => Some((n.min(MAX_ITERATIONS) - 1) as u16),
    }
}

impl GifFile {
    /// Total number of passes the animation plays: `0` = forever, `1` =
    /// once (no looping declaration), `n` = the NETSCAPE2.0 / ANIMEXTS1.0
    /// loop count `n - 1` plus the initial pass
    /// (`docs/image/gif/netscape2.0-loop-extension.md`, "Loop-count
    /// semantics"). The inverse of [`Self::set_iterations`]; see
    /// [`Self::loop_count`] for the raw field.
    pub fn iterations(&self) -> u32 {
        match self.loop_count() {
            None => 1,
            Some(0) => 0,
            Some(n) => u32::from(n) + 1,
        }
    }

    /// Set the total number of passes: `0` loops forever (loop count
    /// `0`), `1` plays once and **removes** the looping declaration (a
    /// stream without one plays a single pass), `n ≥ 2` writes loop count
    /// `n - 1`. This is the "iterations" convention where `1` means no
    /// extension at all.
    ///
    /// # Errors
    ///
    /// `InvalidInput` when `iterations` exceeds 65 536 (one pass plus the
    /// 16-bit maximum of 65 535 repeats).
    pub fn set_iterations(&mut self, iterations: u32) -> Result<()> {
        if iterations > MAX_ITERATIONS {
            return Err(Error::InvalidInput(format!(
                "GIF iterations {iterations} exceed {MAX_ITERATIONS} (one pass plus the \
                 16-bit NETSCAPE2.0 maximum of 65535 repeats)"
            )));
        }
        self.set_loop_count(loop_count_for_iterations(iterations));
        Ok(())
    }

    /// Set the raw NETSCAPE2.0 *Looping* sub-block loop count: `Some(0)`
    /// loops forever, `Some(n)` repeats `n` times after the first pass,
    /// `None` removes the declaration (play once).
    ///
    /// With `Some`, the first NETSCAPE2.0 block is rewritten in place
    /// (its *Buffering* sub-block, if any, is kept) or, when there is
    /// none, a new one is inserted at the head of the block list —
    /// between the Global Color Table and the first frame, the position
    /// the loop-extension note calls portable. With `None`, the
    /// *Looping* sub-block is dropped from every NETSCAPE2.0 block (a
    /// block left with nothing in it is removed). Either way every
    /// ANIMEXTS1.0 block carrying a loop count is removed, so
    /// [`Self::loop_count`] reports exactly what was set.
    ///
    /// Setting a loop count on a [`crate::Version::Gif87a`] stream raises it
    /// to `Gif89a` (§26 Application Extensions require 89a, §7).
    pub fn set_loop_count(&mut self, loop_count: Option<u16>) {
        self.blocks.retain(|b| match b {
            Block::Application(app) => {
                AnimextsLoopControl::from_application(app).is_none_or(|lc| lc.loop_count.is_none())
            }
            _ => true,
        });
        match loop_count {
            Some(n) => {
                let existing = self.blocks.iter_mut().find_map(|b| match b {
                    Block::Application(app) => {
                        LoopControl::from_application(app).map(|lc| (app, lc))
                    }
                    _ => None,
                });
                match existing {
                    Some((app, lc)) => {
                        *app = LoopControl {
                            loop_count: Some(n),
                            buffer_size: lc.buffer_size,
                        }
                        .to_application();
                    }
                    None => self.blocks.insert(
                        0,
                        Block::Application(
                            LoopControl {
                                loop_count: Some(n),
                                buffer_size: None,
                            }
                            .to_application(),
                        ),
                    ),
                }
                self.upgrade_version_if_needed();
            }
            None => {
                self.blocks.retain_mut(|b| match b {
                    Block::Application(app) => match LoopControl::from_application(app) {
                        Some(lc) if lc.loop_count.is_some() => {
                            if lc.buffer_size.is_none() {
                                return false;
                            }
                            *app = LoopControl {
                                loop_count: None,
                                buffer_size: lc.buffer_size,
                            }
                            .to_application();
                            true
                        }
                        _ => true,
                    },
                    _ => true,
                });
            }
        }
    }

    /// Append a §24 Comment Extension to the leading run of the stream:
    /// after every block that precedes the first graphic-rendering block
    /// (§20 image or §25 Plain Text), i.e. after earlier comments and the
    /// looping / metadata Application Extensions — the "beginning of the
    /// Data Stream" position §24.e.ii recommends. A stream without any
    /// graphic-rendering block gets it at the end of its block list.
    ///
    /// `text` is written as-is (§24.e.i recommends 7-bit ASCII; see
    /// [`Self::comments_are_7bit_ascii`]); the encoder splits it into
    /// §15 Data Sub-blocks. Raises a `Gif87a` stream to `Gif89a` (§24
    /// requires 89a).
    pub fn add_comment(&mut self, text: impl Into<Vec<u8>>) {
        let at = self
            .blocks
            .iter()
            .position(Block::is_graphic_rendering)
            .unwrap_or(self.blocks.len());
        self.blocks.insert(at, Block::Comment(text.into()));
        self.upgrade_version_if_needed();
    }

    /// Remove every §24 Comment Extension; returns how many were removed.
    /// The declared version is left as it is (§7 never forces a
    /// downgrade).
    pub fn clear_comments(&mut self) -> usize {
        let before = self.blocks.len();
        self.blocks.retain(|b| !matches!(b, Block::Comment(_)));
        before - self.blocks.len()
    }

    /// Replace every §24 Comment Extension with `comments`, in order, at
    /// the [`Self::add_comment`] position.
    pub fn set_comments<I, C>(&mut self, comments: I)
    where
        I: IntoIterator<Item = C>,
        C: Into<Vec<u8>>,
    {
        self.clear_comments();
        for c in comments {
            self.add_comment(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::image::{Application, Block, GifFile, GifFrameData, Rgb, Version};

    fn still() -> GifFile {
        GifFile {
            version: Version::Gif87a,
            screen_width: 1,
            screen_height: 1,
            color_resolution: 0,
            global_palette_sorted: false,
            background_index: 0,
            pixel_aspect_ratio: 0,
            global_palette: Some(vec![Rgb::new(0, 0, 0), Rgb::new(255, 255, 255)]),
            blocks: vec![Block::Image(GifFrameData {
                left: 0,
                top: 0,
                width: 1,
                height: 1,
                local_palette: None,
                palette_sorted: false,
                interlaced: false,
                indices: vec![1],
                graphic_control: None,
            })],
        }
    }

    #[test]
    fn iterations_map_onto_the_loop_count() {
        let mut f = still();
        assert_eq!(f.iterations(), 1);
        f.set_iterations(0).unwrap();
        assert_eq!((f.loop_count(), f.iterations()), (Some(0), 0));
        assert_eq!(f.version, Version::Gif89a);
        f.set_iterations(3).unwrap();
        assert_eq!((f.loop_count(), f.iterations()), (Some(2), 3));
        // Rewritten in place, never duplicated.
        assert_eq!(f.application_extensions().count(), 1);
        f.set_iterations(1).unwrap();
        assert_eq!((f.loop_count(), f.iterations()), (None, 1));
        assert_eq!(f.application_extensions().count(), 0);
        f.set_iterations(65_536).unwrap();
        assert_eq!(f.loop_count(), Some(u16::MAX));
        assert!(f.set_iterations(65_537).is_err());
    }

    #[test]
    fn clearing_keeps_the_buffering_sub_block_and_drops_animexts() {
        let mut f = still();
        f.version = Version::Gif89a;
        f.blocks.insert(
            0,
            Block::Application(
                crate::app_ext::LoopControl {
                    loop_count: Some(5),
                    buffer_size: Some(4096),
                }
                .to_application(),
            ),
        );
        f.blocks.insert(
            0,
            Block::Application(Application {
                identifier: *b"ANIMEXTS",
                auth_code: *b"1.0",
                data: vec![1, 7, 0],
            }),
        );
        f.set_loop_count(Some(0));
        assert_eq!(f.loop_count(), Some(0));
        assert_eq!(f.netscape_buffer_hint(), Some(4096));
        assert_eq!(f.application_extensions().count(), 1);
        f.set_loop_count(None);
        assert_eq!(f.loop_count(), None);
        assert_eq!(f.netscape_buffer_hint(), Some(4096));
    }

    #[test]
    fn comments_go_to_the_leading_run() {
        let mut f = still();
        f.set_loop_count(Some(0));
        f.add_comment("first");
        f.add_comment(b"second".to_vec());
        assert!(matches!(f.blocks[0], Block::Application(_)));
        assert_eq!(
            f.comments().collect::<Vec<_>>(),
            vec![&b"first"[..], &b"second"[..]]
        );
        assert!(matches!(f.blocks[3], Block::Image(_)));
        assert!(f.comments_in_recommended_position());
        f.set_comments(["only"]);
        assert_eq!(f.comments().collect::<Vec<_>>(), vec![&b"only"[..]]);
        assert_eq!(f.clear_comments(), 1);
        assert_eq!(f.version, Version::Gif89a);
    }

    #[test]
    fn comment_on_87a_upgrades_and_round_trips() {
        let mut f = still();
        f.add_comment("hello");
        assert_eq!(f.version, Version::Gif89a);
        let bytes = f.to_bytes().unwrap();
        let back = GifFile::parse(&bytes).unwrap();
        assert_eq!(back.comments().collect::<Vec<_>>(), vec![&b"hello"[..]]);
    }
}
