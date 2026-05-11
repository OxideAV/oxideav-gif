//! In-memory representation of a parsed or to-be-encoded GIF stream.
//!
//! Mirrors the GIF89a Grammar in Appendix B:
//! `<GIF Data Stream> ::= Header <Logical Screen> <Data>* Trailer`.

/// One RGB triplet from a colour table (§19, §21).
///
/// The spec lays palette entries out as Red-Green-Blue order
/// (Appendix D, "Color Order").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Versions enumerated by §17.c.ii.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    /// `GIF87a` — original CompuServe release, 1987.
    Gif87a,
    /// `GIF89a` — current revision, 1989.
    Gif89a,
}

impl Version {
    pub(crate) fn ascii(self) -> [u8; 3] {
        match self {
            Version::Gif87a => *b"87a",
            Version::Gif89a => *b"89a",
        }
    }
}

/// Disposal Method values from §23.c.iv.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisposalMethod {
    /// Value 0 — "No disposal specified."
    #[default]
    None,
    /// Value 1 — "Do not dispose."
    Keep,
    /// Value 2 — "Restore to background color."
    RestoreBackground,
    /// Value 3 — "Restore to previous."
    RestorePrevious,
}

impl DisposalMethod {
    pub(crate) fn from_bits(b: u8) -> Self {
        match b {
            1 => DisposalMethod::Keep,
            2 => DisposalMethod::RestoreBackground,
            3 => DisposalMethod::RestorePrevious,
            // Values 4-7 are reserved per §23.c.iv. Treat as "None" so
            // a malformed stream still decodes; encoders never produce
            // a reserved value.
            _ => DisposalMethod::None,
        }
    }

    pub(crate) fn as_bits(self) -> u8 {
        match self {
            DisposalMethod::None => 0,
            DisposalMethod::Keep => 1,
            DisposalMethod::RestoreBackground => 2,
            DisposalMethod::RestorePrevious => 3,
        }
    }
}

/// Graphic Control Extension (§23) parameters that modify the
/// graphic-rendering block immediately following the GCE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GraphicControl {
    pub disposal: DisposalMethod,
    /// Set when the GCE asks the decoder to wait for user input
    /// (§23.c.v).
    pub user_input: bool,
    /// Index into the active colour table that should be skipped
    /// during rendering (§23.c.viii). Present when `transparent_index`
    /// is `Some`.
    pub transparent_index: Option<u8>,
    /// Hundredths-of-a-second delay before processing continues
    /// (§23.c.vii).
    pub delay_centis: u16,
}

/// Plain Text Extension (§25) — textual data rendered in a grid of
/// monospaced cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlainText {
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
    pub cell_width: u8,
    pub cell_height: u8,
    pub fg_color_index: u8,
    pub bg_color_index: u8,
    /// Concatenation of all sub-block payloads (§25.c.xii).
    pub text: Vec<u8>,
}

/// Application Extension (§26).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Application {
    /// 8 printable ASCII bytes (§26.c.iv).
    pub identifier: [u8; 8],
    /// 3-byte authentication code (§26.c.v).
    pub auth_code: [u8; 3],
    /// Concatenation of all sub-block payloads.
    pub data: Vec<u8>,
}

/// One Image Descriptor (§20) plus its decoded pixel raster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
    /// `Some` when the image carried a Local Color Table (§21).
    pub local_palette: Option<Vec<Rgb>>,
    /// "Sort Flag" from the Image Descriptor packed byte (§20.c.viii).
    pub palette_sorted: bool,
    /// Decoder presents a de-interlaced, top-to-bottom, left-to-right
    /// raster regardless of the on-disk Interlace Flag; the original
    /// flag is preserved here so an encoder can round-trip it.
    pub interlaced: bool,
    /// One palette index per pixel, in row-major top-to-bottom order;
    /// length is `width * height`.
    pub indices: Vec<u8>,
    /// Optional Graphic Control Extension that immediately preceded
    /// this image (§23.d "scope is the graphic rendering block that
    /// follows it").
    pub graphic_control: Option<GraphicControl>,
}

/// One element of the `<Data>*` repetition in the §B grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Image(Frame),
    PlainText {
        params: PlainText,
        graphic_control: Option<GraphicControl>,
    },
    Comment(Vec<u8>),
    Application(Application),
}

/// Top-level result of a successful decode and the input shape an
/// encoder accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GifImage {
    pub version: Version,
    /// Logical Screen Descriptor §18 — width × height of the virtual
    /// screen on which all frames are composited.
    pub screen_width: u16,
    pub screen_height: u16,
    /// Color Resolution from §18.c.iv ("number of bits per primary
    /// color in the original image, minus 1"). Range 0..=7.
    pub color_resolution: u8,
    /// "Sort Flag" from §18.c.v.
    pub global_palette_sorted: bool,
    /// Background colour index — only meaningful when `global_palette`
    /// is `Some` (§18.c.vii).
    pub background_index: u8,
    /// "Pixel Aspect Ratio" raw byte from §18.c.viii. 0 means "no
    /// aspect ratio information given".
    pub pixel_aspect_ratio: u8,
    pub global_palette: Option<Vec<Rgb>>,
    pub blocks: Vec<Block>,
}

impl GifImage {
    /// Iterate the image-bearing blocks.
    pub fn frames(&self) -> impl Iterator<Item = &Frame> {
        self.blocks.iter().filter_map(|b| match b {
            Block::Image(f) => Some(f),
            _ => None,
        })
    }

    /// Iterate every Application Extension carried by this stream.
    pub fn application_extensions(&self) -> impl Iterator<Item = &Application> {
        self.blocks.iter().filter_map(|b| match b {
            Block::Application(a) => Some(a),
            _ => None,
        })
    }

    /// Animation loop count expressed by the NETSCAPE2.0 *Looping*
    /// sub-block (sub-block ID `0x01`), falling back to the legacy
    /// ANIMEXTS1.0 Application Extension when NETSCAPE2.0 is absent.
    ///
    /// * `None` — neither extension is present (or both are present but
    ///   neither carries a *Looping* sub-block). Per the de-facto
    ///   convention this means "play once".
    /// * `Some(0)` — loop forever.
    /// * `Some(N)` — play `N + 1` times in total (one initial pass
    ///   plus `N` repeats).
    ///
    /// The NETSCAPE2.0 byte layout is documented in
    /// `docs/image/gif/netscape2.0-loop-extension.md`. ANIMEXTS1.0
    /// reuses the same *Looping* sub-block layout under identifier
    /// `b"ANIMEXTS"` + auth code `b"1.0"`. The first matching block in
    /// source order wins if the stream carries more than one (which is
    /// non-portable and discouraged); NETSCAPE2.0 is preferred when both
    /// shapes are present, matching the cross-tool convention that
    /// NETSCAPE2.0 superseded ANIMEXTS1.0.
    pub fn loop_count(&self) -> Option<u16> {
        for app in self.application_extensions() {
            if let Some(lc) = crate::app_ext::LoopControl::from_application(app) {
                if let Some(n) = lc.loop_count {
                    return Some(n);
                }
            }
        }
        // Fall back to ANIMEXTS1.0 if NETSCAPE2.0 was absent or carried
        // no *Looping* sub-block.
        for app in self.application_extensions() {
            if let Some(lc) = crate::app_ext::AnimextsLoopControl::from_application(app) {
                if let Some(n) = lc.loop_count {
                    return Some(n);
                }
            }
        }
        None
    }

    /// Buffering hint from the NETSCAPE2.0 *Buffering* sub-block
    /// (sub-block ID `0x02`). Modern decoders treat this as
    /// discardable; surfaced so consumers can choose to honour or
    /// re-emit it.
    pub fn netscape_buffer_hint(&self) -> Option<u32> {
        for app in self.application_extensions() {
            if let Some(lc) = crate::app_ext::LoopControl::from_application(app) {
                if let Some(n) = lc.buffer_size {
                    return Some(n);
                }
            }
        }
        None
    }

    /// Raw XMP packet bytes from the `XMP Data` Application Extension
    /// (typically a UTF-8 RDF/XML envelope). The decoder does not
    /// parse the XML — callers can hand these bytes to an XMP library.
    pub fn xmp_packet(&self) -> Option<&[u8]> {
        self.application_extensions().find_map(|app| {
            if &app.identifier == crate::app_ext::XMP_IDENTIFIER
                && &app.auth_code == crate::app_ext::XMP_AUTH_CODE
            {
                Some(app.data.as_slice())
            } else {
                None
            }
        })
    }

    /// Raw ICC colour-profile bytes from the `ICCRGBG1` Application
    /// Extension. The decoder does not parse the profile — callers
    /// can hand these bytes to an ICC library.
    pub fn icc_profile(&self) -> Option<&[u8]> {
        self.application_extensions().find_map(|app| {
            if &app.identifier == crate::app_ext::ICC_IDENTIFIER
                && &app.auth_code == crate::app_ext::ICC_AUTH_CODE
            {
                Some(app.data.as_slice())
            } else {
                None
            }
        })
    }

    /// Hoist a shared per-frame Local Color Table to the §18 Global
    /// Color Table when every image frame carries the *same* palette
    /// (or no palette at all, falling back on an existing GCT). On a
    /// successful hoist:
    ///
    /// * Every matching `Frame::local_palette` is set to `None` so the
    ///   encoder writes the frame without an §21 Local Color Table
    ///   block (saves `3 × 2^(size_bits + 1)` bytes per frame).
    /// * The hoisted palette is installed as `self.global_palette`
    ///   when none was set.
    ///
    /// Returns `true` when a hoist actually happened, `false` when the
    /// invariants did not hold (mixed palettes, or frames with no
    /// palette and no GCT to fall back on). The transformation is
    /// idempotent and never invalidates a previously valid stream:
    /// frames that decode under the new GCT decode to the same pixels
    /// as they did under their LCT, because the LCTs were already
    /// equal.
    ///
    /// # Why this is safe
    ///
    /// §20.c "Local Color Table Flag" governs whether a frame ships
    /// its own table; with the flag clear the decoder uses the §18
    /// Global Color Table (§21 "Local Color Table" first paragraph).
    /// Replacing every identical LCT with one shared GCT therefore
    /// preserves the exact mapping from palette index to RGB triplet
    /// for every pixel.
    ///
    /// # Frames with differing palettes
    ///
    /// If any frame uses a different palette from the others, no
    /// hoist is performed — there is no single GCT that would let
    /// every frame omit its LCT.
    pub fn optimize_color_tables(&mut self) -> bool {
        // Phase 1 — collect every frame's effective palette (LCT if
        // present, else current GCT).
        let mut shared: Option<Vec<Rgb>> = self.global_palette.clone();
        let mut any_frame = false;
        for block in &self.blocks {
            if let Block::Image(frame) = block {
                any_frame = true;
                let effective = frame.local_palette.as_ref().or(shared.as_ref());
                let Some(eff) = effective else {
                    // Frame has no LCT and we have nothing to fall back
                    // on. Cannot hoist.
                    return false;
                };
                match &shared {
                    None => shared = Some(eff.clone()),
                    Some(s) if s == eff => {}
                    Some(_) => return false,
                }
            }
        }
        if !any_frame {
            // No image frames → nothing to hoist into / out of.
            return false;
        }
        let palette = match shared {
            Some(p) => p,
            None => return false,
        };
        // Phase 2 — install the GCT and clear identical LCTs.
        self.global_palette = Some(palette.clone());
        for block in &mut self.blocks {
            if let Block::Image(frame) = block {
                if frame.local_palette.as_ref() == Some(&palette) {
                    frame.local_palette = None;
                }
            }
        }
        true
    }

    /// Raw TIFF EXIF bytes from the `Exif    ` Application Extension
    /// (typically beginning with `b"II*\0"` little-endian or `b"MM\0*"`
    /// big-endian per TIFF 6.0 §2). The decoder does not parse the TIFF
    /// tag tree — callers can hand these bytes to a TIFF/EXIF library.
    /// The 3-byte authentication code that follows the identifier in
    /// the §26 wire layout is not checked here (real producers vary the
    /// last two bytes); use [`crate::app_ext::ExifMetadata::from_application`]
    /// when the auth code matters for round-tripping.
    pub fn exif(&self) -> Option<&[u8]> {
        self.application_extensions().find_map(|app| {
            if &app.identifier == crate::app_ext::EXIF_IDENTIFIER {
                Some(app.data.as_slice())
            } else {
                None
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_ext::ExifMetadata;

    fn pal3() -> Vec<Rgb> {
        vec![Rgb::new(1, 1, 1), Rgb::new(2, 2, 2), Rgb::new(3, 3, 3)]
    }

    fn pal3_alt() -> Vec<Rgb> {
        vec![Rgb::new(9, 9, 9), Rgb::new(8, 8, 8), Rgb::new(7, 7, 7)]
    }

    fn frame_with(local: Option<Vec<Rgb>>) -> Frame {
        Frame {
            left: 0,
            top: 0,
            width: 1,
            height: 1,
            local_palette: local,
            palette_sorted: false,
            interlaced: false,
            indices: vec![0],
            graphic_control: None,
        }
    }

    fn base_image(global: Option<Vec<Rgb>>, blocks: Vec<Block>) -> GifImage {
        GifImage {
            version: Version::Gif89a,
            screen_width: 1,
            screen_height: 1,
            color_resolution: 1,
            global_palette_sorted: false,
            background_index: 0,
            pixel_aspect_ratio: 0,
            global_palette: global,
            blocks,
        }
    }

    /// All frames carry the same LCT and there is no GCT — the LCT
    /// should hoist into a fresh GCT and every frame's LCT field
    /// should clear.
    #[test]
    fn optimize_hoists_shared_lct_into_new_gct() {
        let mut img = base_image(
            None,
            vec![
                Block::Image(frame_with(Some(pal3()))),
                Block::Image(frame_with(Some(pal3()))),
            ],
        );
        assert!(img.optimize_color_tables());
        assert_eq!(img.global_palette.as_deref(), Some(pal3().as_slice()));
        for f in img.frames() {
            assert!(f.local_palette.is_none(), "LCT should have been cleared");
        }
    }

    /// Mixed palettes — the optimisation must refuse and leave every
    /// frame's LCT intact.
    #[test]
    fn optimize_refuses_when_palettes_differ() {
        let mut img = base_image(
            None,
            vec![
                Block::Image(frame_with(Some(pal3()))),
                Block::Image(frame_with(Some(pal3_alt()))),
            ],
        );
        assert!(!img.optimize_color_tables());
        assert!(img.global_palette.is_none());
        let kept: Vec<_> = img.frames().map(|f| f.local_palette.clone()).collect();
        assert_eq!(kept, vec![Some(pal3()), Some(pal3_alt())]);
    }

    /// A frame with no LCT and no GCT cannot be hoisted — the
    /// optimisation must refuse.
    #[test]
    fn optimize_refuses_when_frame_has_no_palette_at_all() {
        let mut img = base_image(None, vec![Block::Image(frame_with(None))]);
        assert!(!img.optimize_color_tables());
    }

    /// LCTs that already match an existing GCT should clear without
    /// changing the GCT.
    #[test]
    fn optimize_clears_redundant_lcts_against_existing_gct() {
        let mut img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(Some(pal3()))),
                Block::Image(frame_with(None)),
            ],
        );
        assert!(img.optimize_color_tables());
        assert_eq!(img.global_palette.as_deref(), Some(pal3().as_slice()));
        for f in img.frames() {
            assert!(f.local_palette.is_none());
        }
    }

    /// Idempotent: calling twice does not change the second result.
    #[test]
    fn optimize_is_idempotent() {
        let mut img = base_image(
            None,
            vec![
                Block::Image(frame_with(Some(pal3()))),
                Block::Image(frame_with(Some(pal3()))),
            ],
        );
        assert!(img.optimize_color_tables());
        let snapshot = img.clone();
        assert!(img.optimize_color_tables());
        assert_eq!(img, snapshot);
    }

    /// `exif()` accessor surfaces the raw TIFF blob from a typed
    /// EXIF Application Extension regardless of the surrounding
    /// block layout.
    #[test]
    fn exif_accessor_finds_raw_blob() {
        let exif = ExifMetadata::new(b"II*\0\x08\x00\x00\x00".to_vec());
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(b"hi".to_vec()),
                Block::Application(exif.to_application()),
                Block::Image(frame_with(Some(pal3()))),
            ],
        );
        assert_eq!(img.exif(), Some(&b"II*\0\x08\x00\x00\x00"[..]));
    }

    /// `exif()` returns `None` when no EXIF block is present, even if
    /// other Application Extensions are.
    #[test]
    fn exif_accessor_returns_none_when_absent() {
        let xmp = crate::app_ext::XmpPacket {
            bytes: b"<x:xmpmeta/>".to_vec(),
        };
        let img = base_image(Some(pal3()), vec![Block::Application(xmp.to_application())]);
        assert!(img.exif().is_none());
    }
}
