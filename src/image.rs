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
}
