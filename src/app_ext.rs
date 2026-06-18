//! Structured parsing for well-known GIF Application Extensions (§26).
//!
//! The CompuServe GIF89a specification defines the Application
//! Extension block as a generic vendor-extensibility mechanism: an
//! 8-byte identifier plus a 3-byte authentication code naming an
//! ecosystem-defined sub-tree of arbitrary sub-block payload (§26.c.iv,
//! §26.c.v). The spec itself defines no concrete extensions — every
//! block is opaque to a strict-spec decoder, which is why
//! [`crate::Block::Application`] stores them as raw identifier + auth
//! code + concatenated sub-block bytes.
//!
//! This module sits on top of that raw representation and exposes
//! typed views for the three application-extension shapes that
//! achieved cross-decoder de-facto interoperability:
//!
//! * **NETSCAPE2.0 looping** — identifier `b"NETSCAPE"`, auth code
//!   `b"2.0"`. A 3-byte sub-block whose first byte selects the
//!   sub-block kind:
//!     - `0x01` *Looping* — followed by a little-endian `u16` loop
//!       count. `0` means "loop forever"; `N` means "play `N + 1`
//!       times" per the de-facto convention.
//!     - `0x02` *Buffering* — followed by a little-endian `u32` byte
//!       hint. Treated as a discardable hint by modern decoders;
//!       preserved here so encoders can round-trip it.
//!
//!   The whole framing is documented in
//!   `docs/image/gif/netscape2.0-loop-extension.md`.
//!
//! * **ANIMEXTS1.0 looping** — identifier `b"ANIMEXTS"`, auth code
//!   `b"1.0"`. Same 3-byte *Looping* sub-block layout as NETSCAPE2.0
//!   (sub-block ID `0x01` followed by a little-endian `u16` loop
//!   count). Authored by Aldus circa 1996 and predates the NETSCAPE2.0
//!   form; a small minority of legacy producers still emit it.
//!   Decoders that accept NETSCAPE2.0 are expected to accept this too
//!   per the cross-tool convention noted in
//!   `docs/image/gif/netscape2.0-loop-extension.md`. Surfaced here as a
//!   distinct typed view so a decode → re-encode round-trip preserves
//!   the producer's choice of identifier.
//!
//! * **XMP packet** — identifier `b"XMP Data"` (note the trailing
//!   space, padding to 8 bytes), auth code `b"XMP"`. Carries a single
//!   UTF-8 RDF/XML XMP packet. We don't parse the XML — the caller
//!   gets the raw packet bytes and can hand them to an XMP library.
//!
//! * **ICC profile** — identifier `b"ICCRGBG1"`, auth code `b"012"`.
//!   Carries an embedded ICC colour profile. We don't parse the
//!   profile — the caller gets the raw bytes and can hand them to an
//!   ICC library.
//!
//! * **EXIF metadata** — identifier `b"Exif    "` (the literal `Exif`
//!   token padded to 8 bytes with four trailing ASCII spaces). The
//!   3-byte authentication code immediately following the identifier in
//!   the §26 wire layout is treated as opaque: real-world producers
//!   pin the first byte at `0xFF` and pad the remaining two with
//!   anything they like, so this module preserves whatever was on the
//!   wire and matches solely on the identifier. The Exif 2.3 §4.7.2
//!   convention places a TIFF EXIF blob (header `b"II*\0"` /
//!   `b"MM\0*"`) directly in the sub-block payload; we do not parse
//!   the TIFF — the caller gets the raw bytes and can hand them to a
//!   TIFF/EXIF library.
//!
//! ## Round-tripping
//!
//! Decoders preserve the raw [`crate::Application`] block in
//! [`crate::GifImage::blocks`] regardless of whether the caller
//! invokes the typed accessors below. The accessors are layered on
//! top — they do not consume or rewrite the block list. This means a
//! decode → re-encode round-trip is byte-stable for streams that
//! contain these blocks even when the caller never reads the typed
//! view.
//!
//! Encoders that want to *produce* these blocks from typed input call
//! the `to_application` constructors below to get a
//! [`crate::Application`] suitable for insertion into
//! [`crate::GifImage::blocks`].

use crate::image::Application;

/// 8-byte identifier + 3-byte authentication code for the
/// NETSCAPE2.0 Application Extension.
pub const NETSCAPE_IDENTIFIER: &[u8; 8] = b"NETSCAPE";
pub const NETSCAPE_AUTH_CODE: &[u8; 3] = b"2.0";

/// 8-byte identifier + 3-byte authentication code for the legacy
/// ANIMEXTS1.0 Application Extension. Same *Looping* sub-block layout
/// as NETSCAPE2.0.
pub const ANIMEXTS_IDENTIFIER: &[u8; 8] = b"ANIMEXTS";
pub const ANIMEXTS_AUTH_CODE: &[u8; 3] = b"1.0";

/// Identifier + auth code for the Adobe XMP packet extension.
/// The 8-byte identifier is `"XMP Data"` with a trailing space so it
/// pads to exactly 8 bytes.
pub const XMP_IDENTIFIER: &[u8; 8] = b"XMP Data";
pub const XMP_AUTH_CODE: &[u8; 3] = b"XMP";

/// Identifier + auth code for the ICC colour profile extension.
pub const ICC_IDENTIFIER: &[u8; 8] = b"ICCRGBG1";
pub const ICC_AUTH_CODE: &[u8; 3] = b"012";

/// 8-byte identifier for the EXIF Application Extension. The identifier
/// is the literal `Exif` token padded to 8 bytes with four trailing
/// ASCII spaces.
pub const EXIF_IDENTIFIER: &[u8; 8] = b"Exif    ";
/// Default authentication code used when [`ExifMetadata::to_application`]
/// emits a fresh block. The first byte is fixed at `0xFF` per the
/// observed real-world convention; the remaining two bytes are zero.
/// Parsing accepts any 3-byte auth code so a round-trip preserves
/// whatever the producer wrote.
pub const EXIF_AUTH_CODE_DEFAULT: &[u8; 3] = b"\xFF\x00\x00";

/// Sub-block ID (first byte of the 3-byte sub-block) for the
/// NETSCAPE2.0 *Looping* sub-block.
pub const NETSCAPE_SUBBLOCK_LOOP: u8 = 0x01;
/// Sub-block ID for the (legacy) NETSCAPE2.0 *Buffering* sub-block.
pub const NETSCAPE_SUBBLOCK_BUFFER: u8 = 0x02;

/// Parsed NETSCAPE2.0 Application Extension contents.
///
/// Both fields are independently optional: a real-world stream might
/// carry only the *Looping* sub-block (the common case), only the
/// *Buffering* sub-block (rare), both, or neither (in which case the
/// raw bytes did not match either known sub-block ID and we do not
/// surface a typed view).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LoopControl {
    /// Loop count from sub-block ID `0x01`. `Some(0)` means "loop
    /// forever"; `Some(N)` means "play `N + 1` times" by de-facto
    /// convention. `None` means the *Looping* sub-block was absent.
    pub loop_count: Option<u16>,
    /// Byte-count buffering hint from sub-block ID `0x02`. Modern
    /// decoders ignore this; preserved here so encoders can re-emit
    /// the sub-block for byte-stable round-tripping. `None` means the
    /// *Buffering* sub-block was absent.
    pub buffer_size: Option<u32>,
}

impl LoopControl {
    /// Parse a [`crate::Application`] block as a NETSCAPE2.0 loop
    /// control. Returns `None` when the identifier or auth code does
    /// not match the NETSCAPE2.0 namespace.
    ///
    /// A matching block whose payload contains neither a known
    /// *Looping* (0x01) nor *Buffering* (0x02) sub-block decodes to a
    /// `Some(LoopControl::default())` (both fields `None`); this lets
    /// callers distinguish "block present but unrecognised contents"
    /// from "block absent".
    pub fn from_application(app: &Application) -> Option<Self> {
        if &app.identifier != NETSCAPE_IDENTIFIER || &app.auth_code != NETSCAPE_AUTH_CODE {
            return None;
        }
        // The §15 sub-block sequence concatenated by the decoder is
        // walked here as logical "frames" of [id, payload..]. The
        // *Looping* sub-block ID 0x01 is followed by a 2-byte LE u16;
        // the *Buffering* sub-block ID 0x02 is followed by a 4-byte
        // LE u32. We don't know the original §15 sub-block boundaries
        // — `Application::data` collapses the sequence — so we treat
        // each known ID as introducing its fixed-size payload.
        //
        // §15 sub-blocks are independent length-prefixed units, so the
        // *Looping* and *Buffering* sub-blocks may appear in either
        // order and a producer is free to interleave a sub-block this
        // parser does not recognise (e.g. a future NETSCAPE2.0 control
        // or an encoder-private hint). When the leading byte is neither
        // a known ID nor the start of a complete known sub-block, we
        // advance a single byte and keep scanning rather than abandoning
        // the whole block — that recovers a *Looping* count even when it
        // is preceded by an unrecognised sub-block, which the earlier
        // "bail on first unknown ID" rule silently dropped. Each field
        // is captured at its first complete occurrence; a later stray
        // match cannot overwrite an already-resolved value, keeping the
        // typed view stable.
        let mut out = LoopControl::default();
        let mut i = 0;
        while i < app.data.len() {
            match app.data[i] {
                NETSCAPE_SUBBLOCK_LOOP if out.loop_count.is_none() && i + 3 <= app.data.len() => {
                    let lo = app.data[i + 1] as u16;
                    let hi = app.data[i + 2] as u16;
                    out.loop_count = Some(lo | (hi << 8));
                    i += 3;
                }
                NETSCAPE_SUBBLOCK_BUFFER
                    if out.buffer_size.is_none() && i + 5 <= app.data.len() =>
                {
                    let b = [
                        app.data[i + 1],
                        app.data[i + 2],
                        app.data[i + 3],
                        app.data[i + 4],
                    ];
                    out.buffer_size = Some(u32::from_le_bytes(b));
                    i += 5;
                }
                _ => {
                    // Unknown sub-block ID, an already-captured field, or
                    // a truncated tail of a known sub-block: resync by a
                    // single byte instead of misframing or abandoning the
                    // remainder of the buffer.
                    i += 1;
                }
            }
        }
        Some(out)
    }

    /// Build a NETSCAPE2.0 [`Application`] block from a parsed loop
    /// control. Emits a 3-byte *Looping* sub-block when
    /// `loop_count.is_some()` and a 5-byte *Buffering* sub-block when
    /// `buffer_size.is_some()`. With both fields `None` the
    /// returned block carries an empty payload, which is still a
    /// well-formed NETSCAPE2.0 Application Extension (just one with
    /// no recognised sub-blocks).
    pub fn to_application(&self) -> Application {
        let mut data = Vec::with_capacity(8);
        if let Some(n) = self.loop_count {
            data.push(NETSCAPE_SUBBLOCK_LOOP);
            data.push((n & 0xFF) as u8);
            data.push(((n >> 8) & 0xFF) as u8);
        }
        if let Some(n) = self.buffer_size {
            data.push(NETSCAPE_SUBBLOCK_BUFFER);
            data.extend_from_slice(&n.to_le_bytes());
        }
        Application {
            identifier: *NETSCAPE_IDENTIFIER,
            auth_code: *NETSCAPE_AUTH_CODE,
            data,
        }
    }
}

/// Parsed ANIMEXTS1.0 Application Extension contents.
///
/// The wire framing reuses the NETSCAPE2.0 *Looping* sub-block:
/// sub-block ID `0x01` followed by a little-endian `u16` loop count.
/// The *Buffering* sub-block (`0x02`) is NETSCAPE2.0-specific and does
/// not appear under this identifier in any observed producer; this
/// parser ignores any byte other than the *Looping* sub-block ID, which
/// matches the conservative "first matching sub-block wins" rule used
/// elsewhere in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AnimextsLoopControl {
    /// Loop count from the *Looping* sub-block. `Some(0)` means
    /// "loop forever"; `Some(N)` means "play `N + 1` times" by
    /// de-facto convention. `None` means the sub-block was absent.
    pub loop_count: Option<u16>,
}

impl AnimextsLoopControl {
    /// Parse a [`crate::Application`] block as an ANIMEXTS1.0 loop
    /// control. Returns `None` when the identifier or auth code does
    /// not match the ANIMEXTS1.0 namespace.
    pub fn from_application(app: &Application) -> Option<Self> {
        if &app.identifier != ANIMEXTS_IDENTIFIER || &app.auth_code != ANIMEXTS_AUTH_CODE {
            return None;
        }
        // As in [`LoopControl::from_application`], the §15 sub-block
        // boundaries are already collapsed, so we scan the flat payload
        // for the *Looping* sub-block ID and resync a single byte past
        // anything we do not recognise rather than bailing at the first
        // unknown byte — this recovers a *Looping* count even when an
        // unrecognised sub-block precedes it. The first complete
        // occurrence wins.
        let mut out = AnimextsLoopControl::default();
        let mut i = 0;
        while i < app.data.len() {
            match app.data[i] {
                NETSCAPE_SUBBLOCK_LOOP if out.loop_count.is_none() && i + 3 <= app.data.len() => {
                    let lo = app.data[i + 1] as u16;
                    let hi = app.data[i + 2] as u16;
                    out.loop_count = Some(lo | (hi << 8));
                    i += 3;
                }
                _ => i += 1,
            }
        }
        Some(out)
    }

    /// Build an ANIMEXTS1.0 [`Application`] block from a parsed loop
    /// control. Emits a 3-byte *Looping* sub-block when
    /// `loop_count.is_some()`; otherwise the returned block carries an
    /// empty payload.
    pub fn to_application(&self) -> Application {
        let mut data = Vec::with_capacity(3);
        if let Some(n) = self.loop_count {
            data.push(NETSCAPE_SUBBLOCK_LOOP);
            data.push((n & 0xFF) as u8);
            data.push(((n >> 8) & 0xFF) as u8);
        }
        Application {
            identifier: *ANIMEXTS_IDENTIFIER,
            auth_code: *ANIMEXTS_AUTH_CODE,
            data,
        }
    }
}

/// Parsed XMP packet from the `XMP Data` Application Extension.
///
/// The payload is the raw XMP packet — typically a UTF-8 RDF/XML
/// document. We do not parse the XML here; consumers that need it
/// should hand `bytes` to an XMP library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmpPacket {
    pub bytes: Vec<u8>,
}

impl XmpPacket {
    /// Parse a [`crate::Application`] block as an XMP packet. Returns
    /// `None` when the identifier or auth code does not match.
    pub fn from_application(app: &Application) -> Option<Self> {
        if &app.identifier != XMP_IDENTIFIER || &app.auth_code != XMP_AUTH_CODE {
            return None;
        }
        Some(XmpPacket {
            bytes: app.data.clone(),
        })
    }

    /// Build an XMP [`Application`] block from a raw packet.
    pub fn to_application(&self) -> Application {
        Application {
            identifier: *XMP_IDENTIFIER,
            auth_code: *XMP_AUTH_CODE,
            data: self.bytes.clone(),
        }
    }
}

/// Parsed ICC colour profile from the `ICCRGBG1` Application
/// Extension.
///
/// The payload is the raw ICC profile bytes (typically prefixed with
/// the 128-byte ICC profile header per ISO 15076-1). We do not parse
/// it here; consumers that need to honour the profile should hand
/// `bytes` to an ICC library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IccProfile {
    pub bytes: Vec<u8>,
}

impl IccProfile {
    /// Parse a [`crate::Application`] block as an ICC profile. Returns
    /// `None` when the identifier or auth code does not match.
    pub fn from_application(app: &Application) -> Option<Self> {
        if &app.identifier != ICC_IDENTIFIER || &app.auth_code != ICC_AUTH_CODE {
            return None;
        }
        Some(IccProfile {
            bytes: app.data.clone(),
        })
    }

    /// Build an ICC [`Application`] block from a raw profile.
    pub fn to_application(&self) -> Application {
        Application {
            identifier: *ICC_IDENTIFIER,
            auth_code: *ICC_AUTH_CODE,
            data: self.bytes.clone(),
        }
    }
}

/// Parsed EXIF blob from the `Exif    ` Application Extension.
///
/// The payload is the raw TIFF EXIF blob — typically beginning with
/// either `b"II*\0"` (little-endian byte order) or `b"MM\0*"`
/// (big-endian) per TIFF 6.0 §2 "Image File Header". We do not parse
/// the TIFF tag tree here; consumers that need to honour individual
/// tags should hand `bytes` to a TIFF/EXIF library.
///
/// The 3-byte authentication code that immediately follows the
/// identifier in the §26 wire layout is preserved alongside the payload
/// so a decode → re-encode round-trip is byte-stable: real producers
/// pin the first byte at `0xFF` and pad the remaining two with values
/// that vary by tool, and a strict-spec encoder must replay exactly
/// what it received rather than substitute a default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExifMetadata {
    /// The 3-byte authentication code as it appeared on the wire.
    /// [`ExifMetadata::to_application`] echoes this back unchanged.
    /// Use [`EXIF_AUTH_CODE_DEFAULT`] when constructing a brand-new
    /// block from scratch.
    pub auth_code: [u8; 3],
    /// Raw TIFF EXIF blob. Typically begins with `b"II*\0"` or
    /// `b"MM\0*"`.
    pub bytes: Vec<u8>,
}

impl ExifMetadata {
    /// Parse a [`crate::Application`] block as an EXIF blob. Returns
    /// `None` when the identifier does not match — the auth code is
    /// preserved as-is for round-tripping rather than checked.
    pub fn from_application(app: &Application) -> Option<Self> {
        if &app.identifier != EXIF_IDENTIFIER {
            return None;
        }
        Some(ExifMetadata {
            auth_code: app.auth_code,
            bytes: app.data.clone(),
        })
    }

    /// Build an EXIF [`Application`] block from a raw blob, replaying
    /// the stored authentication code so a decode → re-encode round
    /// trip is byte-stable.
    pub fn to_application(&self) -> Application {
        Application {
            identifier: *EXIF_IDENTIFIER,
            auth_code: self.auth_code,
            data: self.bytes.clone(),
        }
    }

    /// Build a fresh EXIF [`Application`] block from a raw blob using
    /// the [`EXIF_AUTH_CODE_DEFAULT`] auth code. Use this constructor
    /// when authoring a new GIF from scratch (no auth-code preservation
    /// is required).
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            auth_code: *EXIF_AUTH_CODE_DEFAULT,
            bytes,
        }
    }
}

/// Classification of a §26 Application Extension by its 8-byte
/// identifier + 3-byte authentication code (§26.c.iv / §26.c.v).
///
/// The CompuServe GIF89a spec defines no concrete application
/// extensions — every block is opaque to a strict-spec decoder. This
/// enum names the five ecosystem-defined shapes that achieved
/// cross-decoder de-facto interoperability (the ones with typed views
/// in this module) and folds everything else into [`Self::Unknown`].
///
/// It is a *classification by namespace*, not a payload parse: a
/// [`Self::Netscape`] block is one whose identifier+auth code match the
/// NETSCAPE2.0 namespace, regardless of which sub-blocks the payload
/// actually carries (a NETSCAPE2.0 block with no recognised sub-block
/// is still classified `Netscape`). Use [`LoopControl::from_application`]
/// et al. when the *payload* matters.
///
/// Matching follows each typed view's own rule:
///
/// * NETSCAPE2.0 / ANIMEXTS1.0 / XMP / ICC — both the identifier **and**
///   the authentication code must match (the auth code is part of the
///   §26 namespace key for these).
/// * EXIF — identifier-only, matching [`ExifMetadata::from_application`].
///   Real-world EXIF producers pin only the first auth byte at `0xFF`
///   and pad the remaining two arbitrarily, so the auth code is not part
///   of the EXIF match key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApplicationKind {
    /// NETSCAPE2.0 (`b"NETSCAPE"` + `b"2.0"`) — animation looping /
    /// buffering. See [`LoopControl`].
    Netscape,
    /// ANIMEXTS1.0 (`b"ANIMEXTS"` + `b"1.0"`) — legacy looping. See
    /// [`AnimextsLoopControl`].
    Animexts,
    /// Adobe XMP packet (`b"XMP Data"` + `b"XMP"`). See [`XmpPacket`].
    Xmp,
    /// ICC colour profile (`b"ICCRGBG1"` + `b"012"`). See [`IccProfile`].
    Icc,
    /// EXIF metadata (`b"Exif    "`, identifier-only). See
    /// [`ExifMetadata`].
    Exif,
    /// Any Application Extension whose identifier+auth code does not
    /// match one of the recognised ecosystem namespaces above. The raw
    /// [`Application`] is still preserved in
    /// [`crate::GifImage::blocks`] for byte-stable round-trip.
    Unknown,
}

impl ApplicationKind {
    /// Classify an [`Application`] block by its §26 namespace key.
    ///
    /// Returns the matching recognised [`ApplicationKind`], or
    /// [`ApplicationKind::Unknown`] when the identifier+auth code is not
    /// one of the five ecosystem-defined shapes. See the type-level docs
    /// for the per-kind matching rule (auth-code-sensitive for all but
    /// EXIF).
    pub fn classify(app: &Application) -> Self {
        if &app.identifier == NETSCAPE_IDENTIFIER && &app.auth_code == NETSCAPE_AUTH_CODE {
            ApplicationKind::Netscape
        } else if &app.identifier == ANIMEXTS_IDENTIFIER && &app.auth_code == ANIMEXTS_AUTH_CODE {
            ApplicationKind::Animexts
        } else if &app.identifier == XMP_IDENTIFIER && &app.auth_code == XMP_AUTH_CODE {
            ApplicationKind::Xmp
        } else if &app.identifier == ICC_IDENTIFIER && &app.auth_code == ICC_AUTH_CODE {
            ApplicationKind::Icc
        } else if &app.identifier == EXIF_IDENTIFIER {
            ApplicationKind::Exif
        } else {
            ApplicationKind::Unknown
        }
    }

    /// `true` for every variant except [`ApplicationKind::Unknown`] —
    /// i.e. this crate ships a typed view for the block's namespace.
    pub fn is_recognized(self) -> bool {
        !matches!(self, ApplicationKind::Unknown)
    }
}

impl Application {
    /// Classify this §26 Application Extension by its namespace key —
    /// shorthand for [`ApplicationKind::classify`].
    pub fn kind(&self) -> ApplicationKind {
        ApplicationKind::classify(self)
    }

    /// `true` when this block's identifier+auth code matches one of the
    /// five ecosystem-defined namespaces this crate ships a typed view
    /// for (NETSCAPE2.0 / ANIMEXTS1.0 / XMP / ICC / EXIF) — shorthand
    /// for `self.kind().is_recognized()`.
    pub fn is_recognized(&self) -> bool {
        self.kind().is_recognized()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netscape_loop_only_roundtrip() {
        let original = LoopControl {
            loop_count: Some(7),
            buffer_size: None,
        };
        let app = original.to_application();
        assert_eq!(&app.identifier, NETSCAPE_IDENTIFIER);
        assert_eq!(&app.auth_code, NETSCAPE_AUTH_CODE);
        // 1 ID byte + 2 LE u16 = 3 bytes
        assert_eq!(app.data, vec![NETSCAPE_SUBBLOCK_LOOP, 0x07, 0x00]);
        let parsed = LoopControl::from_application(&app).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn netscape_loop_forever() {
        let lc = LoopControl {
            loop_count: Some(0),
            buffer_size: None,
        };
        let parsed = LoopControl::from_application(&lc.to_application()).unwrap();
        assert_eq!(parsed.loop_count, Some(0));
    }

    #[test]
    fn netscape_buffer_only_roundtrip() {
        let original = LoopControl {
            loop_count: None,
            buffer_size: Some(0x0001_2345),
        };
        let app = original.to_application();
        // 1 ID byte + 4 LE u32 = 5 bytes
        assert_eq!(
            app.data,
            vec![NETSCAPE_SUBBLOCK_BUFFER, 0x45, 0x23, 0x01, 0x00]
        );
        let parsed = LoopControl::from_application(&app).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn netscape_loop_plus_buffer_roundtrip() {
        let original = LoopControl {
            loop_count: Some(0xABCD),
            buffer_size: Some(0xDEAD_BEEF),
        };
        let app = original.to_application();
        let parsed = LoopControl::from_application(&app).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn netscape_wrong_identifier_returns_none() {
        let app = Application {
            identifier: *b"NOTSCAPE",
            auth_code: *NETSCAPE_AUTH_CODE,
            data: vec![NETSCAPE_SUBBLOCK_LOOP, 0x05, 0x00],
        };
        assert!(LoopControl::from_application(&app).is_none());
    }

    #[test]
    fn netscape_wrong_auth_returns_none() {
        let app = Application {
            identifier: *NETSCAPE_IDENTIFIER,
            auth_code: *b"3.0",
            data: vec![NETSCAPE_SUBBLOCK_LOOP, 0x05, 0x00],
        };
        assert!(LoopControl::from_application(&app).is_none());
    }

    #[test]
    fn netscape_unknown_subblock_yields_empty_view() {
        // Identifier matches but the sub-block ID is neither 0x01 nor
        // 0x02 → both fields stay None.
        let app = Application {
            identifier: *NETSCAPE_IDENTIFIER,
            auth_code: *NETSCAPE_AUTH_CODE,
            data: vec![0x42, 0xFF, 0xFF],
        };
        let parsed = LoopControl::from_application(&app).unwrap();
        assert_eq!(parsed, LoopControl::default());
    }

    #[test]
    fn netscape_truncated_loop_subblock_does_not_panic() {
        let app = Application {
            identifier: *NETSCAPE_IDENTIFIER,
            auth_code: *NETSCAPE_AUTH_CODE,
            data: vec![NETSCAPE_SUBBLOCK_LOOP, 0x05], // missing high byte
        };
        let parsed = LoopControl::from_application(&app).unwrap();
        assert_eq!(parsed.loop_count, None);
    }

    #[test]
    fn xmp_roundtrip() {
        let original = XmpPacket {
            bytes: br#"<?xpacket begin=""?><x:xmpmeta/>"#.to_vec(),
        };
        let app = original.to_application();
        assert_eq!(&app.identifier, XMP_IDENTIFIER);
        assert_eq!(&app.auth_code, XMP_AUTH_CODE);
        let parsed = XmpPacket::from_application(&app).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn xmp_wrong_namespace_returns_none() {
        let app = Application {
            identifier: *b"NETSCAPE",
            auth_code: *b"2.0",
            data: vec![],
        };
        assert!(XmpPacket::from_application(&app).is_none());
    }

    #[test]
    fn icc_roundtrip() {
        let original = IccProfile {
            bytes: vec![0u8; 128], // simulated header-sized payload
        };
        let app = original.to_application();
        assert_eq!(&app.identifier, ICC_IDENTIFIER);
        assert_eq!(&app.auth_code, ICC_AUTH_CODE);
        let parsed = IccProfile::from_application(&app).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn icc_wrong_namespace_returns_none() {
        let app = Application {
            identifier: *b"XMP Data",
            auth_code: *b"XMP",
            data: vec![],
        };
        assert!(IccProfile::from_application(&app).is_none());
    }

    #[test]
    fn exif_identifier_is_padded_to_eight_bytes() {
        // The identifier slot is exactly 8 bytes per §26.c.iv. The
        // EXIF token is 4 ASCII letters; the canonical form pads with
        // four trailing spaces.
        assert_eq!(EXIF_IDENTIFIER.len(), 8);
        assert_eq!(&EXIF_IDENTIFIER[..4], b"Exif");
        assert_eq!(&EXIF_IDENTIFIER[4..], b"    ");
    }

    #[test]
    fn exif_default_auth_code_pins_first_byte_to_ff() {
        // Real-world producers consistently pin auth_code[0] = 0xFF.
        // Defaulting elsewhere would produce blocks that other
        // ecosystem tools refuse to recognise.
        assert_eq!(EXIF_AUTH_CODE_DEFAULT[0], 0xFF);
    }

    #[test]
    fn exif_roundtrip_with_default_auth_code() {
        let original = ExifMetadata::new(b"II*\0\x08\x00\x00\x00\x00\x00".to_vec());
        let app = original.to_application();
        assert_eq!(&app.identifier, EXIF_IDENTIFIER);
        assert_eq!(&app.auth_code, EXIF_AUTH_CODE_DEFAULT);
        let parsed = ExifMetadata::from_application(&app).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn exif_roundtrip_preserves_unusual_auth_code() {
        // A producer might write a non-default auth code; we must
        // round-trip whatever was on the wire rather than substitute
        // EXIF_AUTH_CODE_DEFAULT.
        let app = Application {
            identifier: *EXIF_IDENTIFIER,
            auth_code: [0xFF, 0xAB, 0xCD],
            data: b"MM\0*\x00\x00\x00\x08".to_vec(),
        };
        let parsed = ExifMetadata::from_application(&app).unwrap();
        assert_eq!(parsed.auth_code, [0xFF, 0xAB, 0xCD]);
        let rebuilt = parsed.to_application();
        assert_eq!(rebuilt, app, "byte-stable round-trip");
    }

    #[test]
    fn exif_wrong_identifier_returns_none() {
        let app = Application {
            identifier: *b"NETSCAPE",
            auth_code: *b"2.0",
            data: vec![],
        };
        assert!(ExifMetadata::from_application(&app).is_none());
    }

    #[test]
    fn animexts_loop_roundtrip() {
        let original = AnimextsLoopControl {
            loop_count: Some(5),
        };
        let app = original.to_application();
        assert_eq!(&app.identifier, ANIMEXTS_IDENTIFIER);
        assert_eq!(&app.auth_code, ANIMEXTS_AUTH_CODE);
        assert_eq!(app.data, vec![NETSCAPE_SUBBLOCK_LOOP, 0x05, 0x00]);
        let parsed = AnimextsLoopControl::from_application(&app).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn animexts_loop_forever() {
        let lc = AnimextsLoopControl {
            loop_count: Some(0),
        };
        let parsed = AnimextsLoopControl::from_application(&lc.to_application()).unwrap();
        assert_eq!(parsed.loop_count, Some(0));
    }

    #[test]
    fn animexts_wrong_identifier_returns_none() {
        let app = Application {
            identifier: *NETSCAPE_IDENTIFIER,
            auth_code: *ANIMEXTS_AUTH_CODE,
            data: vec![NETSCAPE_SUBBLOCK_LOOP, 0x01, 0x00],
        };
        assert!(AnimextsLoopControl::from_application(&app).is_none());
    }

    #[test]
    fn animexts_wrong_auth_returns_none() {
        let app = Application {
            identifier: *ANIMEXTS_IDENTIFIER,
            auth_code: *NETSCAPE_AUTH_CODE,
            data: vec![NETSCAPE_SUBBLOCK_LOOP, 0x01, 0x00],
        };
        assert!(AnimextsLoopControl::from_application(&app).is_none());
    }

    #[test]
    fn animexts_truncated_loop_subblock_does_not_panic() {
        let app = Application {
            identifier: *ANIMEXTS_IDENTIFIER,
            auth_code: *ANIMEXTS_AUTH_CODE,
            data: vec![NETSCAPE_SUBBLOCK_LOOP, 0x42], // missing high byte
        };
        let parsed = AnimextsLoopControl::from_application(&app).unwrap();
        assert_eq!(parsed.loop_count, None);
    }

    #[test]
    fn animexts_namespace_distinct_from_netscape() {
        // A NETSCAPE2.0 block must NOT parse as an ANIMEXTS1.0 block
        // and vice versa, even though the looping sub-block layout is
        // identical.
        let netscape = LoopControl {
            loop_count: Some(3),
            buffer_size: None,
        }
        .to_application();
        assert!(AnimextsLoopControl::from_application(&netscape).is_none());

        let animexts = AnimextsLoopControl {
            loop_count: Some(3),
        }
        .to_application();
        let parsed = LoopControl::from_application(&animexts);
        // LoopControl checks identifier+auth strictly, so ANIMEXTS1.0
        // must NOT decode as a NETSCAPE2.0 view.
        assert!(parsed.is_none());
    }

    #[test]
    fn netscape_loop_recovered_after_unknown_subblock() {
        // §15 sub-blocks are independent units; a producer may place a
        // sub-block this parser does not recognise (here a one-byte 0x42
        // "id" stand-in) ahead of the *Looping* sub-block. The earlier
        // "bail on first unknown ID" rule dropped the loop count; the
        // resync scan must still recover it.
        let app = Application {
            identifier: *NETSCAPE_IDENTIFIER,
            auth_code: *NETSCAPE_AUTH_CODE,
            data: vec![0x42, NETSCAPE_SUBBLOCK_LOOP, 0x09, 0x00],
        };
        let parsed = LoopControl::from_application(&app).unwrap();
        assert_eq!(parsed.loop_count, Some(9));
        assert_eq!(parsed.buffer_size, None);
    }

    #[test]
    fn netscape_buffer_then_loop_order_independent() {
        // Buffering sub-block first, then Looping. Both must surface
        // regardless of order.
        let mut data = vec![NETSCAPE_SUBBLOCK_BUFFER];
        data.extend_from_slice(&0x0000_0400u32.to_le_bytes());
        data.push(NETSCAPE_SUBBLOCK_LOOP);
        data.push(0x03);
        data.push(0x00);
        let app = Application {
            identifier: *NETSCAPE_IDENTIFIER,
            auth_code: *NETSCAPE_AUTH_CODE,
            data,
        };
        let parsed = LoopControl::from_application(&app).unwrap();
        assert_eq!(parsed.loop_count, Some(3));
        assert_eq!(parsed.buffer_size, Some(0x0400));
    }

    #[test]
    fn netscape_first_loop_occurrence_wins() {
        // Two *Looping* sub-blocks: the first complete occurrence must
        // be the one surfaced; a later stray match cannot overwrite it.
        let app = Application {
            identifier: *NETSCAPE_IDENTIFIER,
            auth_code: *NETSCAPE_AUTH_CODE,
            data: vec![
                NETSCAPE_SUBBLOCK_LOOP,
                0x01,
                0x00,
                NETSCAPE_SUBBLOCK_LOOP,
                0x02,
                0x00,
            ],
        };
        let parsed = LoopControl::from_application(&app).unwrap();
        assert_eq!(parsed.loop_count, Some(1));
    }

    #[test]
    fn netscape_trailing_unknown_after_loop_is_ignored() {
        // A recognised *Looping* sub-block followed by trailing bytes
        // that are not a known sub-block: the loop count is captured and
        // the tail is harmlessly resynced away.
        let app = Application {
            identifier: *NETSCAPE_IDENTIFIER,
            auth_code: *NETSCAPE_AUTH_CODE,
            data: vec![NETSCAPE_SUBBLOCK_LOOP, 0x07, 0x00, 0xAA, 0xBB],
        };
        let parsed = LoopControl::from_application(&app).unwrap();
        assert_eq!(parsed.loop_count, Some(7));
    }

    #[test]
    fn animexts_loop_recovered_after_unknown_subblock() {
        let app = Application {
            identifier: *ANIMEXTS_IDENTIFIER,
            auth_code: *ANIMEXTS_AUTH_CODE,
            data: vec![0x42, NETSCAPE_SUBBLOCK_LOOP, 0x04, 0x00],
        };
        let parsed = AnimextsLoopControl::from_application(&app).unwrap();
        assert_eq!(parsed.loop_count, Some(4));
    }

    #[test]
    fn empty_payload_yields_default_view() {
        // A NETSCAPE2.0 / ANIMEXTS1.0 block with a zero-byte payload is
        // structurally valid §26 and must surface as the empty typed
        // view rather than panic on the now byte-at-a-time scan.
        let ns = Application {
            identifier: *NETSCAPE_IDENTIFIER,
            auth_code: *NETSCAPE_AUTH_CODE,
            data: vec![],
        };
        assert_eq!(
            LoopControl::from_application(&ns).unwrap(),
            LoopControl::default()
        );
        let ax = Application {
            identifier: *ANIMEXTS_IDENTIFIER,
            auth_code: *ANIMEXTS_AUTH_CODE,
            data: vec![],
        };
        assert_eq!(
            AnimextsLoopControl::from_application(&ax).unwrap(),
            AnimextsLoopControl::default()
        );
    }

    #[test]
    fn exif_empty_payload_still_parses() {
        // A block with the right identifier but a zero-byte payload is
        // structurally valid §26 — the typed accessor surfaces it as an
        // empty blob rather than rejecting.
        let app = Application {
            identifier: *EXIF_IDENTIFIER,
            auth_code: *EXIF_AUTH_CODE_DEFAULT,
            data: vec![],
        };
        let parsed = ExifMetadata::from_application(&app).unwrap();
        assert!(parsed.bytes.is_empty());
    }

    fn app(identifier: &[u8; 8], auth_code: &[u8; 3]) -> Application {
        Application {
            identifier: *identifier,
            auth_code: *auth_code,
            data: vec![],
        }
    }

    #[test]
    fn classify_recognises_every_known_namespace() {
        assert_eq!(
            app(NETSCAPE_IDENTIFIER, NETSCAPE_AUTH_CODE).kind(),
            ApplicationKind::Netscape
        );
        assert_eq!(
            app(ANIMEXTS_IDENTIFIER, ANIMEXTS_AUTH_CODE).kind(),
            ApplicationKind::Animexts
        );
        assert_eq!(
            app(XMP_IDENTIFIER, XMP_AUTH_CODE).kind(),
            ApplicationKind::Xmp
        );
        assert_eq!(
            app(ICC_IDENTIFIER, ICC_AUTH_CODE).kind(),
            ApplicationKind::Icc
        );
        // EXIF matches on identifier only — any auth code classifies.
        assert_eq!(
            app(EXIF_IDENTIFIER, b"\xFF\x12\x34").kind(),
            ApplicationKind::Exif
        );
        assert!(app(NETSCAPE_IDENTIFIER, NETSCAPE_AUTH_CODE).is_recognized());
        assert!(app(EXIF_IDENTIFIER, b"\xFF\x00\x00").is_recognized());
    }

    #[test]
    fn classify_unknown_namespace() {
        // Right NETSCAPE identifier but wrong auth code → not Netscape.
        let wrong_auth = app(NETSCAPE_IDENTIFIER, b"9.9");
        assert_eq!(wrong_auth.kind(), ApplicationKind::Unknown);
        assert!(!wrong_auth.is_recognized());
        // A totally vendor-private namespace.
        let private = app(b"PRIVATE!", b"xyz");
        assert_eq!(private.kind(), ApplicationKind::Unknown);
        assert!(!private.is_recognized());
    }

    #[test]
    fn classify_is_namespace_not_payload() {
        // A NETSCAPE2.0 block with no recognised sub-block payload is
        // still classified Netscape — classification is by namespace
        // key, not by what the payload parses to.
        let empty_netscape = app(NETSCAPE_IDENTIFIER, NETSCAPE_AUTH_CODE);
        assert!(LoopControl::from_application(&empty_netscape)
            .and_then(|lc| lc.loop_count)
            .is_none());
        assert_eq!(empty_netscape.kind(), ApplicationKind::Netscape);
    }
}
