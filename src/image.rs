//! In-memory representation of a parsed or to-be-encoded GIF stream.
//!
//! Mirrors the GIF89a Grammar in Appendix B:
//! `<GIF Data Stream> ::= Header <Logical Screen> <Data>* Trailer`.

use core::time::Duration;

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

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// `Gif87a < Gif89a` so `version.max(required)` selects the later
/// version when comparing what an encoder needs against what the input
/// declared. §7 ("An encoder should use the earliest possible version
/// number that includes all the blocks used in the Data Stream.")
impl Ord for Version {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        fn rank(v: Version) -> u8 {
            match v {
                Version::Gif87a => 0,
                Version::Gif89a => 1,
            }
        }
        rank(*self).cmp(&rank(*other))
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

impl GraphicControl {
    /// `true` when this Graphic Control Extension asks the decoder to
    /// **block indefinitely** on user input — the §23.e.ii corner where
    /// the §23.c.v User Input Flag is set *and* no §23.c.vii Delay Time
    /// is specified (`delay_centis == 0`).
    ///
    /// §23.e.ii: "In the absence of a specified Delay Time, the decoder
    /// should wait for user input indefinitely." This is the case a
    /// purely time-driven playback loop cannot serve: there is no timeout
    /// to fall back on, so the frame holds until the application supplies
    /// the §23.c.v "Carriage Return, Mouse Button Click, etc." input.
    ///
    /// Distinct from a GCE that pairs the User Input Flag *with* a
    /// non-zero Delay Time — §23.c.vii says processing then continues
    /// "when user input is received or when the delay time expires,
    /// whichever occurs first", so that frame is bounded and this returns
    /// `false`. Also `false` when the User Input Flag is clear regardless
    /// of the Delay Time.
    pub fn waits_for_user_input_indefinitely(&self) -> bool {
        self.user_input && self.delay_centis == 0
    }
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

impl Block {
    /// Minimum GIF version that this block requires under the
    /// CompuServe spec's "Required Version" entries:
    ///
    /// * §20 Image Descriptor — 87a (also covers §21 Local Color Table
    ///   and §22 Table-Based Image Data, which travel with it).
    /// * §23 Graphic Control Extension — 89a. When attached to an
    ///   [`Block::Image`] or [`Block::PlainText`] this also lifts the
    ///   containing block's required version to 89a.
    /// * §24 Comment Extension — 89a.
    /// * §25 Plain Text Extension — 89a.
    /// * §26 Application Extension — 89a.
    pub fn required_version(&self) -> Version {
        match self {
            Block::Image(frame) => {
                if frame.graphic_control.is_some() {
                    Version::Gif89a
                } else {
                    Version::Gif87a
                }
            }
            // PlainText / Comment / Application are all §23.5–§26 89a
            // additions; any GCE attached to a PlainText is by definition
            // also 89a, so the GCE check is redundant here.
            Block::PlainText { .. } | Block::Comment(_) | Block::Application(_) => Version::Gif89a,
        }
    }

    /// §12 "Blocks, Extensions and Scope" classification for this block.
    ///
    /// The spec groups every block into one of three classes and
    /// assigns each a label-byte range so that "decoders can handle
    /// block scope by appropriately identifying block labels, even when
    /// the block itself cannot be processed":
    ///
    /// * **Graphic-Rendering** — labels `0x00..=0x7F` excluding the
    ///   §27 Trailer `0x3B`. §12 names "the Image Descriptor and the
    ///   Plain Text Extension"; their on-wire labels are the §20.c.i
    ///   Image Separator `0x2C` and the §25.c.ii Plain Text Label
    ///   `0x01`, both inside `0x00..=0x7F`.
    /// * **Control** — labels `0x80..=0xF9`. §12 names "the Header, the
    ///   Logical Screen Descriptor, the Graphic Control Extension and
    ///   the Trailer"; of the variants this enum models, none is a
    ///   Control block (the §23 Graphic Control Extension is stored
    ///   *attached* to the graphic-rendering block it scopes, not as a
    ///   free-standing [`Block`], and the Header / LSD / Trailer are
    ///   structural fields of [`GifImage`], not list entries).
    /// * **Special-Purpose** — labels `0xFA..=0xFF`. §12 names "the
    ///   Comment Extension and the Application Extension"; their labels
    ///   are the §24.c.ii Comment Label `0xFE` and the §26.c.ii
    ///   Application Extension Label `0xFF`.
    ///
    /// §12: "Special Purpose blocks do not delimit the scope of any
    /// Control blocks; Special Purpose blocks are transparent to the
    /// decoding process." A renderer can therefore skip every
    /// [`BlockClass::SpecialPurpose`] block without affecting how the
    /// §23 Graphic Control Extension scopes the graphic-rendering
    /// blocks around it.
    pub fn class(&self) -> BlockClass {
        match self {
            Block::Image(_) | Block::PlainText { .. } => BlockClass::GraphicRendering,
            Block::Comment(_) | Block::Application(_) => BlockClass::SpecialPurpose,
        }
    }

    /// `true` when this block is a §12 Graphic-Rendering block (a §20
    /// Image or a §25 Plain Text Extension) — one that "contains
    /// information and data used to render a graphic on the display
    /// device".
    pub fn is_graphic_rendering(&self) -> bool {
        self.class() == BlockClass::GraphicRendering
    }

    /// `true` when this block is a §12 Special-Purpose block (a §24
    /// Comment or a §26 Application Extension) — one that is "neither
    /// used to control the process of the Data Stream nor [does it]
    /// contain information or data used to render a graphic", and so is
    /// "transparent to the decoding process".
    pub fn is_special_purpose(&self) -> bool {
        self.class() == BlockClass::SpecialPurpose
    }
}

/// §12 "Blocks, Extensions and Scope" block class.
///
/// GIF89a §12 partitions every block into three groups by purpose and
/// label-byte range. The third group — Control — covers the Header,
/// Logical Screen Descriptor, §23 Graphic Control Extension and §27
/// Trailer; in this crate those are structural fields of [`GifImage`]
/// or attached to the graphic-rendering block they scope, so a
/// free-standing [`Block`] is never Control. The variant is still
/// modelled for completeness and forward-compatibility with the §12
/// taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockClass {
    /// §12 Graphic-Rendering: §20 Image Descriptor, §25 Plain Text
    /// Extension. Labels `0x00..=0x7F` (excluding §27 Trailer `0x3B`).
    GraphicRendering,
    /// §12 Control: Header, §18 Logical Screen Descriptor, §23 Graphic
    /// Control Extension, §27 Trailer. Labels `0x80..=0xF9`.
    Control,
    /// §12 Special-Purpose: §24 Comment Extension, §26 Application
    /// Extension. Labels `0xFA..=0xFF`. "Transparent to the decoding
    /// process."
    SpecialPurpose,
}

impl Frame {
    /// §20.c.ix "Size of Local Color Table" — the 3-bit encoded field
    /// value that would be written for this frame's Local Color Table
    /// (`0..=7`), or `None` when no LCT is attached.
    ///
    /// Per §20.c.ix the field stores the smallest `N` in `0..=7` such
    /// that `2^(N+1)` is greater than or equal to the LCT entry count;
    /// the §21 LCT then carries `3 × 2^(N+1)` bytes on disk. A 2-entry
    /// LCT encodes as `0`; a 256-entry LCT encodes as `7`; mid-range
    /// counts round up — a 5-entry LCT rounds up to the 8-entry slot and
    /// encodes as `2`.
    ///
    /// Returns `None` when [`Frame::local_palette`] is `None` — per
    /// §20.c.ix "This value should be 0 if there is no Local Color
    /// Table specified", the field is undefined and the encoded `0`
    /// would collide with the "2-entry LCT" case, so the typed
    /// accessor surfaces the absent-LCT case as `None` instead.
    ///
    /// A `Some` result is always paired with a `Some(palette)` whose
    /// `len()` is `1..=256` (the encoder rejects empty or oversized
    /// palettes per §20.c.ix's `1..=256` range); a stream that round-
    /// trips through this crate's decoder always satisfies that bound.
    pub fn local_color_table_size_field(&self) -> Option<u8> {
        let len = self.local_palette.as_ref()?.len();
        // Smallest k in 0..=7 with 2^(k+1) >= len. Matches the §18.c.vi
        // / §20.c.ix encoder rule in `encoder::size_bits_for_palette`.
        // An empty LCT cannot be encoded (the encoder rejects it before
        // reaching the field-value step); for the lenient/decoded
        // shape that pre-validation guarantees, treat 0 as "round up to
        // the 2-entry slot, field value 0" rather than panicking.
        if len == 0 {
            return Some(0);
        }
        for k in 0u8..=7 {
            if (1usize << (k as u32 + 1)) >= len {
                return Some(k);
            }
        }
        // len > 256 is rejected by the encoder; clamp to the field's
        // maximum representable value for a defensive read-side path.
        Some(7)
    }

    /// §20.c.ix / §21.a on-disk entry count for this frame's Local
    /// Color Table: `2^(N+1)` where `N` is the
    /// [`Self::local_color_table_size_field`] value, or `None` when no
    /// LCT is attached.
    ///
    /// Range `2..=256` for any attached LCT (the §20.c.ix field is
    /// 3 bits and the LCT carries at least one R,G,B triplet, so the
    /// smallest representable on-disk LCT holds two entries). The
    /// returned count is the *power-of-two-rounded* on-disk shape, not
    /// `self.local_palette.as_ref().unwrap().len()`: the encoder zero-
    /// pads the tail when the in-memory palette is shorter than the
    /// rounded slot, since §21's table syntax leaves no representation
    /// for the in-between counts.
    ///
    /// A caller comparing the rounded count to the in-memory palette
    /// length can detect the padding window —
    /// `entry_count - palette.len()` is the number of trailing pad
    /// entries the encoder writes.
    pub fn local_color_table_entry_count(&self) -> Option<u32> {
        let field = self.local_color_table_size_field()?;
        Some(1u32 << (field as u32 + 1))
    }
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

    /// Iterate every image-bearing block paired with the colour table the
    /// decoder should render it against, in source order.
    ///
    /// Per §21.a "If present, this color table temporarily becomes the
    /// active color table and the following image should be processed
    /// using it", a Local Color Table on the §20 Image Descriptor takes
    /// precedence over the §18 Global Color Table; when the LCT flag is
    /// clear the GCT applies; when neither is present the second item of
    /// the yielded tuple is `None` (the §13 / §21 fallback "a Data Stream
    /// which does not contain either a Global Color Table or a Local
    /// Color Table" case).
    ///
    /// The yielded slice borrows from `self`, so callers iterating
    /// frames + palette together do not need to clone the palette or
    /// resolve precedence themselves.
    pub fn frames_with_palette(&self) -> impl Iterator<Item = (&Frame, Option<&[Rgb]>)> {
        let global = self.global_palette.as_deref();
        self.frames().map(move |f| {
            let palette = f.local_palette.as_deref().or(global);
            (f, palette)
        })
    }

    /// Iterate every §20 Image Descriptor block paired with its
    /// attached §23 Graphic Control Extension, in source order.
    ///
    /// Per §23.a "The scope of this extension is the first graphic
    /// rendering block to follow" and §23.d "This block can modify
    /// the Image Descriptor Block and the Plain Text Extension", a
    /// §23 GCE attaches to the immediately-following graphic-rendering
    /// block. On decode that attachment is stored on
    /// [`Frame::graphic_control`] (and likewise on
    /// [`Block::PlainText::graphic_control`] for §25 Plain Text); this
    /// accessor surfaces the §20 half of that pairing — `(&Frame,
    /// Option<GraphicControl>)` — in source order so a caller walking
    /// "every image and the GCE that controls it" can do so without
    /// re-deriving the relationship from [`Self::blocks`].
    ///
    /// The second tuple element is `None` for an image with no
    /// attached GCE (every still GIF87a, and 89a streams whose §20
    /// Image was not preceded by a §23 GCE per §23's "at most one
    /// Graphic Control Extension may precede a graphic rendering
    /// block").
    ///
    /// §25 Plain Text Extensions are not included — they are
    /// graphic-rendering blocks under §23.d but not §20 Images.
    /// Callers walking the timing or rendering-flag spine across
    /// both kinds use [`Self::frame_delays`] /
    /// [`Self::has_transparency`] / [`Self::requires_user_input`],
    /// which already cover both. The pairing here mirrors
    /// [`Self::frames_with_palette`]'s §20-only shape so the two
    /// accessors compose naturally.
    pub fn frames_with_graphic_control(
        &self,
    ) -> impl Iterator<Item = (&Frame, Option<GraphicControl>)> {
        self.frames().map(|f| (f, f.graphic_control))
    }

    /// `true` when the stream carries a §18 Global Color Table whose
    /// §18.c.v Sort Flag is set ("Ordered by decreasing importance,
    /// most important color first").
    ///
    /// Per §18.c.v the sort flag exists "to assist a decoder, with
    /// fewer available colors, in choosing the best subset of colors;
    /// the decoder may use an initial segment of the table to render
    /// the graphic." A display constrained to fewer than
    /// [`Self::global_palette`]`.len()` entries can therefore truncate
    /// the table to its leading N entries when this query is `true`,
    /// confident the higher-importance colours are preserved.
    ///
    /// Returns `false` for a stream with no Global Color Table (the
    /// §18.c.v Sort Flag is undefined when §18.c.iii Global Color
    /// Table Flag is clear) and for one that carries a Global Color
    /// Table whose Sort Flag is clear.
    pub fn has_sorted_global_palette(&self) -> bool {
        self.global_palette.is_some() && self.global_palette_sorted
    }

    /// Iterate every §20 Image Descriptor block paired with the
    /// colour table that would render it ([`Self::frames_with_palette`]'s
    /// precedence — Local Color Table when present per §21.a, Global
    /// Color Table when the LCT flag is clear, `None` when neither
    /// table is attached) plus a `bool` flag reporting whether that
    /// active table is *sorted* per §18.c.v (for the Global Color
    /// Table) or §20.c.viii (for the Local Color Table).
    ///
    /// Both spec sections define the Sort Flag identically: "Ordered
    /// by decreasing importance, most important color first." A
    /// palette-display-constrained renderer that walks frame-by-frame
    /// can use this iterator's `bool` to decide, per frame, whether
    /// truncating the active palette to a leading initial segment is
    /// safe (Sort Flag set) or whether a full-table quantiser pass is
    /// needed (Sort Flag clear).
    ///
    /// The `bool` is `false` when there is no active table at all
    /// (the §13 / §21 fallback "a Data Stream which does not contain
    /// either a Global Color Table or a Local Color Table" case) —
    /// no table means no sorted-order guarantee is available.
    ///
    /// The yielded slice borrows from `self`, so callers iterating
    /// frames + palette + sort flag together do not need to clone the
    /// palette or hand-roll the precedence + Sort Flag lookup.
    pub fn frames_with_sorted_palette(
        &self,
    ) -> impl Iterator<Item = (&Frame, Option<&[Rgb]>, bool)> {
        let global = self.global_palette.as_deref();
        let global_sorted = self.global_palette_sorted;
        self.frames().map(move |f| {
            if let Some(local) = f.local_palette.as_deref() {
                // §21.a "this color table temporarily becomes the
                // active color table" — the §20.c.viii LCT Sort Flag
                // takes precedence over the §18.c.v GCT Sort Flag
                // exactly the way the LCT itself does over the GCT.
                (f, Some(local), f.palette_sorted)
            } else if let Some(g) = global {
                (f, Some(g), global_sorted)
            } else {
                // §13 / §21 fallback — no active table, so no sort
                // guarantee.
                (f, None, false)
            }
        })
    }

    /// `true` when every §20 Image Descriptor block in the stream
    /// would render against a colour table whose Sort Flag is set
    /// (LCT-sorted per §20.c.viii when an LCT is attached, otherwise
    /// GCT-sorted per §18.c.v).
    ///
    /// Equivalent to "every frame's active palette is sorted, by the
    /// same precedence [`Self::frames_with_sorted_palette`] applies."
    /// Returns `true` for a zero-frame stream (vacuously) and `false`
    /// for any frame whose active palette is missing (no LCT and no
    /// GCT) — that frame has no sort guarantee available to honour.
    ///
    /// A palette-display-constrained pipeline can gate
    /// initial-segment-truncation on this single query rather than
    /// inspecting [`Self::frames_with_sorted_palette`] per frame.
    pub fn all_frames_palettes_sorted(&self) -> bool {
        self.frames_with_sorted_palette()
            .all(|(_, palette, sorted)| palette.is_some() && sorted)
    }

    /// Count §20 Image Descriptor blocks whose §20.c.vii Interlace Flag
    /// is set (i.e. the on-disk pixel rows are arranged in the
    /// four-pass Appendix E pattern).
    ///
    /// Per §20.c.vii the Interlace Flag is a per-image property; a
    /// single stream may mix interlaced and non-interlaced frames. The
    /// decoder presents every frame already de-interlaced (the
    /// `Frame::indices` raster is row-major top-to-bottom regardless),
    /// but the original flag is preserved on [`Frame::interlaced`] so an
    /// encoder can round-trip it. This accessor is the stream-level
    /// roll-up of that bit — counts only [`Block::Image`] entries, never
    /// §24 Comment / §25 Plain Text / §26 Application (none of which
    /// have an Interlace Flag at all).
    pub fn interlaced_frame_count(&self) -> usize {
        self.frames().filter(|f| f.interlaced).count()
    }

    /// `true` when any §20 Image Descriptor block in the stream has its
    /// §20.c.vii Interlace Flag set.
    ///
    /// A streaming consumer that wants to know up front whether the
    /// stream relies on the Appendix E four-pass row reordering (so it
    /// can, for example, present partial decoded data progressively)
    /// can gate on this single query rather than walking
    /// [`Self::frames`] and inspecting each [`Frame::interlaced`].
    ///
    /// Returns `false` for a stream with no §20 Image blocks (every
    /// metadata-only stream) and for one whose every image leaves the
    /// Interlace Flag clear.
    pub fn has_interlaced_frames(&self) -> bool {
        self.frames().any(|f| f.interlaced)
    }

    /// `true` when every §20 Image Descriptor block in the stream has
    /// its §20.c.vii Interlace Flag set.
    ///
    /// Vacuously `true` for a zero-frame stream per [`Iterator::all`]'s
    /// empty-input contract — matches the shape of
    /// [`Self::all_frames_palettes_sorted`] and is the §20.c.vii
    /// companion to it. A caller that wants a strict "the stream has
    /// frames and every one is interlaced" check pairs this with
    /// [`Self::frame_count`] (`> 0`) or with
    /// [`Self::has_interlaced_frames`].
    pub fn all_frames_interlaced(&self) -> bool {
        self.frames().all(|f| f.interlaced)
    }

    /// Iterate every §20 Image Descriptor block paired with the §20.c.ix
    /// "Size of Local Color Table" 3-bit field value that would encode
    /// its attached Local Color Table on disk, in source order.
    ///
    /// Per §20.c.ix the field stores the *smallest* `N` in `0..=7` such
    /// that `2^(N+1)` is greater than or equal to the LCT's actual entry
    /// count. The §21 LCT then carries `3 × 2^(N+1)` bytes — the same
    /// power-of-two-rounded size relationship as §18.c.vi for the Global
    /// Color Table. The field is meaningful only when §20.c.vi Local
    /// Color Table Flag is set; per §20.c.ix "This value should be 0 if
    /// there is no Local Color Table specified", which this accessor
    /// surfaces as `None` (the LCT flag is implicitly clear too, since
    /// [`Frame::local_palette`] is `None`) rather than a sentinel `0`
    /// that a caller might confuse with the "2-entry LCT" case.
    ///
    /// The yielded field value is what an encoder writes for the LCT —
    /// see [`Self::frames_with_local_color_table_entry_count`] for the
    /// `2^(N+1)` on-disk entry-count companion. Use this accessor when
    /// pinning the encoded shape (round-trip, byte budget, conformance);
    /// use the entry-count accessor when reasoning about the actual
    /// number of LCT colours available to the §20 image.
    ///
    /// Only [`Block::Image`] entries contribute. §24 Comment / §25
    /// Plain Text / §26 Application carry no §20.c.ix at all.
    pub fn frames_with_local_color_table_size(&self) -> impl Iterator<Item = (&Frame, Option<u8>)> {
        self.frames().map(|f| (f, f.local_color_table_size_field()))
    }

    /// Iterate every §20 Image Descriptor block paired with the on-disk
    /// entry count its Local Color Table occupies (`2^(N+1)` per §20.c.ix
    /// / §21.a), in source order.
    ///
    /// `None` for §20 Images with no Local Color Table attached — the
    /// §20.c.vi Local Color Table Flag is clear, so §20.c.ix is undefined
    /// and no LCT bytes follow. `Some(count)` for §20 Images with an LCT
    /// attached, where `count` is the power-of-two-rounded number of
    /// entries (range `2..=256`) the on-disk LCT carries. `count` is
    /// always `>=` `frame.local_palette.as_ref().unwrap().len()`: the
    /// in-memory [`Frame::local_palette`] holds only the colours the
    /// stream actually carries, but the on-disk §21 table is rounded up
    /// to the next power of two with any unused tail entries written as
    /// the encoder's pad (this crate writes black; the spec leaves the
    /// pad value unspecified beyond "should not be referenced").
    ///
    /// Only [`Block::Image`] entries contribute (see
    /// [`Self::frames_with_local_color_table_size`] for the field-value
    /// companion).
    pub fn frames_with_local_color_table_entry_count(
        &self,
    ) -> impl Iterator<Item = (&Frame, Option<u32>)> {
        self.frames()
            .map(|f| (f, f.local_color_table_entry_count()))
    }

    /// Largest §20.c.ix "Size of Local Color Table" field value across
    /// every §20 Image Descriptor block in the stream that carries a
    /// Local Color Table.
    ///
    /// `None` when no §20 Image in the stream carries an LCT (every
    /// §20 frame's §20.c.vi Local Color Table Flag is clear, or the
    /// stream has no §20 Images at all). `Some(0..=7)` when at least
    /// one §20 Image attaches an LCT — the returned value is the
    /// maximum across those frames, i.e. the smallest `N` in `0..=7`
    /// that any individual LCT in the stream needs.
    ///
    /// Useful for a decoder allocating a reusable scratch LCT buffer
    /// up front: `2^(max + 1)` entries is enough for every §21 table
    /// the stream will produce, so the decoder never re-allocates
    /// per-frame. The Global Color Table sized via §18.c.vi is a
    /// separate concern — see [`Self::original_palette_color_count`]
    /// for the §18.c.iv source-richness counterpart.
    pub fn max_local_color_table_size_field(&self) -> Option<u8> {
        self.frames()
            .filter_map(|f| f.local_color_table_size_field())
            .max()
    }

    /// Iterate every Application Extension carried by this stream.
    pub fn application_extensions(&self) -> impl Iterator<Item = &Application> {
        self.blocks.iter().filter_map(|b| match b {
            Block::Application(a) => Some(a),
            _ => None,
        })
    }

    /// Iterate every §25 Plain Text Extension block paired with its
    /// attached §23 Graphic Control Extension, in source order.
    ///
    /// Per §23.d "This block can modify the Image Descriptor Block and
    /// the Plain Text Extension", a §23 GCE attaches to the
    /// immediately-following Plain Text Extension exactly as it does to
    /// a §20 Image. On decode that attachment is stored on
    /// [`Block::PlainText::graphic_control`]; this accessor surfaces the
    /// §25 half of that pairing — `(&PlainText, Option<GraphicControl>)`
    /// — in source order so a caller walking "every Plain Text block
    /// and the GCE that controls it" can do so without re-deriving the
    /// §23 → §25 attachment from [`Self::blocks`].
    ///
    /// The second tuple element is `None` for a Plain Text block with
    /// no preceding §23 GCE per §23.a "at most one Graphic Control
    /// Extension may precede a graphic rendering block".
    ///
    /// §20 Image blocks are not included — they are graphic-rendering
    /// blocks under §23.d but not §25 Plain Text. The §20-only
    /// companion is [`Self::frames_with_graphic_control`]; the
    /// timing / rendering-flag spine that walks both kinds together is
    /// [`Self::frame_delays`] / [`Self::has_transparency`] /
    /// [`Self::requires_user_input`].
    pub fn plain_texts(&self) -> impl Iterator<Item = (&PlainText, Option<GraphicControl>)> {
        self.blocks.iter().filter_map(|b| match b {
            Block::PlainText {
                params,
                graphic_control,
            } => Some((params, *graphic_control)),
            _ => None,
        })
    }

    /// Check every §25 Plain Text Extension payload against the §25.e
    /// printable-character recommendation: "If characters less than
    /// 0x20 or greater than 0xf7 are encountered, it is recommended
    /// that the decoder display a Space character (0x20)."
    ///
    /// Returns `true` only when every payload byte of every Plain Text
    /// Extension is in the inclusive range `0x20..=0xF7` — i.e. no
    /// in-payload byte would be substituted with a Space by a §25.e
    /// conforming renderer. Streams with no Plain Text Extensions
    /// trivially conform.
    ///
    /// §25.e is a *recommendation*, not a hard requirement — a stream
    /// with bytes outside the recommended range is still a valid
    /// GIF89a per §25.a. Consumers that want to honour the
    /// recommendation strictly (e.g. an authoring tool that refuses to
    /// emit a Plain Text Extension whose bytes would render as visible
    /// gaps) can gate emission on this check; the encoder itself does
    /// not enforce it.
    pub fn plain_texts_are_printable(&self) -> bool {
        self.plain_texts()
            .all(|(pt, _)| pt.text.iter().all(|b| (0x20..=0xF7).contains(b)))
    }

    /// Check every §25 Plain Text Extension against the §25.e
    /// "integral number of cells fit in the grid" encoder
    /// recommendation: "an encoder must be careful to specify the grid
    /// dimensions accurately so that this does not happen" (i.e. so
    /// that fractional cells need not be discarded).
    ///
    /// Returns `true` when every Plain Text block satisfies BOTH
    /// `width % cell_width == 0` and `height % cell_height == 0` — i.e.
    /// the §25.c text-grid rectangle is an integer number of character
    /// cells across and down. A block whose `cell_width` or
    /// `cell_height` is `0` does not satisfy the check (no integer
    /// division is defined and the §25 grid layout collapses); such a
    /// block fails the recommendation. Streams with no Plain Text
    /// Extensions trivially conform.
    ///
    /// §25.e is a *recommendation*, not a hard requirement — the
    /// spec's "fractional cells must be discarded" clause is the
    /// fall-back behaviour for the decoder when the encoder fails to
    /// pick clean dimensions. Consumers that author or re-emit Plain
    /// Text Extensions can gate on this check to ensure no glyph is
    /// silently cropped at the right or bottom edge of the grid.
    pub fn plain_texts_grid_fits_cells(&self) -> bool {
        self.plain_texts().all(|(pt, _)| {
            pt.cell_width != 0
                && pt.cell_height != 0
                && pt.width % pt.cell_width as u16 == 0
                && pt.height % pt.cell_height as u16 == 0
        })
    }

    /// Yield each §20 Image / §25 Plain Text graphic-rendering block's
    /// `(left, top, width, height)` placement rectangle in source order.
    ///
    /// Only the two graphic-rendering block types contribute: §20 Image
    /// Descriptors (`left`/`top`/`width`/`height`) and §25 Plain Text
    /// Extensions (the §25.c Text Grid `left`/`top`/`width`/`height`).
    /// §24 Comment and §26 Application Extensions have no placement and
    /// are skipped. The four values are exactly the coordinates §20.a /
    /// §25.a constrain against the §18 Logical Screen.
    fn placement_rects(&self) -> impl Iterator<Item = (u16, u16, u16, u16)> + '_ {
        self.blocks.iter().filter_map(|b| match b {
            Block::Image(f) => Some((f.left, f.top, f.width, f.height)),
            Block::PlainText { params, .. } => {
                Some((params.left, params.top, params.width, params.height))
            }
            Block::Comment(_) | Block::Application(_) => None,
        })
    }

    /// Report whether a single `(left, top, width, height)` placement
    /// rectangle fits within this stream's §18 Logical Screen.
    ///
    /// The §20.a / §25.a constraint is that the graphic-rendering block
    /// "must fit within the boundaries of the Logical Screen": the
    /// rectangle's right edge (`left + width`) must not exceed the §18.b
    /// Logical Screen Width and its bottom edge (`top + height`) must not
    /// exceed the §18.b Logical Screen Height. The `u16` coordinates are
    /// widened to `u32` for the edge sums so a placement near the 65 535
    /// coordinate ceiling cannot wrap. The left/top origin is always
    /// in-bounds by construction (both are `u16` ≥ 0 and a zero-extent
    /// edge sits exactly on the far boundary, which the `<=` admits).
    fn rect_fits_screen(&self, left: u16, top: u16, width: u16, height: u16) -> bool {
        let right = left as u32 + width as u32;
        let bottom = top as u32 + height as u32;
        right <= self.screen_width as u32 && bottom <= self.screen_height as u32
    }

    /// Report whether every §20 Image / §25 Plain Text graphic-rendering
    /// block fits within the §18 Logical Screen boundaries (§20.a /
    /// §25.a).
    ///
    /// `compose()` / [`Playback`] reject an out-of-bounds placement with
    /// an error (the spec makes "must fit within the boundaries of the
    /// Logical Screen" a hard requirement, not a recommendation, so the
    /// composer has no defined clipping behaviour to fall back on). This
    /// accessor surfaces the same check as a boolean so a consumer can
    /// validate a freshly-decoded or freshly-built stream up front —
    /// before attempting to render — without catching the compose error.
    ///
    /// Streams with no graphic-rendering blocks trivially conform
    /// (vacuously `true`), matching the shape of the surrounding
    /// stream-level rollups ([`Self::all_frames_interlaced`],
    /// [`Self::all_frames_palettes_sorted`]).
    ///
    /// [`Playback`]: crate::Playback
    pub fn all_blocks_fit_screen(&self) -> bool {
        self.placement_rects()
            .all(|(l, t, w, h)| self.rect_fits_screen(l, t, w, h))
    }

    /// Count §20 Image / §25 Plain Text graphic-rendering blocks whose
    /// placement rectangle escapes the §18 Logical Screen boundaries
    /// (§20.a / §25.a) — the complement of [`Self::all_blocks_fit_screen`]
    /// expressed as a count so a validator can report *how many* blocks
    /// would fail `compose()` rather than just whether any does.
    ///
    /// Zero exactly when [`Self::all_blocks_fit_screen`] is `true`.
    pub fn out_of_bounds_block_count(&self) -> usize {
        self.placement_rects()
            .filter(|&(l, t, w, h)| !self.rect_fits_screen(l, t, w, h))
            .count()
    }

    /// Iterate every §24 Comment Extension payload in source order.
    ///
    /// The CompuServe spec (§24.a) makes Comment Extensions OPTIONAL
    /// and explicitly allows any number of them to appear in the Data
    /// Stream, so the iterator surface is a sequence rather than a
    /// single payload. Encoders that want every byte of textual
    /// metadata in one buffer can call [`Self::concatenated_comment`].
    pub fn comments(&self) -> impl Iterator<Item = &[u8]> {
        self.blocks.iter().filter_map(|b| match b {
            Block::Comment(data) => Some(data.as_slice()),
            _ => None,
        })
    }

    /// Concatenate every §24 Comment Extension payload into a single
    /// buffer, separating consecutive comments with a single LF
    /// (`b'\n'`). Returns `None` when the stream carries no Comment
    /// Extension at all so callers can distinguish "no comments" from
    /// "one empty comment".
    ///
    /// The newline separator is a convenience for the common
    /// "show me every comment as one blob of text" use case; consumers
    /// that need to preserve the original boundary structure (e.g.
    /// re-emitting them as distinct blocks) should call
    /// [`Self::comments`] and concatenate themselves.
    pub fn concatenated_comment(&self) -> Option<Vec<u8>> {
        let mut iter = self.comments();
        let first = iter.next()?;
        let mut out = first.to_vec();
        for next in iter {
            out.push(b'\n');
            out.extend_from_slice(next);
        }
        Some(out)
    }

    /// Check every §24 Comment Extension payload against §24.e.i:
    /// "It should contain text using the 7-bit ASCII character set."
    ///
    /// Returns `true` only when every byte of every Comment Extension
    /// is in the 7-bit-ASCII range (`0x00..=0x7F`). Streams with no
    /// Comment Extensions trivially conform.
    ///
    /// §24.e is a *recommendation*, not a hard requirement — a stream
    /// with non-ASCII comment bytes is still a valid GIF89a per §24.a.
    /// Consumers that want to honour the recommendation strictly can
    /// gate emission on this check; the encoder itself does not enforce
    /// it.
    pub fn comments_are_7bit_ascii(&self) -> bool {
        self.comments()
            .all(|payload| payload.iter().all(|b| *b <= 0x7F))
    }

    /// Check every §24 Comment Extension against §24.e.ii:
    /// "they should be located at the beginning or at the end of the
    /// Data Stream to the extent possible."
    ///
    /// Returns `true` when every Comment Extension is either:
    ///
    /// * In the leading run — preceded by zero or more Comment /
    ///   Application Extension blocks, with no graphic-rendering block
    ///   (§20 Image or §25 Plain Text) before it. (Application
    ///   Extensions are allowed in the leading run because the
    ///   NETSCAPE2.0 / ANIMEXTS1.0 / XMP / ICC / Exif convention places
    ///   them between the Global Color Table and the first frame.)
    /// * In the trailing run — followed by zero or more Comment /
    ///   Application Extension blocks, with no graphic-rendering block
    ///   after it.
    ///
    /// Streams with no Comment Extensions trivially conform. The check
    /// is informational; the encoder accepts comments anywhere in the
    /// block list (since §12 / §15 / §24.d allow them there).
    pub fn comments_in_recommended_position(&self) -> bool {
        // Locate the bounding indices of the graphic-rendering blocks
        // (§20 Image + §25 Plain Text). Any Comment outside that range
        // is in the leading or trailing run; any Comment inside it
        // (strictly between the first and last graphic block) violates
        // §24.e.ii.
        let first_graphic = self
            .blocks
            .iter()
            .position(|b| matches!(b, Block::Image(_) | Block::PlainText { .. }));
        let last_graphic = self
            .blocks
            .iter()
            .rposition(|b| matches!(b, Block::Image(_) | Block::PlainText { .. }));
        let (Some(first), Some(last)) = (first_graphic, last_graphic) else {
            // No graphic blocks at all → every Comment is trivially in
            // a "leading or trailing" position.
            return true;
        };
        // first ≤ last by construction. A Comment violates the
        // recommendation only when its index lies strictly between
        // first and last (i.e. interleaved with graphic blocks).
        !self
            .blocks
            .iter()
            .enumerate()
            .any(|(i, b)| matches!(b, Block::Comment(_)) && i > first && i < last)
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

    /// Crop every §20 Image frame to the bounding rectangle of the
    /// pixels it actually changes on the composed logical screen — the
    /// inter-frame companion to [`Self::optimize_color_tables`].
    ///
    /// Animation tools commonly emit every frame as a full
    /// logical-screen raster even when consecutive frames differ in a
    /// small region. §20.c.ii–v give each Image its own `(left, top,
    /// width, height)` placement rectangle, so an encoder may instead
    /// ship only the changed region: pixels outside a frame's rectangle
    /// are simply not overwritten and the prior canvas shows through.
    /// This pass rewrites each frame to that minimal rectangle, which
    /// shrinks the §22 pixel payload (and the LZW-compressed stream
    /// with it) without altering anything a viewer displays.
    ///
    /// # What is preserved
    ///
    /// The composed output — [`crate::compose`] and
    /// [`crate::playback::Playback`] yield byte-identical RGBA
    /// canvases before and after
    /// the call. Equivalence is judged at the composed-RGBA level (the
    /// §18 + §23 display model): a frame pixel that re-draws the colour
    /// already on the canvas, or that is skipped via the §23.c.viii
    /// Transparent Index ("the corresponding pixel of the display
    /// device is not modified"), is croppable. Per-frame delays, GCEs,
    /// palettes, metadata blocks, and block order are untouched.
    ///
    /// # Which frames are eligible
    ///
    /// Frames whose §23.c.iv Disposal Method is rect-independent:
    /// values 0 / 1 (no disposal / do not dispose) and value 3 (restore
    /// to previous — the pixels a cropped frame no longer overwrites
    /// already equal the pre-render canvas, so restoring a smaller area
    /// is indistinguishable). Value 2 (restore to background) frames
    /// are **never** cropped: §23.c.iv requires "the area used by the
    /// graphic" to be cleared to the background colour, and shrinking
    /// that area would change what the next frame composes over. §25
    /// Plain Text blocks participate in the state machine but are never
    /// modified. A frame that changes nothing at all (an exact
    /// duplicate) shrinks to a 1×1 rectangle at its original top-left.
    ///
    /// Returns the number of frames whose rectangle was shrunk (`0`
    /// when nothing could be cropped). A stream that does not compose —
    /// placement escaping the §18 logical screen, a missing palette, an
    /// out-of-range pixel index — is left completely unmodified. The
    /// transformation is idempotent: a second call returns `0`.
    pub fn optimize_frame_rects(&mut self) -> usize {
        crate::compose::optimize_frame_rects_impl(self)
    }

    /// Minimum GIF version that this stream's block list requires.
    ///
    /// Returns the maximum of every contained [`Block::required_version`]
    /// — equivalent to "the earliest version that covers all the blocks
    /// in the Data Stream" (§7). A stream with only image blocks (no
    /// GCE, no Comment / Plain Text / Application Extensions) requires
    /// 87a; anything carrying a §23 GCE or any 89a-only extension block
    /// requires 89a.
    pub fn required_version(&self) -> Version {
        let mut required = Version::Gif87a;
        for block in &self.blocks {
            let v = block.required_version();
            if v > required {
                required = v;
            }
        }
        required
    }
}

impl GifImage {
    /// Bump [`GifImage::version`] up to [`Self::required_version`] when
    /// the current declared version is too low to cover the blocks in
    /// this stream. Returns `true` when a bump actually happened.
    ///
    /// The CompuServe encoder responsibility in §7 says "An encoder
    /// should use the earliest possible version number that includes
    /// all the blocks used in the Data Stream." This helper exists so a
    /// caller that mixed 89a-only blocks into an [`Version::Gif87a`]
    /// scaffold can fix the declared version in one call before encode
    /// — otherwise [`crate::encode`] rejects the input.
    ///
    /// This never *down*grades. A stream declared `Gif89a` with only
    /// 87a-required blocks stays `Gif89a` (downgrading would be a
    /// surprise to a caller that explicitly chose the later version).
    pub fn upgrade_version_if_needed(&mut self) -> bool {
        let required = self.required_version();
        if required > self.version {
            self.version = required;
            true
        } else {
            false
        }
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

    /// Resolve the §18.c.vii Background Color Index against the
    /// §18.c.ii Global Color Table.
    ///
    /// Per §18.c.vii the Background Color Index "represents the index
    /// of the Global Color Table … used for the Background Color. The
    /// Background Color is the color used for those pixels on the
    /// screen that are not covered by an image. If the Global Color
    /// Table Flag is set to (zero), this field should be zero and
    /// should be ignored."
    ///
    /// This accessor encodes both halves of the §18.c.vii contract:
    ///
    /// * Returns `None` when [`Self::global_palette`] is `None` (the
    ///   §18.c.iii Global Color Table Flag was zero on decode, or the
    ///   caller never attached one before encode) — the background
    ///   index has no meaning without a palette.
    /// * Returns `None` when [`Self::background_index`] points past
    ///   the end of [`Self::global_palette`]. The spec is silent on
    ///   out-of-range — `None` is the conservative reading and lets
    ///   downstream renderers fall back to their canvas-default colour
    ///   (transparent or black) without a spurious palette lookup.
    /// * Returns `Some(rgb)` otherwise, copying the indexed entry out
    ///   of the Global Color Table.
    ///
    /// [`Self::background_color_rgba`] is the alpha-extended form
    /// used by the §23 disposal-method canvas clear in
    /// [`crate::compose`] / [`crate::playback`].
    pub fn background_color(&self) -> Option<Rgb> {
        let palette = self.global_palette.as_ref()?;
        palette.get(self.background_index as usize).copied()
    }

    /// `[r, g, b, a]` form of [`Self::background_color`] suitable for
    /// the §23 dispose-to-background canvas clear.
    ///
    /// * `Some(rgb)` → `[r, g, b, 0xFF]` — fully-opaque colour pulled
    ///   from the §18 Global Color Table.
    /// * `None` (either no Global Color Table or an out-of-range
    ///   [`Self::background_index`]) → `[0, 0, 0, 0]` — fully
    ///   transparent black, the conservative fallback documented at
    ///   [`Self::background_color`].
    pub fn background_color_rgba(&self) -> [u8; 4] {
        match self.background_color() {
            Some(Rgb { r, g, b }) => [r, g, b, 0xFF],
            None => [0, 0, 0, 0],
        }
    }

    /// Decode the §18.c.iv Color Resolution field into the *bits per
    /// primary colour available to the original image*.
    ///
    /// §18.c.iv defines the raw byte ([`Self::color_resolution`]) as
    /// "Number of bits per primary color available to the original
    /// image, minus 1." A raw value of `3` therefore means the source
    /// palette had `4` bits per primary colour (i.e. `2^4 = 16` levels
    /// each on R, G and B for a `16 × 16 × 16 = 4096`-colour source
    /// palette). The spec scopes this to the *richness of the original
    /// palette*, not the number of colours actually used in the graphic
    /// — a quantiser that knocks the palette down to 256 entries should
    /// still report the original's resolution here so a renderer can
    /// pick the best display mode.
    ///
    /// The returned value is always in the range `1..=8`. The
    /// CompuServe spec carves out only `0..=7` for the raw 3-bit field,
    /// so `color_resolution_bits()` adds 1 and never overflows.
    pub fn color_resolution_bits(&self) -> u8 {
        // §18.c.iv stores raw value = bits per primary - 1.
        // Decoder masks to 3 bits so this max is 7 → bits returns 8.
        (self.color_resolution & 0b0000_0111) + 1
    }

    /// Number of distinct colours the *original* source palette could
    /// represent, derived from §18.c.iv Color Resolution.
    ///
    /// The §18.c.iv field reports the size of the source palette as
    /// bits-per-primary minus one (see [`Self::color_resolution_bits`]
    /// for the bit count); the total colour count is `2^(3 × bits)`
    /// (R, G and B each get `bits` bits). Raw values `0..=7` therefore
    /// map to:
    ///
    /// | raw | bits | colours       |
    /// |-----|------|---------------|
    /// | 0   | 1    | `8`           |
    /// | 1   | 2    | `64`          |
    /// | 2   | 3    | `512`         |
    /// | 3   | 4    | `4 096`       |
    /// | 4   | 5    | `32 768`      |
    /// | 5   | 6    | `262 144`     |
    /// | 6   | 7    | `2 097 152`   |
    /// | 7   | 8    | `16 777 216`  |
    ///
    /// Returned as a `u32` so the maximum (`16_777_216` for a 24-bit
    /// source palette) fits without loss; this is the richness of the
    /// *source*, not the count of palette entries in
    /// [`Self::global_palette`].
    pub fn original_palette_color_count(&self) -> u32 {
        let bits = self.color_resolution_bits() as u32;
        1u32 << (3 * bits)
    }

    /// Count of §20 Image Descriptor blocks in this stream.
    ///
    /// Mirrors `self.frames().count()` but reads as the intent at the
    /// call site. A still-image GIF returns `1`; a multi-frame
    /// animation returns the number of §20 Image blocks (§25 Plain
    /// Text blocks are *graphic-rendering* blocks but not §20 Images,
    /// so they do not count here — see [`Self::frame_delays`] for the
    /// timeline view that includes them).
    ///
    /// A metadata-only stream (only §24 Comment / §26 Application
    /// Extensions before the §27 Trailer) returns `0`. The strict
    /// [`crate::decode`] entry point rejects that shape per §12 ("a
    /// Data Stream shall contain at least one image"); the lenient
    /// [`crate::decode_lenient`] entry point can produce it after
    /// scanning past corrupted image data.
    pub fn frame_count(&self) -> usize {
        self.frames().count()
    }

    /// Count of §12 Graphic-Rendering blocks in this stream — every §20
    /// Image plus every §25 Plain Text Extension.
    ///
    /// Unlike [`Self::frame_count`] (§20 Images only), this includes
    /// §25 Plain Text blocks, which §12 also classifies as
    /// graphic-rendering ("the Image Descriptor and the Plain Text
    /// Extension"). It is the number of blocks that actually paint onto
    /// the §18 Logical Screen during [`crate::compose`].
    pub fn graphic_rendering_block_count(&self) -> usize {
        self.blocks
            .iter()
            .filter(|b| b.is_graphic_rendering())
            .count()
    }

    /// Count of §12 Special-Purpose blocks in this stream — every §24
    /// Comment plus every §26 Application Extension.
    ///
    /// §12: Special-Purpose blocks "are neither used to control the
    /// process of the Data Stream nor do they contain information or
    /// data used to render a graphic", and are "transparent to the
    /// decoding process". A renderer can skip all of them; this count
    /// tells a consumer how many metadata-only blocks the stream
    /// carries without walking the §24 / §26 accessors separately.
    pub fn special_purpose_block_count(&self) -> usize {
        self.blocks
            .iter()
            .filter(|b| b.is_special_purpose())
            .count()
    }

    /// `true` when this stream is a §11 "palette-loader" Data Stream:
    /// one that carries a §18 Global Color Table but **no**
    /// graphic-rendering block at all.
    ///
    /// §11 "About Color Tables" defines this shape explicitly: "The
    /// Definition of the GIF Format allows for a Data Stream to contain
    /// only the Header, the Logical Screen Descriptor, a Global Color
    /// Table and the GIF Trailer. Such a Data Stream would be used to
    /// load a decoder with a Global Color Table, in preparation for
    /// subsequent Data Streams without a color table at all." §11 also
    /// recommends a decoder "save the last Global Color Table used
    /// until another Global Color Table is encountered", so a later
    /// Data Stream with no table of its own renders against the table a
    /// palette-loader stream installed.
    ///
    /// This query returns `true` only when a §18 Global Color Table is
    /// present *and* the stream has no §20 Image and no §25 Plain Text
    /// Extension (§24 Comment / §26 Application Extension blocks are
    /// §12 "transparent" and do not disqualify the loader shape). The
    /// strict [`crate::decode`] entry point rejects an image-less
    /// stream, so this shape arises from [`crate::decode_lenient`] or
    /// from a freshly-built [`GifImage`]; the query lets a multi-stream
    /// consumer recognise a table-install stream before discarding it
    /// as "frameless".
    pub fn is_palette_loader_stream(&self) -> bool {
        self.global_palette.is_some() && self.graphic_rendering_block_count() == 0
    }

    /// Decode the §18.c.viii Pixel Aspect Ratio field into the
    /// width ÷ height ratio of a pixel in the original image.
    ///
    /// Per §18.c.viii, when the raw byte ([`Self::pixel_aspect_ratio`])
    /// is non-zero the ratio is computed by:
    ///
    /// ```text
    /// Aspect Ratio = (Pixel Aspect Ratio + 15) / 64
    /// ```
    ///
    /// A raw value of `0` means "no aspect ratio information is given"
    /// (§18.c.viii) and yields `None`. The value range `1..=255` maps to
    /// the widest pixel of 4:1 (raw `255` → `270/64 ≈ 4.22`, the spec's
    /// nominal "4:1") down to the tallest pixel of 1:4 (raw `1` →
    /// `16/64 = 0.25`), in increments of `1/64`.
    pub fn pixel_aspect_ratio_value(&self) -> Option<f32> {
        if self.pixel_aspect_ratio == 0 {
            None
        } else {
            Some((self.pixel_aspect_ratio as f32 + 15.0) / 64.0)
        }
    }

    /// Encode a desired pixel width ÷ height `ratio` into the raw
    /// §18.c.viii Pixel Aspect Ratio byte, inverting the §18 decode
    /// formula:
    ///
    /// ```text
    /// Pixel Aspect Ratio = round(ratio × 64) − 15
    /// ```
    ///
    /// The §18.c.viii value range `1..=255` represents the widest pixel
    /// of 4:1 down to the tallest pixel of 1:4. A `ratio` outside the
    /// representable span (anything that would round to a raw byte
    /// outside `1..=255`) returns `None` rather than silently clamping;
    /// callers that prefer clamping can do so themselves. The smallest
    /// representable ratio is `16/64 = 0.25` (raw `1`) and the largest is
    /// `270/64 ≈ 4.21875` (raw `255`). Square pixels (`ratio == 1.0`)
    /// map to raw `49`.
    ///
    /// Note that `0` ("no aspect ratio information") is *not*
    /// representable through this helper — set
    /// [`Self::pixel_aspect_ratio`] to `0` directly to clear the field.
    pub fn raw_pixel_aspect_ratio_for(ratio: f32) -> Option<u8> {
        if !ratio.is_finite() || ratio <= 0.0 {
            return None;
        }
        // Invert `(raw + 15) / 64`: raw = round(ratio * 64) - 15.
        let raw = (ratio * 64.0).round() - 15.0;
        if (1.0..=255.0).contains(&raw) {
            Some(raw as u8)
        } else {
            None
        }
    }

    /// Per-graphic-rendering-block §23.c.vii Delay Time, expressed as a
    /// [`Duration`], in source order.
    ///
    /// A GIF's animation timeline is the sequence of *graphic-rendering
    /// blocks* — §20 Image Descriptors **and** §25 Plain Text Extensions
    /// (both are "graphic-rendering blocks" whose scope a §23 Graphic
    /// Control Extension can modify per §23.d). Each yielded `Duration`
    /// is that block's §23.c.vii Delay Time ("the number of hundredths
    /// (1/100) of a second to wait before continuing with the processing
    /// of the Data Stream"), so 1 centi-second = 10 ms exactly. A block
    /// with no attached Graphic Control Extension — or one whose Delay
    /// Time is `0` ("not 0, this field specifies …", so `0` is "do not
    /// wait") — contributes [`Duration::ZERO`].
    ///
    /// This mirrors exactly the per-frame delay surfaced by
    /// [`crate::playback::PlaybackFrame::delay`]; the iterator here is the
    /// timing-only view for callers that want to total or inspect delays
    /// without compositing pixels.
    pub fn frame_delays(&self) -> impl Iterator<Item = Duration> + '_ {
        self.graphic_rendering_controls().map(|gce| {
            let centis = gce.map(|g| g.delay_centis).unwrap_or(0);
            Duration::from_millis(centis as u64 * 10)
        })
    }

    /// One item per *graphic-rendering block* (§20 Image **and** §25
    /// Plain Text — both are graphic-rendering blocks whose scope a §23
    /// Graphic Control Extension can modify per §23.d), in source order;
    /// the item is that block's attached [`GraphicControl`] (`None` when
    /// the block had no preceding §23 GCE).
    ///
    /// §24 Comment and §26 Application Extensions produce no rendered
    /// output, so they never carry a Graphic Control Extension and are
    /// skipped. This is the shared spine of the timing
    /// ([`Self::frame_delays`]) and rendering-flag
    /// ([`Self::has_transparency`] / [`Self::requires_user_input`])
    /// accessors.
    fn graphic_rendering_controls(&self) -> impl Iterator<Item = Option<GraphicControl>> + '_ {
        self.blocks.iter().filter_map(|b| match b {
            Block::Image(f) => Some(f.graphic_control),
            Block::PlainText {
                graphic_control, ..
            } => Some(*graphic_control),
            Block::Comment(_) | Block::Application(_) => None,
        })
    }

    /// `true` when any graphic-rendering block in the stream carries a
    /// §23 Graphic Control Extension with the §23.c.vi Transparency Flag
    /// set (i.e. a §23.c.viii Transparent Index is given).
    ///
    /// Per §23.c.viii a transparent pixel is one the decoder "does not
    /// modify", so a renderer that wants to know up front whether it must
    /// allocate an alpha channel (or preserve whatever is already on the
    /// canvas underneath each frame) can gate on this single query rather
    /// than walking every frame's [`GraphicControl::transparent_index`].
    ///
    /// Returns `false` for a stream with no Graphic Control Extensions at
    /// all (every still-image GIF87a) and for one whose GCEs all leave the
    /// Transparency Flag clear.
    pub fn has_transparency(&self) -> bool {
        self.graphic_rendering_controls()
            .flatten()
            .any(|gce| gce.transparent_index.is_some())
    }

    /// One item per *graphic-rendering block* (§20 Image **and** §25
    /// Plain Text — both carry a §23-attachable Graphic Control Extension
    /// per §23.d), in source order; the item is that block's §23.c.viii
    /// Transparent Index, or `None` when the block's GCE leaves the
    /// §23.c.vi Transparency Flag clear (value `0`, "Transparent Index is
    /// not given") or carries no GCE at all.
    ///
    /// This is the transparent-index-side companion to
    /// [`Self::frame_disposals`]: where [`Self::has_transparency`] answers
    /// the any-block question, this iterator surfaces *which* index each
    /// block skips, so a renderer building a per-frame alpha mask can walk
    /// it once rather than re-deriving the §23 → §20 / §25 attachment from
    /// [`Self::blocks`]. Per §23.c.viii the transparent pixel is one the
    /// decoder "does not modify"; the index is "present if and only if the
    /// Transparency Flag is set to 1", so a `None` here is exactly the
    /// "no transparency for this block" case. §24 Comment / §26
    /// Application Extensions produce no rendered output and carry no
    /// Transparent Index; they are skipped, matching the
    /// [`Self::frame_delays`] / [`Self::frame_disposals`] spine.
    pub fn frame_transparent_indices(&self) -> impl Iterator<Item = Option<u8>> + '_ {
        self.graphic_rendering_controls()
            .map(|gce| gce.and_then(|g| g.transparent_index))
    }

    /// The number of graphic-rendering blocks (§20 Image or §25 Plain
    /// Text) whose §23 Graphic Control Extension sets the §23.c.vi
    /// Transparency Flag (i.e. gives a §23.c.viii Transparent Index).
    ///
    /// The count-valued companion to [`Self::has_transparency`]: a stream
    /// with `transparent_index_count() == frame_count()` declares every
    /// graphic-rendering block transparent, while a value strictly between
    /// `0` and the block count flags a mixed stream where only some frames
    /// reserve a transparent index. Returns `0` for a stream with no
    /// Graphic Control Extensions and for one whose GCEs all leave the
    /// Transparency Flag clear.
    pub fn transparent_index_count(&self) -> usize {
        self.frame_transparent_indices().flatten().count()
    }

    /// `true` when any graphic-rendering block in the stream selects the
    /// given §23.c.viii Transparent Index.
    ///
    /// Per §23.c.viii the index addresses the active colour table (§21.a
    /// precedence: Local Color Table supersedes Global), so a caller that
    /// wants to know whether a particular palette slot is ever treated as
    /// transparent — for example, to decide whether reclaiming that slot
    /// for a fresh colour is safe during a palette-optimisation pass — can
    /// gate on this single query. A block with the §23.c.vi Transparency
    /// Flag clear contributes no index per [`Self::frame_transparent_indices`],
    /// so this returns `false` for a fully-opaque stream.
    pub fn uses_transparent_index(&self, index: u8) -> bool {
        self.frame_transparent_indices()
            .flatten()
            .any(|i| i == index)
    }

    /// `true` when **every** graphic-rendering block in the stream carries
    /// a §23 Graphic Control Extension with the §23.c.vi Transparency Flag
    /// set (a §23.c.viii Transparent Index is given for each).
    ///
    /// The every-block counterpart to [`Self::has_transparency`], matching
    /// the shape of [`Self::all_frames_use_disposal`] /
    /// [`Self::all_frames_interlaced`]: a renderer can gate the
    /// "allocate one alpha channel for the whole animation" fast path on
    /// this, rather than re-checking each frame. A graphic-rendering block
    /// with no attached GCE — or one whose Transparency Flag is clear —
    /// contributes `None` per [`Self::frame_transparent_indices`] and so
    /// makes this `false`. Vacuously `true` for a stream with no
    /// graphic-rendering blocks at all (only §24 Comment / §26 Application
    /// metadata).
    pub fn all_frames_transparent(&self) -> bool {
        self.frame_transparent_indices().all(|i| i.is_some())
    }

    /// `true` when any graphic-rendering block in the stream carries a
    /// §23 Graphic Control Extension with the §23.c.v User Input Flag set.
    ///
    /// Per §23.c.v "the decoder should wait for user input … before
    /// continuing", and §23.c.vii adds that when both a Delay Time and the
    /// User Input Flag are present, processing resumes "when user input is
    /// received or when the delay time expires, whichever occurs first".
    /// An interactive viewer can use this query to decide whether the
    /// stream needs an input-aware playback loop at all, instead of the
    /// purely time-driven loop sufficient for the common case.
    ///
    /// Returns `false` for a stream with no Graphic Control Extensions and
    /// for one whose GCEs all leave the User Input Flag clear.
    pub fn requires_user_input(&self) -> bool {
        self.graphic_rendering_controls()
            .flatten()
            .any(|gce| gce.user_input)
    }

    /// `true` when any graphic-rendering block's §23 Graphic Control
    /// Extension would make playback **block indefinitely** on user
    /// input — the §23.e.ii corner where the §23.c.v User Input Flag is
    /// set with no §23.c.vii Delay Time
    /// ([`GraphicControl::waits_for_user_input_indefinitely`]).
    ///
    /// This is a strictly stronger condition than
    /// [`Self::requires_user_input`]: a stream can require user input yet
    /// never block indefinitely if every user-input GCE also carries a
    /// non-zero Delay Time (per §23.c.vii, processing then resumes "when
    /// user input is received or when the delay time expires, whichever
    /// occurs first" — a bounded wait). When this query is `true`, a
    /// playback engine *must* be input-aware: there is at least one frame
    /// that a time-only loop would hang on forever, and
    /// [`Self::total_play_duration`] cannot account for it (the §23.e.ii
    /// wait has no upper bound).
    ///
    /// Returns `false` for a stream with no Graphic Control Extensions,
    /// one whose GCEs all leave the User Input Flag clear, and one whose
    /// user-input GCEs all pair the flag with a non-zero Delay Time.
    pub fn blocks_indefinitely_for_user_input(&self) -> bool {
        self.graphic_rendering_controls()
            .flatten()
            .any(|gce| gce.waits_for_user_input_indefinitely())
    }

    /// One item per *graphic-rendering block* (§20 Image **and** §25
    /// Plain Text — both are graphic-rendering blocks whose §23 Graphic
    /// Control Extension carries the Disposal Method field per §23.d),
    /// in source order; the item is that block's §23.c.iv Disposal
    /// Method.
    ///
    /// A graphic-rendering block with no attached GCE contributes
    /// [`DisposalMethod::None`] — per §23.c.iv value `0` is "No disposal
    /// specified", which is also the default for the missing-GCE case
    /// (the decoder "is not required to take any action"). §24 Comment
    /// and §26 Application Extensions produce no rendered output and so
    /// carry no Disposal Method; they are skipped, matching the
    /// [`Self::frame_delays`] / [`Self::has_transparency`] /
    /// [`Self::requires_user_input`] spine.
    ///
    /// This is the disposal-method-side companion to
    /// [`Self::frame_delays`]: a renderer that wants to know up front
    /// which disposal modes a stream exercises (and therefore which
    /// branches of the §23 disposal-method state machine it needs to
    /// implement) can walk this iterator once rather than re-deriving it
    /// from [`Self::blocks`].
    pub fn frame_disposals(&self) -> impl Iterator<Item = DisposalMethod> + '_ {
        self.graphic_rendering_controls()
            .map(|gce| gce.map(|g| g.disposal).unwrap_or(DisposalMethod::None))
    }

    /// `true` when any graphic-rendering block in the stream carries a
    /// §23 Graphic Control Extension with §23.c.iv Disposal Method
    /// `3` (*Restore To Previous*).
    ///
    /// §23.e.i flags this mode as the one that "imposes severe demands
    /// on the decoder to store the section of the graphic that needs to
    /// be saved" — a renderer that wants to skip pre-allocating the
    /// snapshot buffer for streams that never use it can gate on this
    /// single query rather than walking every frame's
    /// [`GraphicControl::disposal`]. The §23.e.i fallback recommendation
    /// for decoders that "cannot save an area" — restore to background
    /// colour instead — applies per-frame at render time; this query is
    /// the up-front decision point.
    ///
    /// Returns `false` for a stream with no Graphic Control Extensions
    /// at all and for one whose GCEs all use a non-`RestorePrevious`
    /// disposal.
    pub fn requires_canvas_snapshot(&self) -> bool {
        self.frame_disposals()
            .any(|d| matches!(d, DisposalMethod::RestorePrevious))
    }

    /// `true` when any graphic-rendering block in the stream uses the
    /// given §23.c.iv Disposal Method.
    ///
    /// A graphic-rendering block with no attached GCE counts as
    /// [`DisposalMethod::None`] per [`Self::frame_disposals`], so
    /// `uses_disposal(DisposalMethod::None)` is `true` for any stream
    /// that contains at least one §20 Image or §25 Plain Text without an
    /// attached GCE (the common GIF87a / un-controlled-89a case).
    ///
    /// Returns `false` for a zero-graphic-rendering-block stream (only
    /// metadata blocks).
    pub fn uses_disposal(&self, method: DisposalMethod) -> bool {
        self.frame_disposals().any(|d| d == method)
    }

    /// `true` when **every** graphic-rendering block in the stream uses
    /// the given §23.c.iv Disposal Method.
    ///
    /// A graphic-rendering block with no attached GCE counts as
    /// [`DisposalMethod::None`] per [`Self::frame_disposals`], so
    /// `all_frames_use_disposal(DisposalMethod::None)` covers the
    /// no-GCE-anywhere still-image case as well as the uniformly-disposed
    /// case.
    ///
    /// Vacuously `true` for a stream with no graphic-rendering blocks
    /// at all (only §24 Comment / §26 Application metadata), matching
    /// the shape of [`Self::all_frames_interlaced`] /
    /// [`Self::all_frames_palettes_sorted`].
    pub fn all_frames_use_disposal(&self, method: DisposalMethod) -> bool {
        self.frame_disposals().all(|d| d == method)
    }

    /// `true` when the stream carries more than one graphic-rendering
    /// block (§20 Image or §25 Plain Text) — i.e. the stream is a
    /// multi-frame animation rather than a single still.
    ///
    /// A single-frame GIF (the overwhelmingly common still-image case)
    /// returns `false` even if it carries a NETSCAPE2.0 *Looping*
    /// sub-block, because there is nothing to animate. A zero-frame
    /// stream (only metadata blocks) also returns `false`.
    pub fn is_animated(&self) -> bool {
        self.frame_delays().take(2).count() >= 2
    }

    /// Total wall-clock time of **one pass** through the animation: the
    /// sum of every graphic-rendering block's §23.c.vii Delay Time
    /// ([`Self::frame_delays`]).
    ///
    /// For a still image this is the single frame's delay (usually
    /// [`Duration::ZERO`]). The result never overflows: the per-block
    /// maximum is `65535 × 10 ms ≈ 655 s` and the block count is bounded
    /// by the parsed stream length, so the sum stays well inside
    /// [`Duration`]'s range.
    pub fn single_pass_duration(&self) -> Duration {
        self.frame_delays().sum()
    }

    /// Total wall-clock time to play the whole animation honouring the
    /// NETSCAPE2.0 / ANIMEXTS1.0 *Looping* sub-block, or `None` when the
    /// animation loops forever.
    ///
    /// The loop count comes from [`Self::loop_count`]; its de-facto
    /// "first pass plus *N* repeats" semantics (documented in
    /// `docs/image/gif/netscape2.0-loop-extension.md`) mean the number of
    /// passes is:
    ///
    /// * no *Looping* sub-block (`loop_count` is `None`) → 1 pass.
    /// * `Some(0)` → loop forever → returns `None`.
    /// * `Some(n)` → `n + 1` passes.
    ///
    /// The returned value is [`Self::single_pass_duration`] multiplied by
    /// the pass count. Returns `None` for the infinite-loop case so a
    /// caller can distinguish "plays for a finite time" from "never
    /// terminates"; saturating arithmetic guards the (practically
    /// unreachable) `Duration` overflow at `Some(65535)` passes of a
    /// maximal per-pass delay.
    pub fn total_play_duration(&self) -> Option<Duration> {
        let passes = match self.loop_count() {
            None => 1u64,
            Some(0) => return None,
            Some(n) => n as u64 + 1,
        };
        let per_pass = self.single_pass_duration();
        Some(per_pass.saturating_mul(passes.try_into().unwrap_or(u32::MAX)))
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

    /// §7 — `Version::Gif87a < Version::Gif89a`. The ordering supports
    /// `version.max(required)` for the "earliest covering version" rule.
    #[test]
    fn version_ordering_is_87a_then_89a() {
        assert!(Version::Gif87a < Version::Gif89a);
        assert_eq!(Version::Gif87a.max(Version::Gif89a), Version::Gif89a);
        assert_eq!(Version::Gif89a.max(Version::Gif87a), Version::Gif89a);
    }

    /// A pure-image stream (no extensions, no GCE) requires 87a.
    #[test]
    fn required_version_pure_image_is_87a() {
        let img = base_image(Some(pal3()), vec![Block::Image(frame_with(None))]);
        assert_eq!(img.required_version(), Version::Gif87a);
    }

    /// Any §23 GCE attached to an image lifts the requirement to 89a.
    #[test]
    fn required_version_image_with_gce_is_89a() {
        let mut f = frame_with(None);
        f.graphic_control = Some(GraphicControl {
            disposal: DisposalMethod::None,
            user_input: false,
            transparent_index: None,
            delay_centis: 0,
        });
        let img = base_image(Some(pal3()), vec![Block::Image(f)]);
        assert_eq!(img.required_version(), Version::Gif89a);
    }

    /// §24 Comment Extension forces 89a regardless of what other blocks
    /// look like.
    #[test]
    fn required_version_comment_is_89a() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(b"x".to_vec()),
                Block::Image(frame_with(None)),
            ],
        );
        assert_eq!(img.required_version(), Version::Gif89a);
    }

    /// §26 Application Extension forces 89a.
    #[test]
    fn required_version_application_is_89a() {
        let app = Application {
            identifier: *b"NETSCAPE",
            auth_code: *b"2.0",
            data: vec![0x01, 0x00, 0x00],
        };
        let img = base_image(
            Some(pal3()),
            vec![Block::Application(app), Block::Image(frame_with(None))],
        );
        assert_eq!(img.required_version(), Version::Gif89a);
    }

    /// §25 Plain Text Extension forces 89a.
    #[test]
    fn required_version_plain_text_is_89a() {
        let pt = PlainText {
            left: 0,
            top: 0,
            width: 1,
            height: 1,
            cell_width: 8,
            cell_height: 8,
            fg_color_index: 1,
            bg_color_index: 0,
            text: b"X".to_vec(),
        };
        let img = base_image(
            Some(pal3()),
            vec![
                Block::PlainText {
                    params: pt,
                    graphic_control: None,
                },
                Block::Image(frame_with(None)),
            ],
        );
        assert_eq!(img.required_version(), Version::Gif89a);
    }

    /// `upgrade_version_if_needed` bumps from 87a to 89a when an
    /// extension is present.
    #[test]
    fn upgrade_bumps_when_extension_added() {
        let mut img = base_image(Some(pal3()), vec![Block::Comment(b"hi".to_vec())]);
        img.version = Version::Gif87a;
        assert!(img.upgrade_version_if_needed());
        assert_eq!(img.version, Version::Gif89a);
    }

    /// `upgrade_version_if_needed` is a no-op when the declared version
    /// already covers every contained block.
    #[test]
    fn upgrade_is_noop_when_already_high_enough() {
        let mut img = base_image(Some(pal3()), vec![Block::Image(frame_with(None))]);
        img.version = Version::Gif89a;
        assert!(!img.upgrade_version_if_needed());
        assert_eq!(img.version, Version::Gif89a);
    }

    /// `upgrade_version_if_needed` never *downgrades* — a `Gif89a`
    /// stream with only 87a-required blocks stays `Gif89a` because the
    /// caller's explicit choice of the later version is respected.
    #[test]
    fn upgrade_never_downgrades() {
        // Pure image, no extensions → required is 87a, but caller said 89a.
        let mut img = base_image(Some(pal3()), vec![Block::Image(frame_with(None))]);
        img.version = Version::Gif89a;
        assert_eq!(img.required_version(), Version::Gif87a);
        assert!(!img.upgrade_version_if_needed());
        assert_eq!(img.version, Version::Gif89a, "must not downgrade");
    }

    // ---------------------------------------------------------------
    // §24 Comment Extension accessors.
    // ---------------------------------------------------------------

    /// `comments()` yields every §24 Comment Extension payload in
    /// source order and skips non-comment blocks.
    #[test]
    fn comments_iterator_yields_in_source_order_and_skips_non_comment_blocks() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(b"first".to_vec()),
                Block::Image(frame_with(None)),
                Block::Application(Application {
                    identifier: *b"NETSCAPE",
                    auth_code: *b"2.0",
                    data: vec![],
                }),
                Block::Comment(b"second".to_vec()),
            ],
        );
        let collected: Vec<&[u8]> = img.comments().collect();
        assert_eq!(collected, vec![b"first".as_slice(), b"second".as_slice()]);
    }

    /// `concatenated_comment` returns `None` when the stream carries no
    /// Comment Extension. This lets a caller distinguish "no comments"
    /// from "one empty comment", which `concatenated_comment` returns
    /// as `Some(vec![])`.
    #[test]
    fn concatenated_comment_returns_none_when_no_comments() {
        let img = base_image(Some(pal3()), vec![Block::Image(frame_with(None))]);
        assert!(img.concatenated_comment().is_none());
    }

    /// One Comment Extension → `concatenated_comment` returns its raw
    /// payload, no leading or trailing separator.
    #[test]
    fn concatenated_comment_single_comment_returned_verbatim() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(b"hello".to_vec()),
                Block::Image(frame_with(None)),
            ],
        );
        assert_eq!(img.concatenated_comment(), Some(b"hello".to_vec()));
    }

    /// Multiple Comment Extensions are joined by a single LF.
    #[test]
    fn concatenated_comment_multiple_comments_joined_with_lf() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(b"line one".to_vec()),
                Block::Image(frame_with(None)),
                Block::Comment(b"line two".to_vec()),
                Block::Comment(b"line three".to_vec()),
            ],
        );
        assert_eq!(
            img.concatenated_comment(),
            Some(b"line one\nline two\nline three".to_vec())
        );
    }

    /// An empty Comment Extension is still a valid §24 block (sub-block
    /// terminator only) — `concatenated_comment` returns `Some(vec![])`
    /// to distinguish it from `None`.
    #[test]
    fn concatenated_comment_one_empty_comment_returns_empty_vec() {
        let img = base_image(
            Some(pal3()),
            vec![Block::Comment(Vec::new()), Block::Image(frame_with(None))],
        );
        assert_eq!(img.concatenated_comment(), Some(Vec::new()));
    }

    /// §24.e.i — pure 7-bit ASCII (incl. control chars and `0x7F`) is
    /// reported as conforming.
    #[test]
    fn comments_are_7bit_ascii_pure_ascii_returns_true() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(b"hello world\n\t".to_vec()),
                Block::Image(frame_with(None)),
            ],
        );
        assert!(img.comments_are_7bit_ascii());
    }

    /// §24.e.i — any byte ≥ `0x80` flips the recommendation to "not
    /// conforming". The encoder still accepts the block (it's a
    /// recommendation, not a hard requirement).
    #[test]
    fn comments_are_7bit_ascii_non_ascii_byte_flips_check() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(vec![b'h', b'i', 0xE9]),
                Block::Image(frame_with(None)),
            ],
        );
        assert!(!img.comments_are_7bit_ascii());
    }

    /// §24.e.i — a stream with no Comment Extensions at all trivially
    /// conforms (vacuous truth on an empty iterator).
    #[test]
    fn comments_are_7bit_ascii_no_comments_is_true() {
        let img = base_image(Some(pal3()), vec![Block::Image(frame_with(None))]);
        assert!(img.comments_are_7bit_ascii());
    }

    /// §24.e.ii — every Comment in a leading run (before any graphic
    /// block) is conforming.
    #[test]
    fn comments_in_recommended_position_leading_run_passes() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(b"a".to_vec()),
                Block::Comment(b"b".to_vec()),
                Block::Image(frame_with(None)),
                Block::Image(frame_with(None)),
            ],
        );
        assert!(img.comments_in_recommended_position());
    }

    /// §24.e.ii — every Comment in a trailing run (after the last
    /// graphic block) is conforming.
    #[test]
    fn comments_in_recommended_position_trailing_run_passes() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)),
                Block::Image(frame_with(None)),
                Block::Comment(b"a".to_vec()),
                Block::Comment(b"b".to_vec()),
            ],
        );
        assert!(img.comments_in_recommended_position());
    }

    /// §24.e.ii — a Comment interleaved between two graphic blocks
    /// violates the recommendation.
    #[test]
    fn comments_in_recommended_position_interleaved_fails() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)),
                Block::Comment(b"a".to_vec()),
                Block::Image(frame_with(None)),
            ],
        );
        assert!(!img.comments_in_recommended_position());
    }

    /// §24.e.ii — Application Extensions between the leading Comments
    /// and the first graphic block (e.g. NETSCAPE2.0) do not break the
    /// recommendation — they're not graphic blocks.
    #[test]
    fn comments_in_recommended_position_app_extensions_between_comments_and_image_allowed() {
        let netscape = Application {
            identifier: *b"NETSCAPE",
            auth_code: *b"2.0",
            data: vec![0x01, 0x00, 0x00],
        };
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(b"leading".to_vec()),
                Block::Application(netscape),
                Block::Image(frame_with(None)),
            ],
        );
        assert!(img.comments_in_recommended_position());
    }

    /// §24.e.ii — a stream with no graphic blocks trivially conforms
    /// (any position is "the end of the data stream").
    #[test]
    fn comments_in_recommended_position_no_graphic_blocks_is_true() {
        let img = base_image(Some(pal3()), vec![Block::Comment(b"a".to_vec())]);
        assert!(img.comments_in_recommended_position());
    }

    /// §25 Plain Text is also a graphic-rendering block per §25.a —
    /// a Comment between two Plain Text blocks violates §24.e.ii.
    #[test]
    fn comments_in_recommended_position_plain_text_counts_as_graphic_block() {
        let pt = PlainText {
            left: 0,
            top: 0,
            width: 1,
            height: 1,
            cell_width: 8,
            cell_height: 8,
            fg_color_index: 1,
            bg_color_index: 0,
            text: b"X".to_vec(),
        };
        let img = base_image(
            Some(pal3()),
            vec![
                Block::PlainText {
                    params: pt.clone(),
                    graphic_control: None,
                },
                Block::Comment(b"in the middle".to_vec()),
                Block::PlainText {
                    params: pt,
                    graphic_control: None,
                },
            ],
        );
        assert!(!img.comments_in_recommended_position());
    }

    // -----------------------------------------------------------------
    // §18.c.viii Pixel Aspect Ratio.
    // -----------------------------------------------------------------

    /// §18.c.viii — a raw value of 0 means "no aspect ratio information
    /// is given", so the decoded value is `None`.
    #[test]
    fn pixel_aspect_ratio_zero_means_none() {
        let mut img = base_image(Some(pal3()), vec![]);
        img.pixel_aspect_ratio = 0;
        assert_eq!(img.pixel_aspect_ratio_value(), None);
    }

    /// §18.c.viii — `Aspect Ratio = (Pixel Aspect Ratio + 15) / 64`.
    /// Raw 49 → (49+15)/64 = 1.0 (square pixels), the spec's nominal
    /// midpoint.
    #[test]
    fn pixel_aspect_ratio_square_pixels() {
        let mut img = base_image(Some(pal3()), vec![]);
        img.pixel_aspect_ratio = 49;
        assert_eq!(img.pixel_aspect_ratio_value(), Some(1.0));
    }

    /// §18.c.viii endpoints: raw 1 → 16/64 = 0.25 (tallest, 1:4);
    /// raw 255 → 270/64 ≈ 4.21875 (widest, ~4:1).
    #[test]
    fn pixel_aspect_ratio_endpoints() {
        let mut img = base_image(Some(pal3()), vec![]);
        img.pixel_aspect_ratio = 1;
        assert_eq!(img.pixel_aspect_ratio_value(), Some(0.25));
        img.pixel_aspect_ratio = 255;
        let widest = img.pixel_aspect_ratio_value().unwrap();
        assert!((widest - 270.0 / 64.0).abs() < 1e-6, "{widest}");
    }

    /// The inverse helper inverts the §18 decode formula:
    /// raw = round(ratio × 64) − 15. Square pixels (1.0) → 49.
    #[test]
    fn raw_pixel_aspect_ratio_inverts_decode() {
        assert_eq!(GifImage::raw_pixel_aspect_ratio_for(1.0), Some(49));
        assert_eq!(GifImage::raw_pixel_aspect_ratio_for(0.25), Some(1));
        assert_eq!(
            GifImage::raw_pixel_aspect_ratio_for(270.0 / 64.0),
            Some(255)
        );
    }

    /// Encode → decode round-trips for every representable raw byte
    /// (1..=255): decoding raw R then re-encoding must recover R.
    #[test]
    fn pixel_aspect_ratio_roundtrips_all_raw_values() {
        let mut img = base_image(Some(pal3()), vec![]);
        for raw in 1u8..=255 {
            img.pixel_aspect_ratio = raw;
            let ratio = img.pixel_aspect_ratio_value().unwrap();
            assert_eq!(
                GifImage::raw_pixel_aspect_ratio_for(ratio),
                Some(raw),
                "raw {raw} -> ratio {ratio} did not round-trip"
            );
        }
    }

    /// §18.c.viii value range only spans 1:4 .. ~4:1. Ratios outside
    /// that span (and non-positive / non-finite inputs) are not
    /// representable and return `None` rather than clamping.
    #[test]
    fn raw_pixel_aspect_ratio_out_of_range_is_none() {
        // 0.25 is the smallest; anything meaningfully smaller is gone.
        assert_eq!(GifImage::raw_pixel_aspect_ratio_for(0.2), None);
        // ~4.22 is the largest; 5:1 is out of range.
        assert_eq!(GifImage::raw_pixel_aspect_ratio_for(5.0), None);
        assert_eq!(GifImage::raw_pixel_aspect_ratio_for(0.0), None);
        assert_eq!(GifImage::raw_pixel_aspect_ratio_for(-1.0), None);
        assert_eq!(GifImage::raw_pixel_aspect_ratio_for(f32::NAN), None);
        assert_eq!(GifImage::raw_pixel_aspect_ratio_for(f32::INFINITY), None);
    }

    fn frame_with_delay(delay_centis: u16) -> Frame {
        Frame {
            graphic_control: Some(GraphicControl {
                delay_centis,
                ..GraphicControl::default()
            }),
            ..frame_with(None)
        }
    }

    fn netscape_loop(loop_count: u16) -> Block {
        Block::Application(
            crate::app_ext::LoopControl {
                loop_count: Some(loop_count),
                buffer_size: None,
            }
            .to_application(),
        )
    }

    /// §23.c.vii: a block with no Graphic Control Extension, or one
    /// whose Delay Time is 0, contributes a zero delay. A block with a
    /// non-zero Delay Time contributes `centis × 10 ms`.
    #[test]
    fn frame_delays_reads_gce_delay_time() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_delay(50)), // 0.50 s
                Block::Image(frame_with(None)),     // no GCE -> 0
                Block::Image(frame_with_delay(0)),  // GCE but delay 0 -> 0
                Block::Comment(b"x".to_vec()),      // not a rendering block
                Block::Image(frame_with_delay(25)), // 0.25 s
            ],
        );
        let delays: Vec<Duration> = img.frame_delays().collect();
        assert_eq!(
            delays,
            vec![
                Duration::from_millis(500),
                Duration::ZERO,
                Duration::ZERO,
                Duration::from_millis(250),
            ]
        );
    }

    /// §25 Plain Text is a graphic-rendering block too, so its attached
    /// GCE Delay Time participates in the timeline alongside §20 Images.
    #[test]
    fn frame_delays_includes_plain_text_blocks() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_delay(10)),
                Block::PlainText {
                    params: PlainText {
                        left: 0,
                        top: 0,
                        width: 1,
                        height: 1,
                        cell_width: 1,
                        cell_height: 1,
                        fg_color_index: 1,
                        bg_color_index: 0,
                        text: b"A".to_vec(),
                    },
                    graphic_control: Some(GraphicControl {
                        delay_centis: 30,
                        ..GraphicControl::default()
                    }),
                },
            ],
        );
        assert_eq!(
            img.frame_delays().collect::<Vec<_>>(),
            vec![Duration::from_millis(100), Duration::from_millis(300)]
        );
    }

    fn frame_with_gce(gce: GraphicControl) -> Frame {
        Frame {
            graphic_control: Some(gce),
            ..frame_with(None)
        }
    }

    /// §23.c.vi / §23.c.viii — `has_transparency` is true iff some
    /// graphic-rendering block's GCE sets a Transparent Index.
    #[test]
    fn has_transparency_keys_off_transparent_index() {
        // No GCE anywhere -> no transparency.
        let none = base_image(Some(pal3()), vec![Block::Image(frame_with(None))]);
        assert!(!none.has_transparency());

        // A GCE present but with the Transparency Flag clear.
        let opaque = base_image(
            Some(pal3()),
            vec![Block::Image(frame_with_gce(GraphicControl {
                transparent_index: None,
                ..GraphicControl::default()
            }))],
        );
        assert!(!opaque.has_transparency());

        // A later frame turns transparency on -> whole stream reports true.
        let transparent = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)),
                Block::Comment(b"x".to_vec()),
                Block::Image(frame_with_gce(GraphicControl {
                    transparent_index: Some(0),
                    ..GraphicControl::default()
                })),
            ],
        );
        assert!(transparent.has_transparency());

        // §25 Plain Text is a graphic-rendering block too — its GCE counts.
        let pt = base_image(
            Some(pal3()),
            vec![Block::PlainText {
                params: PlainText {
                    left: 0,
                    top: 0,
                    width: 1,
                    height: 1,
                    cell_width: 1,
                    cell_height: 1,
                    fg_color_index: 1,
                    bg_color_index: 0,
                    text: b"A".to_vec(),
                },
                graphic_control: Some(GraphicControl {
                    transparent_index: Some(2),
                    ..GraphicControl::default()
                }),
            }],
        );
        assert!(pt.has_transparency());
    }

    /// §23.c.viii — `frame_transparent_indices` yields the GCE Transparent
    /// Index per graphic-rendering block in source order; a block with no
    /// attached GCE, or one whose §23.c.vi Transparency Flag is clear,
    /// contributes `None`. §24 / §26 metadata blocks are skipped, and §25
    /// Plain Text participates alongside §20 Image.
    #[test]
    fn frame_transparent_indices_reads_gce_transparent_index() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_gce(GraphicControl {
                    transparent_index: Some(2),
                    ..GraphicControl::default()
                })),
                Block::Image(frame_with(None)), // no GCE -> None
                Block::Image(frame_with_gce(GraphicControl {
                    transparent_index: None, // flag clear -> None
                    ..GraphicControl::default()
                })),
                Block::Comment(b"x".to_vec()), // not a rendering block
                Block::PlainText {
                    params: PlainText {
                        left: 0,
                        top: 0,
                        width: 1,
                        height: 1,
                        cell_width: 1,
                        cell_height: 1,
                        fg_color_index: 1,
                        bg_color_index: 0,
                        text: b"A".to_vec(),
                    },
                    graphic_control: Some(GraphicControl {
                        transparent_index: Some(0),
                        ..GraphicControl::default()
                    }),
                },
            ],
        );
        assert_eq!(
            img.frame_transparent_indices().collect::<Vec<_>>(),
            vec![Some(2), None, None, Some(0)]
        );
    }

    /// §23.c.vi — `transparent_index_count` counts only the
    /// graphic-rendering blocks whose Transparency Flag is set.
    #[test]
    fn transparent_index_count_counts_transparency_flag_set() {
        // Zero GCEs / all-opaque -> 0.
        let opaque = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)),
                Block::Image(frame_with_gce(GraphicControl {
                    transparent_index: None,
                    ..GraphicControl::default()
                })),
            ],
        );
        assert_eq!(opaque.transparent_index_count(), 0);

        // Two of three rendering blocks transparent.
        let mixed = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_gce(GraphicControl {
                    transparent_index: Some(1),
                    ..GraphicControl::default()
                })),
                Block::Image(frame_with(None)),
                Block::Image(frame_with_gce(GraphicControl {
                    transparent_index: Some(2),
                    ..GraphicControl::default()
                })),
            ],
        );
        assert_eq!(mixed.transparent_index_count(), 2);
    }

    /// §23.c.viii — `uses_transparent_index` reports whether any block
    /// selects a specific palette slot as transparent.
    #[test]
    fn uses_transparent_index_matches_specific_slot() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_gce(GraphicControl {
                    transparent_index: Some(2),
                    ..GraphicControl::default()
                })),
                Block::Image(frame_with(None)),
            ],
        );
        assert!(img.uses_transparent_index(2));
        assert!(!img.uses_transparent_index(0));

        // A fully-opaque stream never matches any index.
        let opaque = base_image(Some(pal3()), vec![Block::Image(frame_with(None))]);
        assert!(!opaque.uses_transparent_index(0));
    }

    /// §23.c.vi — `all_frames_transparent` is true iff every
    /// graphic-rendering block gives a Transparent Index; vacuously true
    /// for a metadata-only stream.
    #[test]
    fn all_frames_transparent_requires_every_block_flagged() {
        // Every rendering block transparent.
        let all = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_gce(GraphicControl {
                    transparent_index: Some(0),
                    ..GraphicControl::default()
                })),
                Block::Image(frame_with_gce(GraphicControl {
                    transparent_index: Some(1),
                    ..GraphicControl::default()
                })),
            ],
        );
        assert!(all.all_frames_transparent());

        // One opaque block -> false.
        let mixed = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_gce(GraphicControl {
                    transparent_index: Some(0),
                    ..GraphicControl::default()
                })),
                Block::Image(frame_with(None)),
            ],
        );
        assert!(!mixed.all_frames_transparent());

        // Vacuously true: only metadata, no graphic-rendering blocks.
        let meta_only = base_image(Some(pal3()), vec![Block::Comment(b"x".to_vec())]);
        assert!(meta_only.all_frames_transparent());

        // Cross-check against has_transparency / count on the all-true case.
        assert!(all.has_transparency());
        assert_eq!(all.transparent_index_count(), 2);
    }

    /// §23.c.v — `requires_user_input` is true iff some graphic-rendering
    /// block's GCE sets the User Input Flag.
    #[test]
    fn requires_user_input_keys_off_user_input_flag() {
        let none = base_image(Some(pal3()), vec![Block::Image(frame_with(None))]);
        assert!(!none.requires_user_input());

        let no_wait = base_image(
            Some(pal3()),
            vec![Block::Image(frame_with_gce(GraphicControl {
                user_input: false,
                ..GraphicControl::default()
            }))],
        );
        assert!(!no_wait.requires_user_input());

        let waits = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)),
                Block::Image(frame_with_gce(GraphicControl {
                    user_input: true,
                    ..GraphicControl::default()
                })),
            ],
        );
        assert!(waits.requires_user_input());
    }

    /// §23.e.ii — `blocks_indefinitely_for_user_input` is true iff some
    /// graphic-rendering block's GCE sets the User Input Flag *and* leaves
    /// the Delay Time at 0 (the "wait indefinitely" corner). A user-input
    /// GCE that also carries a non-zero Delay Time is a bounded wait
    /// (§23.c.vii) and does not count.
    #[test]
    fn blocks_indefinitely_only_when_user_input_without_delay() {
        // No GCE anywhere -> never blocks indefinitely.
        let none = base_image(Some(pal3()), vec![Block::Image(frame_with(None))]);
        assert!(!none.blocks_indefinitely_for_user_input());

        // User input set but with a Delay Time -> bounded wait per
        // §23.c.vii, so requires_user_input but not indefinite.
        let bounded = base_image(
            Some(pal3()),
            vec![Block::Image(frame_with_gce(GraphicControl {
                user_input: true,
                delay_centis: 50,
                ..GraphicControl::default()
            }))],
        );
        assert!(bounded.requires_user_input());
        assert!(!bounded.blocks_indefinitely_for_user_input());

        // Delay Time but no user input -> not indefinite either.
        let timed = base_image(
            Some(pal3()),
            vec![Block::Image(frame_with_gce(GraphicControl {
                user_input: false,
                delay_centis: 0,
                ..GraphicControl::default()
            }))],
        );
        assert!(!timed.blocks_indefinitely_for_user_input());

        // User input with no Delay Time -> §23.e.ii indefinite wait.
        let indefinite = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)),
                Block::Image(frame_with_gce(GraphicControl {
                    user_input: true,
                    delay_centis: 0,
                    ..GraphicControl::default()
                })),
            ],
        );
        assert!(indefinite.requires_user_input());
        assert!(indefinite.blocks_indefinitely_for_user_input());

        // Per-GCE predicate matches the stream-level any-block roll-up.
        assert!(GraphicControl {
            user_input: true,
            delay_centis: 0,
            ..GraphicControl::default()
        }
        .waits_for_user_input_indefinitely());
        assert!(!GraphicControl {
            user_input: true,
            delay_centis: 1,
            ..GraphicControl::default()
        }
        .waits_for_user_input_indefinitely());
        assert!(!GraphicControl {
            user_input: false,
            delay_centis: 0,
            ..GraphicControl::default()
        }
        .waits_for_user_input_indefinitely());
    }

    /// §23.c.iv — `frame_disposals` yields the GCE Disposal Method per
    /// graphic-rendering block in source order; a block with no attached
    /// GCE contributes `DisposalMethod::None`. §24 / §26 metadata
    /// blocks are skipped.
    #[test]
    fn frame_disposals_reads_gce_disposal_method() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_gce(GraphicControl {
                    disposal: DisposalMethod::RestoreBackground,
                    ..GraphicControl::default()
                })),
                Block::Image(frame_with(None)), // no GCE -> None
                Block::Comment(b"x".to_vec()),  // skipped
                Block::Image(frame_with_gce(GraphicControl {
                    disposal: DisposalMethod::RestorePrevious,
                    ..GraphicControl::default()
                })),
            ],
        );
        let disposals: Vec<DisposalMethod> = img.frame_disposals().collect();
        assert_eq!(
            disposals,
            vec![
                DisposalMethod::RestoreBackground,
                DisposalMethod::None,
                DisposalMethod::RestorePrevious,
            ]
        );
    }

    /// §25 Plain Text is a graphic-rendering block too, so its attached
    /// GCE Disposal Method participates in the disposal-method spine
    /// alongside §20 Images — mirrors `frame_delays_includes_plain_text_blocks`.
    #[test]
    fn frame_disposals_includes_plain_text_blocks() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_gce(GraphicControl {
                    disposal: DisposalMethod::Keep,
                    ..GraphicControl::default()
                })),
                Block::PlainText {
                    params: PlainText {
                        left: 0,
                        top: 0,
                        width: 1,
                        height: 1,
                        cell_width: 1,
                        cell_height: 1,
                        fg_color_index: 1,
                        bg_color_index: 0,
                        text: b"A".to_vec(),
                    },
                    graphic_control: Some(GraphicControl {
                        disposal: DisposalMethod::RestoreBackground,
                        ..GraphicControl::default()
                    }),
                },
            ],
        );
        assert_eq!(
            img.frame_disposals().collect::<Vec<_>>(),
            vec![DisposalMethod::Keep, DisposalMethod::RestoreBackground]
        );
    }

    /// §23.c.iv / §23.e.i — `requires_canvas_snapshot` is true iff some
    /// graphic-rendering block's GCE selects RestorePrevious.
    #[test]
    fn requires_canvas_snapshot_keys_off_restore_previous() {
        // No GCEs anywhere -> no snapshot needed.
        let none = base_image(Some(pal3()), vec![Block::Image(frame_with(None))]);
        assert!(!none.requires_canvas_snapshot());

        // GCEs present but all non-RestorePrevious.
        let no_snapshot = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_gce(GraphicControl {
                    disposal: DisposalMethod::Keep,
                    ..GraphicControl::default()
                })),
                Block::Image(frame_with_gce(GraphicControl {
                    disposal: DisposalMethod::RestoreBackground,
                    ..GraphicControl::default()
                })),
            ],
        );
        assert!(!no_snapshot.requires_canvas_snapshot());

        // A single RestorePrevious anywhere -> snapshot needed.
        let snapshot = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)),
                Block::Image(frame_with_gce(GraphicControl {
                    disposal: DisposalMethod::RestorePrevious,
                    ..GraphicControl::default()
                })),
            ],
        );
        assert!(snapshot.requires_canvas_snapshot());

        // §25 Plain Text carrying a RestorePrevious GCE counts too.
        let pt = base_image(
            Some(pal3()),
            vec![Block::PlainText {
                params: PlainText {
                    left: 0,
                    top: 0,
                    width: 1,
                    height: 1,
                    cell_width: 1,
                    cell_height: 1,
                    fg_color_index: 1,
                    bg_color_index: 0,
                    text: b"A".to_vec(),
                },
                graphic_control: Some(GraphicControl {
                    disposal: DisposalMethod::RestorePrevious,
                    ..GraphicControl::default()
                }),
            }],
        );
        assert!(pt.requires_canvas_snapshot());
    }

    /// `uses_disposal` reports whether any block uses the queried method.
    /// `all_frames_use_disposal` is the every-block query, vacuously true
    /// for zero-rendering-block streams.
    #[test]
    fn uses_disposal_and_all_frames_use_disposal() {
        // Zero-rendering-block stream: `uses_disposal` is false for every
        // method, `all_frames_use_disposal` is vacuously true.
        let meta_only = base_image(Some(pal3()), vec![Block::Comment(b"c".to_vec())]);
        assert!(!meta_only.uses_disposal(DisposalMethod::None));
        assert!(!meta_only.uses_disposal(DisposalMethod::RestorePrevious));
        assert!(meta_only.all_frames_use_disposal(DisposalMethod::None));
        assert!(meta_only.all_frames_use_disposal(DisposalMethod::Keep));

        // No-GCE still — counts as DisposalMethod::None per the spine.
        let still = base_image(Some(pal3()), vec![Block::Image(frame_with(None))]);
        assert!(still.uses_disposal(DisposalMethod::None));
        assert!(!still.uses_disposal(DisposalMethod::Keep));
        assert!(still.all_frames_use_disposal(DisposalMethod::None));
        assert!(!still.all_frames_use_disposal(DisposalMethod::Keep));

        // Mixed disposals — `uses_disposal` true for each present method,
        // `all_frames_use_disposal` false for every method.
        let mixed = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_gce(GraphicControl {
                    disposal: DisposalMethod::Keep,
                    ..GraphicControl::default()
                })),
                Block::Image(frame_with_gce(GraphicControl {
                    disposal: DisposalMethod::RestoreBackground,
                    ..GraphicControl::default()
                })),
            ],
        );
        assert!(mixed.uses_disposal(DisposalMethod::Keep));
        assert!(mixed.uses_disposal(DisposalMethod::RestoreBackground));
        assert!(!mixed.uses_disposal(DisposalMethod::None));
        assert!(!mixed.uses_disposal(DisposalMethod::RestorePrevious));
        assert!(!mixed.all_frames_use_disposal(DisposalMethod::Keep));
        assert!(!mixed.all_frames_use_disposal(DisposalMethod::RestoreBackground));

        // Uniform disposal across every block -> `all_frames_use_disposal`
        // is true for that method only.
        let uniform = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_gce(GraphicControl {
                    disposal: DisposalMethod::Keep,
                    ..GraphicControl::default()
                })),
                Block::Image(frame_with_gce(GraphicControl {
                    disposal: DisposalMethod::Keep,
                    ..GraphicControl::default()
                })),
            ],
        );
        assert!(uniform.all_frames_use_disposal(DisposalMethod::Keep));
        assert!(!uniform.all_frames_use_disposal(DisposalMethod::None));
    }

    /// `frame_disposals` must match exactly the disposal method that
    /// `frames_with_graphic_control` carries — the two views share the
    /// §20 image spine and must agree on the disposal field.
    #[test]
    fn frame_disposals_matches_frames_with_graphic_control() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_gce(GraphicControl {
                    disposal: DisposalMethod::RestoreBackground,
                    ..GraphicControl::default()
                })),
                Block::Image(frame_with(None)),
                Block::Image(frame_with_gce(GraphicControl {
                    disposal: DisposalMethod::Keep,
                    ..GraphicControl::default()
                })),
            ],
        );
        let from_disposals: Vec<DisposalMethod> = img.frame_disposals().collect();
        let from_pairs: Vec<DisposalMethod> = img
            .frames_with_graphic_control()
            .map(|(_, gce)| gce.map(|g| g.disposal).unwrap_or(DisposalMethod::None))
            .collect();
        assert_eq!(from_disposals, from_pairs);
    }

    /// `is_animated` keys off the count of graphic-rendering blocks, not
    /// the presence of a NETSCAPE loop block.
    #[test]
    fn is_animated_counts_rendering_blocks() {
        // Zero frames (metadata only) -> not animated.
        let meta_only = base_image(Some(pal3()), vec![Block::Comment(b"c".to_vec())]);
        assert!(!meta_only.is_animated());

        // One frame, even with a loop block, is a still.
        let still = base_image(
            Some(pal3()),
            vec![netscape_loop(0), Block::Image(frame_with(None))],
        );
        assert!(!still.is_animated());

        // Two frames -> animated.
        let anim = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)),
                Block::Image(frame_with(None)),
            ],
        );
        assert!(anim.is_animated());
    }

    /// `single_pass_duration` sums every frame's §23.c.vii delay once.
    #[test]
    fn single_pass_duration_sums_frame_delays() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_delay(10)),
                Block::Image(frame_with_delay(20)),
                Block::Image(frame_with_delay(70)),
            ],
        );
        assert_eq!(img.single_pass_duration(), Duration::from_millis(1000));
    }

    /// `total_play_duration` honours NETSCAPE2.0 *Looping* semantics:
    /// no block -> 1 pass; `Some(n)` -> `n + 1` passes; `Some(0)` ->
    /// infinite -> `None`.
    #[test]
    fn total_play_duration_honours_loop_count() {
        let frames = || {
            vec![
                Block::Image(frame_with_delay(10)),
                Block::Image(frame_with_delay(10)),
            ]
        };
        let per_pass = Duration::from_millis(200);

        // No loop block: a single pass.
        let no_loop = base_image(Some(pal3()), frames());
        assert_eq!(no_loop.total_play_duration(), Some(per_pass));

        // Some(2): first pass plus two repeats = three passes.
        let mut three = frames();
        three.insert(0, netscape_loop(2));
        let img = base_image(Some(pal3()), three);
        assert_eq!(img.total_play_duration(), Some(per_pass * 3));

        // Some(0): loop forever -> None.
        let mut forever = frames();
        forever.insert(0, netscape_loop(0));
        let inf = base_image(Some(pal3()), forever);
        assert_eq!(inf.total_play_duration(), None);
    }

    /// The timeline view matches the playback iterator's per-frame
    /// delays exactly (same blocks, same §23.c.vii reading).
    #[test]
    fn frame_delays_matches_playback_iterator() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_delay(7)),
                Block::Image(frame_with(None)),
                Block::Image(frame_with_delay(42)),
            ],
        );
        let from_timeline: Vec<Duration> = img.frame_delays().collect();
        let from_playback: Vec<Duration> = crate::playback::Playback::new(&img)
            .frames()
            .map(|r| r.unwrap().delay)
            .collect();
        assert_eq!(from_timeline, from_playback);
    }

    /// §18.c.vii — with a Global Color Table present and an in-range
    /// `background_index`, `background_color` resolves to that GCT
    /// entry and `background_color_rgba` is the same triplet with a
    /// fully-opaque alpha byte.
    #[test]
    fn background_color_resolves_via_global_palette() {
        let mut img = base_image(Some(pal3()), Vec::new());
        img.background_index = 2;
        assert_eq!(img.background_color(), Some(Rgb::new(3, 3, 3)));
        assert_eq!(img.background_color_rgba(), [3, 3, 3, 0xFF]);

        img.background_index = 0;
        assert_eq!(img.background_color(), Some(Rgb::new(1, 1, 1)));
        assert_eq!(img.background_color_rgba(), [1, 1, 1, 0xFF]);
    }

    /// §18.c.iii — when the Global Color Table Flag is zero (no
    /// `global_palette`), `background_index` is "meaningless"; the
    /// accessor surfaces that as `None` / fully-transparent black.
    #[test]
    fn background_color_none_without_global_palette() {
        let mut img = base_image(None, Vec::new());
        img.background_index = 0;
        assert_eq!(img.background_color(), None);
        assert_eq!(img.background_color_rgba(), [0, 0, 0, 0]);

        img.background_index = 200;
        assert_eq!(img.background_color(), None);
        assert_eq!(img.background_color_rgba(), [0, 0, 0, 0]);
    }

    /// §18.c.vii is silent on what to do when the index falls past
    /// the end of the GCT. The accessor takes the conservative
    /// reading — `None` / transparent black — rather than panicking
    /// or wrapping.
    #[test]
    fn background_color_none_for_out_of_range_index() {
        let mut img = base_image(Some(pal3()), Vec::new());
        // pal3() has 3 entries (indices 0..=2); 3 is the first
        // out-of-range index.
        img.background_index = 3;
        assert_eq!(img.background_color(), None);
        assert_eq!(img.background_color_rgba(), [0, 0, 0, 0]);

        img.background_index = u8::MAX;
        assert_eq!(img.background_color(), None);
        assert_eq!(img.background_color_rgba(), [0, 0, 0, 0]);
    }

    /// `frames_with_palette` returns the §18 Global Color Table for a
    /// frame whose Local Color Table flag is clear (§21.a).
    #[test]
    fn frames_with_palette_falls_back_to_gct() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)),
                Block::Image(frame_with(None)),
            ],
        );
        let collected: Vec<_> = img
            .frames_with_palette()
            .map(|(_, p)| p.map(<[Rgb]>::to_vec))
            .collect();
        assert_eq!(collected, vec![Some(pal3()), Some(pal3())]);
    }

    /// §21.a — "this color table temporarily becomes the active color
    /// table". When a Local Color Table is present it supersedes the
    /// Global Color Table for the frame that follows.
    #[test]
    fn frames_with_palette_prefers_lct_over_gct() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(Some(pal3_alt()))),
                Block::Image(frame_with(None)),
            ],
        );
        let collected: Vec<_> = img
            .frames_with_palette()
            .map(|(_, p)| p.map(<[Rgb]>::to_vec))
            .collect();
        assert_eq!(collected, vec![Some(pal3_alt()), Some(pal3())]);
    }

    /// §13 / §21 — "a Data Stream which does not contain either a
    /// Global Color Table or a Local Color Table" — neither table is
    /// available, so the yielded palette is `None`.
    #[test]
    fn frames_with_palette_none_when_no_tables_at_all() {
        let img = base_image(None, vec![Block::Image(frame_with(None))]);
        let collected: Vec<_> = img.frames_with_palette().map(|(_, p)| p).collect();
        assert_eq!(collected.len(), 1);
        assert!(collected[0].is_none());
    }

    /// Non-Image blocks (§24 Comment, §25 Plain Text, §26 Application)
    /// are skipped — only §20 Image Descriptors are paired with a
    /// palette, matching `frames()`.
    #[test]
    fn frames_with_palette_skips_non_image_blocks() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(b"first".to_vec()),
                Block::Image(frame_with(None)),
                Block::Application(Application {
                    identifier: *b"OXIDE001",
                    auth_code: *b"r81",
                    data: Vec::new(),
                }),
                Block::Image(frame_with(Some(pal3_alt()))),
                Block::Comment(b"last".to_vec()),
            ],
        );
        let collected: Vec<_> = img
            .frames_with_palette()
            .map(|(_, p)| p.map(<[Rgb]>::to_vec))
            .collect();
        assert_eq!(collected, vec![Some(pal3()), Some(pal3_alt())]);
    }

    /// The yielded `&Frame` reference points at the same frame
    /// `frames()` would yield — the new iterator is a strict extension
    /// of the existing one.
    #[test]
    fn frames_with_palette_frame_handle_matches_frames() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(Some(pal3_alt()))),
                Block::Image(frame_with(None)),
            ],
        );
        let lhs: Vec<*const Frame> = img.frames().map(|f| f as *const _).collect();
        let rhs: Vec<*const Frame> = img
            .frames_with_palette()
            .map(|(f, _)| f as *const _)
            .collect();
        assert_eq!(lhs, rhs);
    }

    /// `frames_with_graphic_control` returns `None` for every image in
    /// a stream that carries no §23 Graphic Control Extensions (the
    /// still-image / GIF87a case).
    #[test]
    fn frames_with_graphic_control_none_when_no_gce() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)),
                Block::Image(frame_with(None)),
            ],
        );
        let collected: Vec<_> = img
            .frames_with_graphic_control()
            .map(|(_, gce)| gce)
            .collect();
        assert_eq!(collected, vec![None, None]);
    }

    /// §23.a "The scope of this extension is the first graphic
    /// rendering block to follow" — a GCE attached to the §20 Image
    /// surfaces verbatim on the paired iterator item.
    #[test]
    fn frames_with_graphic_control_returns_attached_gce() {
        let gce = GraphicControl {
            disposal: DisposalMethod::RestoreBackground,
            user_input: true,
            transparent_index: Some(2),
            delay_centis: 50,
        };
        let mut f = frame_with(Some(pal3()));
        f.graphic_control = Some(gce);
        let img = base_image(Some(pal3()), vec![Block::Image(f)]);
        let collected: Vec<_> = img.frames_with_graphic_control().map(|(_, g)| g).collect();
        assert_eq!(collected, vec![Some(gce)]);
    }

    /// A mixed stream where only some §20 Images carry a §23 GCE — the
    /// pairing must report each frame's actual attachment in source
    /// order, not aggregate or fill in.
    #[test]
    fn frames_with_graphic_control_per_frame_independence() {
        let gce_a = GraphicControl {
            disposal: DisposalMethod::Keep,
            user_input: false,
            transparent_index: None,
            delay_centis: 10,
        };
        let gce_b = GraphicControl {
            disposal: DisposalMethod::RestorePrevious,
            user_input: false,
            transparent_index: Some(0),
            delay_centis: 20,
        };
        let mut f_a = frame_with(None);
        f_a.graphic_control = Some(gce_a);
        let f_bare = frame_with(None);
        let mut f_b = frame_with(None);
        f_b.graphic_control = Some(gce_b);
        let img = base_image(
            Some(pal3()),
            vec![Block::Image(f_a), Block::Image(f_bare), Block::Image(f_b)],
        );
        let collected: Vec<_> = img.frames_with_graphic_control().map(|(_, g)| g).collect();
        assert_eq!(collected, vec![Some(gce_a), None, Some(gce_b)]);
    }

    /// Non-Image blocks (§24 Comment, §26 Application, §25 Plain Text)
    /// must be skipped — the iterator's shape mirrors `frames()` and
    /// `frames_with_palette()`.
    #[test]
    fn frames_with_graphic_control_skips_non_image_blocks() {
        let mut f = frame_with(None);
        f.graphic_control = Some(GraphicControl {
            disposal: DisposalMethod::None,
            user_input: false,
            transparent_index: None,
            delay_centis: 7,
        });
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(b"leading".to_vec()),
                Block::Application(Application {
                    identifier: *b"NETSCAPE",
                    auth_code: *b"2.0",
                    data: vec![0x01, 0x00, 0x00],
                }),
                Block::Image(f),
                Block::Comment(b"trailing".to_vec()),
            ],
        );
        // Only one §20 Image block, so exactly one item.
        let collected: Vec<_> = img.frames_with_graphic_control().collect();
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].1.map(|g| g.delay_centis), Some(7));
    }

    /// The yielded `&Frame` reference points at the same frame
    /// `frames()` would yield — the new iterator is a strict extension
    /// of the existing one, matching the `frames_with_palette`
    /// invariant.
    #[test]
    fn frames_with_graphic_control_frame_handle_matches_frames() {
        let mut f1 = frame_with(None);
        f1.graphic_control = Some(GraphicControl {
            disposal: DisposalMethod::Keep,
            user_input: false,
            transparent_index: None,
            delay_centis: 1,
        });
        let f2 = frame_with(Some(pal3_alt()));
        let img = base_image(Some(pal3()), vec![Block::Image(f1), Block::Image(f2)]);
        let lhs: Vec<*const Frame> = img.frames().map(|f| f as *const _).collect();
        let rhs: Vec<*const Frame> = img
            .frames_with_graphic_control()
            .map(|(f, _)| f as *const _)
            .collect();
        assert_eq!(lhs, rhs);
    }

    /// The §23 GCE pairing must agree with the per-frame timing
    /// surfaced by [`GifImage::frame_delays`] for §20 Image blocks. The
    /// pairing carries the full GCE (not just the delay), but the
    /// `delay_centis` field is the source of truth for both accessors.
    #[test]
    fn frames_with_graphic_control_delay_matches_frame_delays() {
        let mut f1 = frame_with(None);
        f1.graphic_control = Some(GraphicControl {
            disposal: DisposalMethod::None,
            user_input: false,
            transparent_index: None,
            delay_centis: 25,
        });
        let f2_no_gce = frame_with(None);
        let mut f3 = frame_with(None);
        f3.graphic_control = Some(GraphicControl {
            disposal: DisposalMethod::Keep,
            user_input: false,
            transparent_index: None,
            delay_centis: 100,
        });
        let img = base_image(
            Some(pal3()),
            vec![Block::Image(f1), Block::Image(f2_no_gce), Block::Image(f3)],
        );
        // No Plain Text blocks, so frame_delays and
        // frames_with_graphic_control align element-for-element.
        let pairs: Vec<Duration> = img
            .frames_with_graphic_control()
            .map(|(_, g)| {
                let centis = g.map(|gc| gc.delay_centis).unwrap_or(0);
                Duration::from_millis(centis as u64 * 10)
            })
            .collect();
        let delays: Vec<Duration> = img.frame_delays().collect();
        assert_eq!(pairs, delays);
    }

    /// `color_resolution_bits()` adds 1 to the raw §18.c.iv field and
    /// covers the full `0..=7` raw range without overflow.
    #[test]
    fn color_resolution_bits_covers_full_field_range() {
        let mut img = base_image(Some(pal3()), vec![]);
        for raw in 0u8..=7 {
            img.color_resolution = raw;
            assert_eq!(img.color_resolution_bits(), raw + 1);
        }
    }

    /// `color_resolution_bits()` masks the raw field to its low 3
    /// bits, so a defensively-set high-bit value still yields a sane
    /// `1..=8` result. (The decoder already masks, but the accessor
    /// is reachable from caller-built `GifImage`s too.)
    #[test]
    fn color_resolution_bits_masks_high_bits() {
        let mut img = base_image(Some(pal3()), vec![]);
        img.color_resolution = 0b1111_0011;
        // Low 3 bits = 3, plus one → 4 bits per primary.
        assert_eq!(img.color_resolution_bits(), 4);
    }

    /// `original_palette_color_count()` follows the §18.c.iv table:
    /// raw `0..=7` maps to `2^(3 * (raw + 1))` distinct colours.
    #[test]
    fn original_palette_color_count_table() {
        let mut img = base_image(Some(pal3()), vec![]);
        let expected = [8u32, 64, 512, 4096, 32_768, 262_144, 2_097_152, 16_777_216];
        for (raw, want) in expected.iter().enumerate() {
            img.color_resolution = raw as u8;
            assert_eq!(img.original_palette_color_count(), *want, "raw={raw}");
        }
    }

    /// `frame_count()` counts §20 Image blocks specifically; §24
    /// Comment, §25 Plain Text, and §26 Application blocks do not
    /// contribute.
    #[test]
    fn frame_count_counts_image_blocks_only() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(b"hi".to_vec()),
                Block::Image(frame_with(None)),
                Block::Image(frame_with(Some(pal3_alt()))),
                Block::PlainText {
                    params: PlainText {
                        left: 0,
                        top: 0,
                        width: 8,
                        height: 8,
                        cell_width: 8,
                        cell_height: 8,
                        fg_color_index: 1,
                        bg_color_index: 0,
                        text: b"hi".to_vec(),
                    },
                    graphic_control: None,
                },
            ],
        );
        assert_eq!(img.frame_count(), 2);
    }

    /// `frame_count() == 0` is reachable for a metadata-only stream
    /// (lenient mode can yield this after scanning past corrupt image
    /// data).
    #[test]
    fn frame_count_zero_for_metadata_only_stream() {
        let img = base_image(Some(pal3()), vec![Block::Comment(b"hi".to_vec())]);
        assert_eq!(img.frame_count(), 0);
    }

    /// Build a §20 Frame whose §20.c.viii Sort Flag (`palette_sorted`)
    /// is set, with the supplied (optional) Local Color Table. Used by
    /// the §18.c.v / §20.c.viii Sort Flag accessor tests.
    fn frame_with_sorted(local: Option<Vec<Rgb>>) -> Frame {
        let mut f = frame_with(local);
        f.palette_sorted = true;
        f
    }

    /// §18.c.v — a Global Color Table whose Sort Flag is set returns
    /// `true` from `has_sorted_global_palette()`.
    #[test]
    fn has_sorted_global_palette_true_when_gct_sort_flag_set() {
        let mut img = base_image(Some(pal3()), vec![]);
        img.global_palette_sorted = true;
        assert!(img.has_sorted_global_palette());
    }

    /// §18.c.iii — when the Global Color Table is absent the §18.c.v
    /// Sort Flag is meaningless. The accessor must short-circuit on
    /// the no-GCT case rather than report a sort guarantee no
    /// palette can honour.
    #[test]
    fn has_sorted_global_palette_false_when_no_gct_even_if_flag_set() {
        let mut img = base_image(None, vec![]);
        img.global_palette_sorted = true;
        assert!(!img.has_sorted_global_palette());
    }

    /// Default no-Sort-Flag GCT reports `false` so a renderer never
    /// truncates a palette whose order has not been declared important.
    #[test]
    fn has_sorted_global_palette_false_when_flag_clear() {
        let img = base_image(Some(pal3()), vec![]);
        assert!(!img.has_sorted_global_palette());
    }

    /// `frames_with_sorted_palette` follows the §21.a precedence: a
    /// frame with a Local Color Table whose §20.c.viii Sort Flag is
    /// set reports the LCT's sorted state, not the GCT's.
    #[test]
    fn frames_with_sorted_palette_prefers_lct_sort_flag() {
        let mut img = base_image(
            Some(pal3()),
            vec![Block::Image(frame_with_sorted(Some(pal3_alt())))],
        );
        // GCT Sort Flag clear; the frame's LCT Sort Flag is set.
        img.global_palette_sorted = false;
        let collected: Vec<_> = img
            .frames_with_sorted_palette()
            .map(|(_, p, sorted)| (p.map(<[Rgb]>::to_vec), sorted))
            .collect();
        assert_eq!(collected, vec![(Some(pal3_alt()), true)]);
    }

    /// When the LCT flag is clear the §18 GCT applies and its §18.c.v
    /// Sort Flag is what `frames_with_sorted_palette` reports.
    #[test]
    fn frames_with_sorted_palette_falls_back_to_gct_sort_flag() {
        let mut img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)),
                Block::Image(frame_with(None)),
            ],
        );
        img.global_palette_sorted = true;
        let collected: Vec<_> = img
            .frames_with_sorted_palette()
            .map(|(_, p, sorted)| (p.map(<[Rgb]>::to_vec), sorted))
            .collect();
        assert_eq!(collected, vec![(Some(pal3()), true), (Some(pal3()), true)]);
    }

    /// §13 / §21 — no active table → no sort guarantee. The yielded
    /// palette is `None` and the bool is `false`.
    #[test]
    fn frames_with_sorted_palette_no_table_is_unsorted() {
        let img = base_image(None, vec![Block::Image(frame_with(None))]);
        let collected: Vec<_> = img
            .frames_with_sorted_palette()
            .map(|(_, p, sorted)| (p.map(<[Rgb]>::to_vec), sorted))
            .collect();
        assert_eq!(collected, vec![(None, false)]);
    }

    /// A frame whose LCT Sort Flag is clear must report unsorted even
    /// when the §18 GCT happens to be sorted — the LCT is the active
    /// table per §21.a, so its Sort Flag is the only one in scope.
    #[test]
    fn frames_with_sorted_palette_lct_clear_overrides_sorted_gct() {
        let mut img = base_image(
            Some(pal3()),
            vec![Block::Image(frame_with(Some(pal3_alt())))],
        );
        img.global_palette_sorted = true;
        // Frame has an LCT but its palette_sorted field is `false` by
        // default — the active LCT is *not* sorted.
        let collected: Vec<_> = img
            .frames_with_sorted_palette()
            .map(|(_, _, sorted)| sorted)
            .collect();
        assert_eq!(collected, vec![false]);
    }

    /// Non-Image blocks (§24 Comment, §25 Plain Text, §26 Application)
    /// are skipped by `frames_with_sorted_palette` — only §20 Image
    /// Descriptors are paired with a palette + sort flag, matching
    /// `frames()` and `frames_with_palette()`.
    #[test]
    fn frames_with_sorted_palette_skips_non_image_blocks() {
        let mut img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(b"first".to_vec()),
                Block::Image(frame_with(None)),
                Block::Image(frame_with_sorted(Some(pal3_alt()))),
                Block::Comment(b"last".to_vec()),
            ],
        );
        img.global_palette_sorted = true;
        let collected: Vec<_> = img
            .frames_with_sorted_palette()
            .map(|(_, p, sorted)| (p.map(<[Rgb]>::to_vec), sorted))
            .collect();
        assert_eq!(
            collected,
            vec![(Some(pal3()), true), (Some(pal3_alt()), true)]
        );
    }

    /// `all_frames_palettes_sorted()` short-circuits to `true` when
    /// every frame's active palette is sorted — by LCT or by GCT
    /// fallback under §21.a.
    #[test]
    fn all_frames_palettes_sorted_true_for_mixed_lct_gct() {
        let mut img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)),                    // GCT-rendered
                Block::Image(frame_with_sorted(Some(pal3_alt()))), // LCT-rendered
            ],
        );
        img.global_palette_sorted = true;
        assert!(img.all_frames_palettes_sorted());
    }

    /// A single unsorted active palette is enough to flip
    /// `all_frames_palettes_sorted()` to `false`.
    #[test]
    fn all_frames_palettes_sorted_false_when_any_frame_unsorted() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)), // GCT-rendered, sort flag clear
                Block::Image(frame_with_sorted(Some(pal3_alt()))),
            ],
        );
        // GCT Sort Flag stays clear → the first frame renders against
        // an unsorted table.
        assert!(!img.all_frames_palettes_sorted());
    }

    /// Vacuous-truth: a zero-frame stream reports
    /// `all_frames_palettes_sorted() == true` per `Iterator::all`'s
    /// empty-input contract.
    #[test]
    fn all_frames_palettes_sorted_true_for_zero_frame_stream() {
        let img = base_image(Some(pal3()), vec![Block::Comment(b"hi".to_vec())]);
        assert!(img.all_frames_palettes_sorted());
    }

    /// A frame with no active palette at all (§13 / §21 fallback) must
    /// flip `all_frames_palettes_sorted()` to `false`: no table means
    /// no sort guarantee is available to honour.
    #[test]
    fn all_frames_palettes_sorted_false_when_no_active_palette() {
        let img = base_image(None, vec![Block::Image(frame_with(None))]);
        assert!(!img.all_frames_palettes_sorted());
    }

    /// `frames_with_sorted_palette` mirrors `frames_with_palette` on
    /// the (frame, palette) pair — the new iterator is a strict
    /// extension, adding only the §18.c.v / §20.c.viii Sort Flag bit.
    #[test]
    fn frames_with_sorted_palette_extends_frames_with_palette() {
        let mut img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_sorted(Some(pal3_alt()))),
                Block::Image(frame_with(None)),
            ],
        );
        img.global_palette_sorted = true;
        let with_palette: Vec<_> = img
            .frames_with_palette()
            .map(|(f, p)| (f as *const _, p.map(<[Rgb]>::to_vec)))
            .collect();
        let with_sorted: Vec<_> = img
            .frames_with_sorted_palette()
            .map(|(f, p, _)| (f as *const _, p.map(<[Rgb]>::to_vec)))
            .collect();
        assert_eq!(with_palette, with_sorted);
    }

    // ---- §20.c.vii Interlace Flag stream-level accessors ----

    /// Build a §20 Frame whose §20.c.vii Interlace Flag is set.
    fn frame_with_interlaced(local: Option<Vec<Rgb>>) -> Frame {
        let mut f = frame_with(local);
        f.interlaced = true;
        f
    }

    /// `interlaced_frame_count()` counts only §20 Image blocks whose
    /// Interlace Flag is set; non-interlaced images and non-§20 blocks
    /// (Comment / Plain Text / Application) do not contribute.
    #[test]
    fn interlaced_frame_count_counts_set_flag_image_blocks_only() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(b"hi".to_vec()),
                Block::Image(frame_with_interlaced(None)),
                Block::Image(frame_with(None)),
                Block::Image(frame_with_interlaced(Some(pal3_alt()))),
                Block::PlainText {
                    params: PlainText {
                        left: 0,
                        top: 0,
                        width: 8,
                        height: 8,
                        cell_width: 8,
                        cell_height: 8,
                        fg_color_index: 1,
                        bg_color_index: 0,
                        text: b"hi".to_vec(),
                    },
                    graphic_control: None,
                },
            ],
        );
        assert_eq!(img.interlaced_frame_count(), 2);
    }

    /// `interlaced_frame_count() == 0` when the stream has no §20
    /// Images at all (metadata-only).
    #[test]
    fn interlaced_frame_count_zero_for_metadata_only_stream() {
        let img = base_image(Some(pal3()), vec![Block::Comment(b"hi".to_vec())]);
        assert_eq!(img.interlaced_frame_count(), 0);
    }

    /// `interlaced_frame_count() == 0` when every §20 Image leaves the
    /// Interlace Flag clear.
    #[test]
    fn interlaced_frame_count_zero_when_all_frames_progressive() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)),
                Block::Image(frame_with(None)),
            ],
        );
        assert_eq!(img.interlaced_frame_count(), 0);
    }

    /// `has_interlaced_frames()` flips to `true` as soon as one §20
    /// Image carries the Interlace Flag set.
    #[test]
    fn has_interlaced_frames_true_when_any_frame_interlaced() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)),
                Block::Image(frame_with_interlaced(None)),
            ],
        );
        assert!(img.has_interlaced_frames());
    }

    /// `has_interlaced_frames()` is `false` for a stream with no §20
    /// Images at all, matching the stream-level no-frame contract.
    #[test]
    fn has_interlaced_frames_false_for_metadata_only_stream() {
        let img = base_image(Some(pal3()), vec![Block::Comment(b"hi".to_vec())]);
        assert!(!img.has_interlaced_frames());
    }

    /// `has_interlaced_frames()` is `false` when every §20 Image leaves
    /// the Interlace Flag clear (the common case for modern still
    /// images).
    #[test]
    fn has_interlaced_frames_false_when_no_frame_interlaced() {
        let img = base_image(Some(pal3()), vec![Block::Image(frame_with(None))]);
        assert!(!img.has_interlaced_frames());
    }

    /// `all_frames_interlaced()` is `true` when every §20 Image has the
    /// flag set.
    #[test]
    fn all_frames_interlaced_true_when_every_frame_interlaced() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_interlaced(None)),
                Block::Image(frame_with_interlaced(Some(pal3_alt()))),
            ],
        );
        assert!(img.all_frames_interlaced());
    }

    /// `all_frames_interlaced()` is `false` if any §20 Image leaves the
    /// flag clear.
    #[test]
    fn all_frames_interlaced_false_when_any_frame_progressive() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with_interlaced(None)),
                Block::Image(frame_with(None)),
            ],
        );
        assert!(!img.all_frames_interlaced());
    }

    /// Vacuous-truth: a zero-frame stream reports
    /// `all_frames_interlaced() == true` per `Iterator::all`'s
    /// empty-input contract — mirrors `all_frames_palettes_sorted()`.
    #[test]
    fn all_frames_interlaced_true_for_zero_frame_stream() {
        let img = base_image(Some(pal3()), vec![Block::Comment(b"hi".to_vec())]);
        assert!(img.all_frames_interlaced());
        assert_eq!(img.frame_count(), 0);
    }

    /// Non-image blocks (§24 Comment / §25 Plain Text / §26 Application)
    /// never count toward the §20.c.vii roll-up. Mixing them in around
    /// interlaced images must not flip the all-frames query.
    #[test]
    fn interlace_accessors_skip_non_image_blocks() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(b"a".to_vec()),
                Block::Image(frame_with_interlaced(None)),
                Block::PlainText {
                    params: PlainText {
                        left: 0,
                        top: 0,
                        width: 8,
                        height: 8,
                        cell_width: 8,
                        cell_height: 8,
                        fg_color_index: 1,
                        bg_color_index: 0,
                        text: b"x".to_vec(),
                    },
                    graphic_control: None,
                },
                Block::Image(frame_with_interlaced(Some(pal3_alt()))),
            ],
        );
        assert_eq!(img.interlaced_frame_count(), 2);
        assert!(img.has_interlaced_frames());
        assert!(img.all_frames_interlaced());
    }

    // ---- §20.c.ix Size of Local Color Table stream-level accessors ----

    /// Build a palette of `n` distinct entries for the §20.c.ix tests.
    fn pal_n(n: usize) -> Vec<Rgb> {
        (0..n)
            .map(|i| {
                let v = (i & 0xFF) as u8;
                Rgb::new(v, v.wrapping_add(1), v.wrapping_add(2))
            })
            .collect()
    }

    /// `Frame::local_color_table_size_field()` returns `None` when the
    /// frame carries no Local Color Table (§20.c.vi flag clear, §20.c.ix
    /// undefined).
    #[test]
    fn local_color_table_size_field_none_without_lct() {
        let f = frame_with(None);
        assert_eq!(f.local_color_table_size_field(), None);
        assert_eq!(f.local_color_table_entry_count(), None);
    }

    /// §20.c.ix field encoding pins: every length in `1..=256` rounds
    /// up to the smallest `2^(N+1)` slot, matching the §18.c.vi / §20.c.ix
    /// encoder rule. 1-entry LCT slots into the 2-entry field (N=0);
    /// 256-entry LCT pins the field at the maximum N=7.
    #[test]
    fn local_color_table_size_field_round_up_table() {
        // (palette length, expected field, expected entry count).
        let cases: &[(usize, u8, u32)] = &[
            (1, 0, 2),
            (2, 0, 2),
            (3, 1, 4),
            (4, 1, 4),
            (5, 2, 8),
            (8, 2, 8),
            (9, 3, 16),
            (16, 3, 16),
            (17, 4, 32),
            (32, 4, 32),
            (33, 5, 64),
            (64, 5, 64),
            (65, 6, 128),
            (128, 6, 128),
            (129, 7, 256),
            (256, 7, 256),
        ];
        for &(len, expected_field, expected_count) in cases {
            let f = frame_with(Some(pal_n(len)));
            assert_eq!(
                f.local_color_table_size_field(),
                Some(expected_field),
                "len={len}"
            );
            assert_eq!(
                f.local_color_table_entry_count(),
                Some(expected_count),
                "len={len}"
            );
        }
    }

    /// `frames_with_local_color_table_size()` yields one entry per §20
    /// Image block, in source order, paired with its §20.c.ix field. §24
    /// Comment / §25 Plain Text / §26 Application contribute nothing.
    #[test]
    fn frames_with_local_color_table_size_pairs_source_order() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(b"hdr".to_vec()),
                // LCT of 5 entries → rounds up to 8 (N=2).
                Block::Image(frame_with(Some(pal_n(5)))),
                // No LCT — yields None.
                Block::Image(frame_with(None)),
                Block::PlainText {
                    params: PlainText {
                        left: 0,
                        top: 0,
                        width: 8,
                        height: 8,
                        cell_width: 8,
                        cell_height: 8,
                        fg_color_index: 1,
                        bg_color_index: 0,
                        text: b"x".to_vec(),
                    },
                    graphic_control: None,
                },
                // LCT of 256 entries → pins the field at N=7.
                Block::Image(frame_with(Some(pal_n(256)))),
            ],
        );
        let fields: Vec<Option<u8>> = img
            .frames_with_local_color_table_size()
            .map(|(_, n)| n)
            .collect();
        assert_eq!(fields, vec![Some(2), None, Some(7)]);
    }

    /// `frames_with_local_color_table_entry_count()` is the entry-count
    /// companion: `Some(2^(N+1))` when an LCT is attached, `None` otherwise.
    #[test]
    fn frames_with_local_color_table_entry_count_matches_size_field() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(Some(pal_n(5)))),
                Block::Image(frame_with(None)),
                Block::Image(frame_with(Some(pal_n(256)))),
            ],
        );
        let counts: Vec<Option<u32>> = img
            .frames_with_local_color_table_entry_count()
            .map(|(_, c)| c)
            .collect();
        assert_eq!(counts, vec![Some(8), None, Some(256)]);
    }

    /// `max_local_color_table_size_field()` returns `None` when no §20
    /// Image in the stream attaches an LCT (every frame's §20.c.vi flag
    /// is clear) — the stream uses only the §18 Global Color Table.
    #[test]
    fn max_local_color_table_size_field_none_when_no_lcts() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)),
                Block::Image(frame_with(None)),
            ],
        );
        assert_eq!(img.max_local_color_table_size_field(), None);
    }

    /// `max_local_color_table_size_field()` returns `None` for a stream
    /// with no §20 Image blocks at all (metadata-only).
    #[test]
    fn max_local_color_table_size_field_none_for_metadata_only_stream() {
        let img = base_image(Some(pal3()), vec![Block::Comment(b"hi".to_vec())]);
        assert_eq!(img.max_local_color_table_size_field(), None);
    }

    /// `max_local_color_table_size_field()` reports the largest §20.c.ix
    /// field across every LCT-carrying §20 Image. Frames without an LCT
    /// are skipped, not folded as `0`.
    #[test]
    fn max_local_color_table_size_field_picks_largest() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(Some(pal_n(2)))),  // field 0
                Block::Image(frame_with(None)),            // skipped
                Block::Image(frame_with(Some(pal_n(17)))), // field 4
                Block::Image(frame_with(Some(pal_n(5)))),  // field 2
            ],
        );
        assert_eq!(img.max_local_color_table_size_field(), Some(4));
    }

    /// `max_local_color_table_size_field()` does not consider §25 Plain
    /// Text or §24 Comment / §26 Application blocks (they carry no
    /// §20.c.ix at all).
    #[test]
    fn max_local_color_table_size_field_ignores_non_image_blocks() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(b"a".to_vec()),
                Block::Image(frame_with(Some(pal_n(8)))), // field 2
                Block::PlainText {
                    params: PlainText {
                        left: 0,
                        top: 0,
                        width: 8,
                        height: 8,
                        cell_width: 8,
                        cell_height: 8,
                        fg_color_index: 1,
                        bg_color_index: 0,
                        text: b"x".to_vec(),
                    },
                    graphic_control: None,
                },
                Block::Application(Application {
                    identifier: *b"NETSCAPE",
                    auth_code: *b"2.0",
                    data: vec![1, 0, 0, 0],
                }),
            ],
        );
        assert_eq!(img.max_local_color_table_size_field(), Some(2));
    }

    /// Round-trip pin: `entry_count == 1 << (size_field + 1)` for every
    /// LCT-carrying frame the accessors surface.
    #[test]
    fn local_color_table_size_field_and_entry_count_consistency() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(Some(pal_n(1)))),
                Block::Image(frame_with(Some(pal_n(3)))),
                Block::Image(frame_with(Some(pal_n(33)))),
                Block::Image(frame_with(Some(pal_n(256)))),
                Block::Image(frame_with(None)),
            ],
        );
        for (f, field) in img.frames_with_local_color_table_size() {
            let count = f.local_color_table_entry_count();
            match (field, count) {
                (Some(n), Some(c)) => assert_eq!(c, 1u32 << (n as u32 + 1)),
                (None, None) => {}
                pair => panic!("field/count out of sync: {pair:?}"),
            }
        }
    }

    // ---- §25 Plain Text typed accessor + §25.e conformance queries ----

    fn plain_text_block(
        width: u16,
        height: u16,
        cell_width: u8,
        cell_height: u8,
        text: Vec<u8>,
    ) -> Block {
        Block::PlainText {
            params: PlainText {
                left: 0,
                top: 0,
                width,
                height,
                cell_width,
                cell_height,
                fg_color_index: 1,
                bg_color_index: 0,
                text,
            },
            graphic_control: None,
        }
    }

    /// `plain_texts()` yields each §25 block paired with its attached
    /// §23 GCE (when one is present) in source order, skipping every
    /// non-PlainText block.
    #[test]
    fn plain_texts_pairs_with_attached_graphic_control() {
        let gce = GraphicControl {
            disposal: DisposalMethod::None,
            user_input: false,
            transparent_index: Some(7),
            delay_centis: 5,
        };
        let pt_with_gce = Block::PlainText {
            params: PlainText {
                left: 1,
                top: 2,
                width: 8,
                height: 8,
                cell_width: 8,
                cell_height: 8,
                fg_color_index: 1,
                bg_color_index: 0,
                text: b"HELLO".to_vec(),
            },
            graphic_control: Some(gce),
        };
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Comment(b"prelude".to_vec()),
                Block::Image(frame_with(None)),
                pt_with_gce,
                plain_text_block(8, 8, 8, 8, b"WORLD".to_vec()),
            ],
        );
        let collected: Vec<_> = img
            .plain_texts()
            .map(|(pt, g)| (pt.text.clone(), g))
            .collect();
        assert_eq!(
            collected,
            vec![(b"HELLO".to_vec(), Some(gce)), (b"WORLD".to_vec(), None)]
        );
    }

    /// A stream with no §25 Plain Text blocks yields nothing.
    #[test]
    fn plain_texts_empty_when_no_plain_text_blocks() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)),
                Block::Comment(b"x".to_vec()),
            ],
        );
        assert_eq!(img.plain_texts().count(), 0);
    }

    /// §25.e printable check: every byte inside `0x20..=0xF7` passes;
    /// a single byte outside that range flips the query to `false`.
    #[test]
    fn plain_texts_printable_check_boundaries() {
        // Range edges: 0x20 (space) and 0xF7 are inclusive.
        let ok = base_image(
            Some(pal3()),
            vec![plain_text_block(
                8,
                8,
                8,
                8,
                vec![0x20, b'A', b'~', 0x7F, 0xA0, 0xF7],
            )],
        );
        assert!(ok.plain_texts_are_printable());

        // 0x1F is below the range → fails.
        let too_low = base_image(Some(pal3()), vec![plain_text_block(8, 8, 8, 8, vec![0x1F])]);
        assert!(!too_low.plain_texts_are_printable());

        // 0xF8 is just above the range → fails.
        let too_high = base_image(Some(pal3()), vec![plain_text_block(8, 8, 8, 8, vec![0xF8])]);
        assert!(!too_high.plain_texts_are_printable());
    }

    /// A zero-Plain-Text stream trivially conforms to §25.e printable
    /// recommendation.
    #[test]
    fn plain_texts_printable_vacuous_true() {
        let img = base_image(Some(pal3()), vec![Block::Image(frame_with(None))]);
        assert!(img.plain_texts_are_printable());
    }

    /// §25.e grid-fits-cells: an 80×16 grid of 8×8 cells produces an
    /// integer 10×2 cell count and conforms.
    #[test]
    fn plain_texts_grid_fits_cells_integer_count() {
        let img = base_image(
            Some(pal3()),
            vec![plain_text_block(80, 16, 8, 8, b"hi".to_vec())],
        );
        assert!(img.plain_texts_grid_fits_cells());
    }

    /// A grid whose `width % cell_width != 0` (here 81 / 8) is exactly
    /// the §25.e "fractional cells must be discarded" failure case.
    #[test]
    fn plain_texts_grid_fits_cells_rejects_fractional_width() {
        let img = base_image(
            Some(pal3()),
            vec![plain_text_block(81, 16, 8, 8, b"hi".to_vec())],
        );
        assert!(!img.plain_texts_grid_fits_cells());
    }

    /// A grid whose `height % cell_height != 0` (here 17 / 8) also
    /// fails the §25.e integer-cell check.
    #[test]
    fn plain_texts_grid_fits_cells_rejects_fractional_height() {
        let img = base_image(
            Some(pal3()),
            vec![plain_text_block(80, 17, 8, 8, b"hi".to_vec())],
        );
        assert!(!img.plain_texts_grid_fits_cells());
    }

    /// A block whose cell dimensions are zero collapses the grid
    /// layout entirely — we refuse it as non-conforming rather than
    /// dividing by zero.
    #[test]
    fn plain_texts_grid_fits_cells_rejects_zero_cell_dimension() {
        let zero_w = base_image(
            Some(pal3()),
            vec![plain_text_block(80, 16, 0, 8, b"hi".to_vec())],
        );
        assert!(!zero_w.plain_texts_grid_fits_cells());

        let zero_h = base_image(
            Some(pal3()),
            vec![plain_text_block(80, 16, 8, 0, b"hi".to_vec())],
        );
        assert!(!zero_h.plain_texts_grid_fits_cells());
    }

    /// A zero-Plain-Text stream trivially conforms to §25.e
    /// integer-cell recommendation.
    #[test]
    fn plain_texts_grid_fits_cells_vacuous_true() {
        let img = base_image(Some(pal3()), vec![Block::Image(frame_with(None))]);
        assert!(img.plain_texts_grid_fits_cells());
    }

    // ---- §20.a / §25.a "must fit within the boundaries of the
    //      Logical Screen" validation accessors ----

    /// A §20 Image at `(left, top)` with `width × height`, filled with
    /// index 0 so the indices buffer matches the declared rectangle.
    fn placed_frame(left: u16, top: u16, width: u16, height: u16) -> Block {
        Block::Image(Frame {
            left,
            top,
            width,
            height,
            local_palette: None,
            palette_sorted: false,
            interlaced: false,
            indices: vec![0; width as usize * height as usize],
            graphic_control: None,
        })
    }

    /// A §25 Plain Text grid at `(left, top)` with `width × height`.
    fn placed_plain_text(left: u16, top: u16, width: u16, height: u16) -> Block {
        Block::PlainText {
            params: PlainText {
                left,
                top,
                width,
                height,
                cell_width: 8,
                cell_height: 8,
                fg_color_index: 1,
                bg_color_index: 0,
                text: b"x".to_vec(),
            },
            graphic_control: None,
        }
    }

    /// A `GifImage` with an arbitrary §18 Logical Screen size.
    fn screen_image(screen_width: u16, screen_height: u16, blocks: Vec<Block>) -> GifImage {
        GifImage {
            version: Version::Gif89a,
            screen_width,
            screen_height,
            color_resolution: 1,
            global_palette_sorted: false,
            background_index: 0,
            pixel_aspect_ratio: 0,
            global_palette: Some(pal3()),
            blocks,
        }
    }

    /// Every §20 Image fits inside the §18 Logical Screen (a frame flush
    /// to the far corner counts as fitting — the `<=` boundary).
    #[test]
    fn all_blocks_fit_screen_accepts_in_bounds_frames() {
        let img = screen_image(
            16,
            16,
            vec![placed_frame(0, 0, 8, 8), placed_frame(8, 8, 8, 8)],
        );
        assert!(img.all_blocks_fit_screen());
        assert_eq!(img.out_of_bounds_block_count(), 0);
    }

    /// A frame whose right edge exceeds the §18.b Logical Screen Width
    /// fails §20.a.
    #[test]
    fn all_blocks_fit_screen_rejects_right_overflow() {
        let img = screen_image(16, 16, vec![placed_frame(10, 0, 8, 8)]);
        assert!(!img.all_blocks_fit_screen());
        assert_eq!(img.out_of_bounds_block_count(), 1);
    }

    /// A frame whose bottom edge exceeds the §18.b Logical Screen Height
    /// fails §20.a.
    #[test]
    fn all_blocks_fit_screen_rejects_bottom_overflow() {
        let img = screen_image(16, 16, vec![placed_frame(0, 10, 8, 8)]);
        assert!(!img.all_blocks_fit_screen());
        assert_eq!(img.out_of_bounds_block_count(), 1);
    }

    /// §25.a applies the same boundary constraint to the Plain Text Text
    /// Grid; an out-of-bounds grid is counted exactly like an image.
    #[test]
    fn all_blocks_fit_screen_covers_plain_text_grid() {
        let img = screen_image(16, 16, vec![placed_plain_text(12, 12, 8, 8)]);
        assert!(!img.all_blocks_fit_screen());
        assert_eq!(img.out_of_bounds_block_count(), 1);
    }

    /// The count tallies *each* escaping graphic-rendering block; §24
    /// Comment and §26 Application Extensions never contribute (they
    /// have no placement).
    #[test]
    fn out_of_bounds_block_count_tallies_each_escaping_block() {
        let img = screen_image(
            16,
            16,
            vec![
                placed_frame(0, 0, 8, 8),         // fits
                placed_frame(10, 0, 8, 8),        // escapes (right)
                placed_plain_text(0, 10, 8, 8),   // escapes (bottom)
                Block::Comment(b"note".to_vec()), // no placement
            ],
        );
        assert!(!img.all_blocks_fit_screen());
        assert_eq!(img.out_of_bounds_block_count(), 2);
    }

    /// A coordinate sum near the `u16` ceiling must not wrap: a frame at
    /// `left = 65_535` with `width = 1` has a right edge of 65_536,
    /// which exceeds any `u16` screen width and must be reported as
    /// escaping rather than wrapping to 0.
    #[test]
    fn all_blocks_fit_screen_no_u16_wrap_at_ceiling() {
        let img = screen_image(u16::MAX, u16::MAX, vec![placed_frame(u16::MAX, 0, 1, 1)]);
        assert!(!img.all_blocks_fit_screen());
        assert_eq!(img.out_of_bounds_block_count(), 1);
    }

    /// A stream with no graphic-rendering block trivially conforms,
    /// matching the vacuous-true shape of the surrounding rollups.
    #[test]
    fn all_blocks_fit_screen_vacuous_true() {
        let img = screen_image(1, 1, vec![Block::Comment(b"only metadata".to_vec())]);
        assert!(img.all_blocks_fit_screen());
        assert_eq!(img.out_of_bounds_block_count(), 0);
    }

    /// The boolean and the count agree: `all_blocks_fit_screen()` is
    /// `true` exactly when `out_of_bounds_block_count()` is `0`.
    #[test]
    fn all_blocks_fit_screen_is_zero_count() {
        let fits = screen_image(16, 16, vec![placed_frame(0, 0, 16, 16)]);
        assert_eq!(
            fits.all_blocks_fit_screen(),
            fits.out_of_bounds_block_count() == 0
        );

        let escapes = screen_image(16, 16, vec![placed_frame(1, 0, 16, 16)]);
        assert_eq!(
            escapes.all_blocks_fit_screen(),
            escapes.out_of_bounds_block_count() == 0
        );
    }

    /// `plain_texts()` is consistent with the existing
    /// `frame_delays()` / GCE spine: a Plain Text block with a GCE
    /// contributes the GCE's delay, and the typed iterator surfaces
    /// the same GCE.
    #[test]
    fn plain_texts_gce_pairing_matches_frame_delays() {
        let gce = GraphicControl {
            disposal: DisposalMethod::None,
            user_input: false,
            transparent_index: None,
            delay_centis: 13,
        };
        let img = base_image(
            Some(pal3()),
            vec![Block::PlainText {
                params: PlainText {
                    left: 0,
                    top: 0,
                    width: 8,
                    height: 8,
                    cell_width: 8,
                    cell_height: 8,
                    fg_color_index: 1,
                    bg_color_index: 0,
                    text: b"X".to_vec(),
                },
                graphic_control: Some(gce),
            }],
        );
        let (_, paired_gce) = img.plain_texts().next().unwrap();
        assert_eq!(paired_gce, Some(gce));
        let delay = img.frame_delays().next().unwrap();
        assert_eq!(delay, Duration::from_millis(130));
    }

    fn pt_block() -> Block {
        plain_text_block(8, 8, 8, 8, b"hi".to_vec())
    }

    fn app_block() -> Block {
        Block::Application(Application {
            identifier: *b"NETSCAPE",
            auth_code: *b"2.0",
            data: vec![0x01, 0x00, 0x00],
        })
    }

    /// §12 classification: §20 Image and §25 Plain Text are
    /// Graphic-Rendering; §24 Comment and §26 Application are
    /// Special-Purpose.
    #[test]
    fn block_class_matches_section_12_taxonomy() {
        assert_eq!(
            Block::Image(frame_with(None)).class(),
            BlockClass::GraphicRendering
        );
        assert_eq!(pt_block().class(), BlockClass::GraphicRendering);
        assert_eq!(
            Block::Comment(b"c".to_vec()).class(),
            BlockClass::SpecialPurpose
        );
        assert_eq!(app_block().class(), BlockClass::SpecialPurpose);

        assert!(Block::Image(frame_with(None)).is_graphic_rendering());
        assert!(pt_block().is_graphic_rendering());
        assert!(!Block::Image(frame_with(None)).is_special_purpose());

        assert!(Block::Comment(b"c".to_vec()).is_special_purpose());
        assert!(app_block().is_special_purpose());
        assert!(!app_block().is_graphic_rendering());
    }

    /// §12 stream-level rollups: graphic-rendering counts §20 + §25,
    /// special-purpose counts §24 + §26.
    #[test]
    fn block_class_rollups_partition_the_stream() {
        let img = base_image(
            Some(pal3()),
            vec![
                Block::Image(frame_with(None)),
                pt_block(),
                Block::Comment(b"hello".to_vec()),
                app_block(),
            ],
        );
        // §20 Image + §25 Plain Text are graphic-rendering...
        assert_eq!(img.graphic_rendering_block_count(), 2);
        // ...while frame_count is §20 Images only.
        assert_eq!(img.frame_count(), 1);
        // §24 Comment + §26 Application are special-purpose.
        assert_eq!(img.special_purpose_block_count(), 2);
        // Every block is partitioned into exactly one of the two
        // surfaced classes (no free-standing Control block exists).
        assert_eq!(
            img.graphic_rendering_block_count() + img.special_purpose_block_count(),
            img.blocks.len()
        );
    }

    /// §11 palette-loader: a GCT-bearing stream with no
    /// graphic-rendering block is a table-install Data Stream;
    /// §12-transparent Comment / Application blocks do not disqualify
    /// it.
    #[test]
    fn palette_loader_stream_section_11() {
        // GCT + Trailer only (no blocks) — the canonical §11 loader.
        let loader = base_image(Some(pal3()), vec![]);
        assert!(loader.is_palette_loader_stream());

        // GCT + only special-purpose blocks — still a loader (§12
        // "transparent to the decoding process").
        let loader_with_meta = base_image(
            Some(pal3()),
            vec![Block::Comment(b"x".to_vec()), app_block()],
        );
        assert!(loader_with_meta.is_palette_loader_stream());

        // A graphic-rendering block disqualifies it.
        let with_image = base_image(Some(pal3()), vec![Block::Image(frame_with(None))]);
        assert!(!with_image.is_palette_loader_stream());
        let with_text = base_image(Some(pal3()), vec![pt_block()]);
        assert!(!with_text.is_palette_loader_stream());

        // No GCT — not a loader even with zero frames.
        let no_gct = base_image(None, vec![]);
        assert!(!no_gct.is_palette_loader_stream());
    }
}
