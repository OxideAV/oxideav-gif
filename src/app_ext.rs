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

/// Identifier + auth code for the Adobe XMP packet extension.
/// The 8-byte identifier is `"XMP Data"` with a trailing space so it
/// pads to exactly 8 bytes.
pub const XMP_IDENTIFIER: &[u8; 8] = b"XMP Data";
pub const XMP_AUTH_CODE: &[u8; 3] = b"XMP";

/// Identifier + auth code for the ICC colour profile extension.
pub const ICC_IDENTIFIER: &[u8; 8] = b"ICCRGBG1";
pub const ICC_AUTH_CODE: &[u8; 3] = b"012";

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
        let mut out = LoopControl::default();
        let mut i = 0;
        while i < app.data.len() {
            match app.data[i] {
                NETSCAPE_SUBBLOCK_LOOP => {
                    if app.data.len() < i + 3 {
                        // Truncated; surface what we have.
                        break;
                    }
                    let lo = app.data[i + 1] as u16;
                    let hi = app.data[i + 2] as u16;
                    out.loop_count = Some(lo | (hi << 8));
                    i += 3;
                }
                NETSCAPE_SUBBLOCK_BUFFER => {
                    if app.data.len() < i + 5 {
                        break;
                    }
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
                    // Unknown sub-block ID — bail rather than risk
                    // misframing the rest of the buffer.
                    break;
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
}
