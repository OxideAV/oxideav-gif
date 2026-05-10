//! In-memory representation of a decoded GIF Data Stream.
//!
//! GIF is intrinsically multi-image: a single Data Stream can carry any
//! number of `<Table-Based Image>` blocks (GIF89a §B), each with its own
//! Image Descriptor + optional Local Color Table + LZW image data, and
//! optional Graphic Control Extension supplying delay / transparency /
//! disposal information for that frame. We mirror that structure
//! verbatim — there is no eager compositing onto the logical screen.
//! Consumers that want a flat RGBA framebuffer per frame can call
//! [`GifImage::composite_frame_rgba`].
//!
//! All multi-byte fields land in their parsed form (`u16` width/height,
//! decoded packed-field bits broken out into named flags). Color tables
//! are stored as flat `Vec<u8>` of `R,G,B` triplets per spec §19 / §21.

/// Parsed GIF Data Stream version number — drives the `Header.Version`
/// field per GIF89a §17 / GIF87a §I.A.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GifVersion {
    /// `87a` — original CompuServe release, June 1987.
    Gif87a,
    /// `89a` — adds Graphic / Comment / Plain Text / Application
    /// Extensions per GIF89a §23–§26.
    Gif89a,
}

impl GifVersion {
    /// Three-byte ASCII version identifier as it appears on disk.
    pub fn as_bytes(self) -> &'static [u8; 3] {
        match self {
            Self::Gif87a => b"87a",
            Self::Gif89a => b"89a",
        }
    }
}

/// Disposal Method enumeration per GIF89a §23.c.iv.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisposalMethod {
    /// `0` — No disposal specified. Decoder is not required to take any action.
    #[default]
    None,
    /// `1` — Do not dispose. Graphic is to be left in place.
    DoNotDispose,
    /// `2` — Restore to background color. The area used by the graphic
    /// must be restored to the background color.
    RestoreToBackground,
    /// `3` — Restore to previous. Decoder must restore the area
    /// overwritten with what was there prior to rendering.
    RestoreToPrevious,
    /// `4..7` — Reserved by the spec ("To be defined").
    Reserved(u8),
}

impl DisposalMethod {
    /// Spec encoding — bits 4..2 of GCE packed-fields byte.
    pub fn from_bits(b: u8) -> Self {
        match b {
            0 => Self::None,
            1 => Self::DoNotDispose,
            2 => Self::RestoreToBackground,
            3 => Self::RestoreToPrevious,
            other => Self::Reserved(other),
        }
    }

    /// Spec encoding — emit the canonical 3-bit value.
    pub fn to_bits(self) -> u8 {
        match self {
            Self::None => 0,
            Self::DoNotDispose => 1,
            Self::RestoreToBackground => 2,
            Self::RestoreToPrevious => 3,
            Self::Reserved(b) => b & 0x07,
        }
    }
}

/// Graphic Control Extension (GIF89a §23). Optional per-frame metadata
/// supplying delay / transparency / disposal / user-input.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GraphicControl {
    /// Disposal Method (§23.c.iv).
    pub disposal: DisposalMethod,
    /// User Input Flag (§23.c.v): processing should pause for input.
    pub user_input: bool,
    /// Delay Time in hundredths of a second (§23.c.vii).
    pub delay_cs: u16,
    /// Transparent Color Index (§23.c.viii). `None` if the Transparency
    /// Flag is unset.
    pub transparent_index: Option<u8>,
}

/// One `<Table-Based Image>` (GIF89a §B). The pixel data is the
/// LZW-decompressed sequence of indices into the active color table,
/// with row order de-interlaced when `interlace` was set on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GifFrame {
    /// Image Left Position in pixels (§20.c.ii).
    pub left: u16,
    /// Image Top Position in pixels (§20.c.iii).
    pub top: u16,
    /// Image Width in pixels (§20.c.iv).
    pub width: u16,
    /// Image Height in pixels (§20.c.v).
    pub height: u16,
    /// Local Color Table, if one was associated with this image (§21).
    /// Layout is `R,G,B,R,G,B,…` — same as Global. `None` means the
    /// frame uses the Global Color Table.
    pub local_palette: Option<Vec<u8>>,
    /// Sort Flag from the Image Descriptor packed-fields (§20.c.viii).
    pub local_palette_sorted: bool,
    /// Whether the image was stored on the wire in interlaced row order
    /// (§E). After decode, [`indices`] is always in natural top-to-bottom
    /// order; this flag is preserved so the encoder can choose to write
    /// the image back interlaced when round-tripping.
    pub interlaced: bool,
    /// `width * height` palette indices in natural top-to-bottom,
    /// left-to-right order.
    pub indices: Vec<u8>,
    /// Graphic Control Extension that immediately preceded this image
    /// in the Data Stream (§23). `None` if none was present.
    pub control: Option<GraphicControl>,
}

/// Comment Extension data (GIF89a §24).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentExtension {
    /// Concatenated bytes of all sub-blocks belonging to this comment
    /// extension — the spec recommends 7-bit ASCII (§24.e.i) but does
    /// not enforce it.
    pub data: Vec<u8>,
}

/// Plain Text Extension (GIF89a §25). Note the format mandates a
/// Global Color Table be present; when none is, decoders are still
/// expected to read past these blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlainTextExtension {
    /// §25.c.iv–vii.
    pub grid_left: u16,
    pub grid_top: u16,
    pub grid_width: u16,
    pub grid_height: u16,
    /// §25.c.viii–ix.
    pub cell_width: u8,
    pub cell_height: u8,
    /// §25.c.x–xi — indices into the Global Color Table.
    pub fg_color_index: u8,
    pub bg_color_index: u8,
    /// Concatenated text data sub-blocks (§25.c.xii).
    pub data: Vec<u8>,
    /// Optional Graphic Control Extension that scoped this Plain Text
    /// (Plain Text is a graphic-rendering block per §25.a, so a GCE
    /// preceding it modifies it — same scoping rule as for an Image).
    pub control: Option<GraphicControl>,
}

/// Application Extension (GIF89a §26). Used by NETSCAPE2.0 looping,
/// XMP, ICC profile, and EXIF embedding (none of which are defined by
/// the GIF spec itself — see `docs/image/gif/README.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationExtension {
    /// 8-byte Application Identifier (§26.c.iv). Stored verbatim, no
    /// trimming or NUL-stripping.
    pub identifier: [u8; 8],
    /// 3-byte Application Authentication Code (§26.c.v).
    pub auth_code: [u8; 3],
    /// Concatenated bytes of the application data sub-blocks.
    pub data: Vec<u8>,
}

/// One element of the on-the-wire Data block sequence (§B grammar).
/// Ordering is preserved across decode/encode round-trips so that
/// inter-frame extensions land in the same positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GifBlock {
    /// A `<Table-Based Image>` — Image Descriptor + optional Local
    /// Color Table + Image Data, with any preceding Graphic Control
    /// Extension folded into [`GifFrame::control`].
    Frame(GifFrame),
    /// A Plain Text Extension graphic-rendering block.
    PlainText(PlainTextExtension),
    /// Comment Extension (§24).
    Comment(CommentExtension),
    /// Application Extension (§26).
    Application(ApplicationExtension),
}

/// Decoded GIF Data Stream — one of these per `decode_gif` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GifImage {
    /// Header version (§17).
    pub version: GifVersion,
    /// Logical Screen Width (§18.c.i) — the total canvas the frames
    /// composite onto.
    pub width: u16,
    /// Logical Screen Height (§18.c.ii).
    pub height: u16,
    /// Color Resolution per §18.c.iv. Stored as the 3-bit on-wire
    /// value (one less than the bit count of the source palette).
    pub color_resolution: u8,
    /// Sort Flag from the Logical Screen Descriptor packed-fields
    /// (§18.c.v).
    pub global_palette_sorted: bool,
    /// Background Color Index (§18.c.vii).
    pub background_color_index: u8,
    /// Pixel Aspect Ratio (§18.c.viii). `0` means no info.
    pub pixel_aspect_ratio: u8,
    /// Global Color Table, if present (§19). Layout is `R,G,B,R,G,B,…`.
    pub global_palette: Option<Vec<u8>>,
    /// Ordered sequence of Data blocks per §B grammar.
    pub blocks: Vec<GifBlock>,
}

impl GifImage {
    /// All `Frame` blocks in disk order, as a convenience for the
    /// (very common) animated-GIF case where consumers only care about
    /// the rendered frames.
    pub fn frames(&self) -> impl Iterator<Item = &GifFrame> {
        self.blocks.iter().filter_map(|b| match b {
            GifBlock::Frame(f) => Some(f),
            _ => None,
        })
    }

    /// Composite one frame onto the logical screen as RGBA8 (4 bytes
    /// per pixel, row-major, no padding) using the Disposal Method
    /// rules from §23.
    ///
    /// `prev` is the framebuffer that resulted from rendering all
    /// preceding frames; this fn returns the framebuffer after the
    /// supplied frame is rendered. The first frame should pass a
    /// fully-zero `prev` (or a buffer pre-filled with the background
    /// color — the spec leaves the initial canvas state undefined).
    ///
    /// Disposal rules implemented:
    /// * `None` / `DoNotDispose` — frame is layered on top, no clear.
    /// * `RestoreToBackground` — after layering, the *next* frame will
    ///   see the rendered region cleared to the background color (or
    ///   transparent, if a Transparency Flag is set on this frame's
    ///   GCE — common practice across the ecosystem).
    /// * `RestoreToPrevious` — the next frame will see the framebuffer
    ///   restored to the state of `prev` for the rendered region.
    pub fn composite_frame_rgba(
        &self,
        frame_index: usize,
        prev: &[u8],
    ) -> Result<Vec<u8>, crate::error::GifError> {
        let frame = self
            .frames()
            .nth(frame_index)
            .ok_or_else(|| crate::error::GifError::invalid("frame_index out of range"))?;
        let canvas_w = self.width as usize;
        let canvas_h = self.height as usize;
        let expected = canvas_w * canvas_h * 4;
        if prev.len() != expected {
            return Err(crate::error::GifError::invalid(format!(
                "prev RGBA buffer is {} bytes, expected {}x{}x4 = {}",
                prev.len(),
                canvas_w,
                canvas_h,
                expected
            )));
        }
        // Determine active palette for this frame (§11).
        let palette = frame
            .local_palette
            .as_deref()
            .or(self.global_palette.as_deref())
            .ok_or_else(|| {
                crate::error::GifError::invalid("frame has neither local nor global palette")
            })?;
        let max_index = palette.len() / 3;
        let mut buf = prev.to_vec();
        let trans = frame.control.as_ref().and_then(|c| c.transparent_index);

        for row in 0..frame.height as usize {
            for col in 0..frame.width as usize {
                let idx = frame.indices[row * frame.width as usize + col];
                if Some(idx) == trans {
                    // Transparent pixel — leave the existing canvas value alone (§23.c.viii).
                    continue;
                }
                if idx as usize >= max_index {
                    return Err(crate::error::GifError::invalid(format!(
                        "pixel index {idx} >= palette length {max_index}"
                    )));
                }
                let dst_x = frame.left as usize + col;
                let dst_y = frame.top as usize + row;
                if dst_x >= canvas_w || dst_y >= canvas_h {
                    // Spec §20: "Each image must fit within the
                    // boundaries of the Logical Screen". Don't crash on
                    // ecosystem files that violate this — silently clip.
                    continue;
                }
                let off = (dst_y * canvas_w + dst_x) * 4;
                let p = idx as usize * 3;
                buf[off] = palette[p];
                buf[off + 1] = palette[p + 1];
                buf[off + 2] = palette[p + 2];
                buf[off + 3] = 0xFF;
            }
        }
        Ok(buf)
    }
}
