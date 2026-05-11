//! Minimal 8×8 bitmap font for the GIF89a §25 Plain Text Extension.
//!
//! # Why a font lives in the codec
//!
//! §25.a — "Text data are rendered using a grid of character cells …
//! rendered as mono-spaced characters, one character per cell, with a
//! best fitting font and size." §25.e — "The selection of font and
//! size is left to the discretion of the decoder."
//!
//! The spec leaves the rendered appearance up to the decoder, so a
//! self-contained Plain Text renderer needs a built-in font. This
//! module supplies a clean-room minimal 8×8 stylised font covering
//! printable ASCII (0x20..=0x7E), authored from first principles for
//! this crate. It is intentionally simple — geometric primitives
//! (boxes, slashes, crossbars) chosen so each glyph reads at the 8×8
//! resolution without any artistic detailing borrowed from another
//! font.
//!
//! # Bit layout
//!
//! Each glyph occupies exactly eight bytes, one byte per row,
//! top-to-bottom. Within a row the bits are big-endian — **bit 7 is
//! the leftmost pixel**, bit 0 the rightmost — so a row byte of
//! `0b1000_0001` paints pixel `(0, row)` and `(7, row)`.
//!
//! # Coverage and fallback
//!
//! §25.e — "If characters less than 0x20 or greater than 0xf7 are
//! encountered, it is recommended that the decoder display a Space
//! character (0x20)." [`glyph`] returns the all-zero space glyph for
//! every code point outside the printable-ASCII subset this font
//! covers, satisfying the spec's fallback rule.

/// Width of every glyph cell in pixels.
pub const GLYPH_WIDTH: u8 = 8;
/// Height of every glyph cell in pixels.
pub const GLYPH_HEIGHT: u8 = 8;

/// Lookup table — eight bytes per character in code-point order from
/// 0x20 (space) to 0x7E (tilde). Indexing is `[c - 0x20]`.
///
/// Glyph shapes were authored row-by-row for this crate from a small
/// set of stroke primitives (vertical bar, horizontal bar, diagonals,
/// crossbar). They are deliberately compact and stylised — readable
/// at the 8×8 resolution without artistic embellishment.
#[rustfmt::skip]
const PRINTABLE_ASCII: [[u8; 8]; 0x7E - 0x20 + 1] = [
    // 0x20 ' ' — space (intentionally blank).
    [0,0,0,0,0,0,0,0],
    // 0x21 '!' — vertical bar with a dot at the bottom.
    [0b00011000, 0b00011000, 0b00011000, 0b00011000, 0b00011000, 0b00000000, 0b00011000, 0b00000000],
    // 0x22 '"'
    [0b01101100, 0b01101100, 0b00000000, 0,0,0,0,0],
    // 0x23 '#'
    [0b00100100, 0b00100100, 0b01111110, 0b00100100, 0b01111110, 0b00100100, 0b00100100, 0],
    // 0x24 '$'
    [0b00010000, 0b00111110, 0b01010000, 0b00111100, 0b00010010, 0b01111100, 0b00010000, 0],
    // 0x25 '%'
    [0b01100010, 0b01100100, 0b00001000, 0b00010000, 0b00100110, 0b01000110, 0,0],
    // 0x26 '&'
    [0b00110000, 0b01001000, 0b00110000, 0b01010110, 0b01001100, 0b00110110, 0,0],
    // 0x27 '\''
    [0b00011000, 0b00011000, 0,0,0,0,0,0],
    // 0x28 '('
    [0b00000110, 0b00001100, 0b00011000, 0b00011000, 0b00011000, 0b00001100, 0b00000110, 0],
    // 0x29 ')'
    [0b01100000, 0b00110000, 0b00011000, 0b00011000, 0b00011000, 0b00110000, 0b01100000, 0],
    // 0x2A '*'
    [0,0b01001000, 0b00110000, 0b11111100, 0b00110000, 0b01001000, 0,0],
    // 0x2B '+'
    [0,0b00011000, 0b00011000, 0b01111110, 0b00011000, 0b00011000, 0,0],
    // 0x2C ','
    [0,0,0,0,0,0b00011000, 0b00011000, 0b00110000],
    // 0x2D '-'
    [0,0,0,0b01111110, 0,0,0,0],
    // 0x2E '.'
    [0,0,0,0,0,0,0b00011000, 0b00011000],
    // 0x2F '/'
    [0b00000010, 0b00000100, 0b00001000, 0b00010000, 0b00100000, 0b01000000, 0b10000000, 0],
    // 0x30 '0'
    [0b00111100, 0b01000110, 0b01001010, 0b01010010, 0b01100010, 0b01000010, 0b00111100, 0],
    // 0x31 '1'
    [0b00011000, 0b00111000, 0b00011000, 0b00011000, 0b00011000, 0b00011000, 0b01111110, 0],
    // 0x32 '2'
    [0b00111100, 0b01000010, 0b00000010, 0b00001100, 0b00110000, 0b01000000, 0b01111110, 0],
    // 0x33 '3'
    [0b00111100, 0b01000010, 0b00000010, 0b00011100, 0b00000010, 0b01000010, 0b00111100, 0],
    // 0x34 '4'
    [0b00001100, 0b00010100, 0b00100100, 0b01000100, 0b01111110, 0b00000100, 0b00000100, 0],
    // 0x35 '5'
    [0b01111110, 0b01000000, 0b01111100, 0b00000010, 0b00000010, 0b01000010, 0b00111100, 0],
    // 0x36 '6'
    [0b00111100, 0b01000010, 0b01000000, 0b01111100, 0b01000010, 0b01000010, 0b00111100, 0],
    // 0x37 '7'
    [0b01111110, 0b00000010, 0b00000100, 0b00001000, 0b00010000, 0b00010000, 0b00010000, 0],
    // 0x38 '8'
    [0b00111100, 0b01000010, 0b01000010, 0b00111100, 0b01000010, 0b01000010, 0b00111100, 0],
    // 0x39 '9'
    [0b00111100, 0b01000010, 0b01000010, 0b00111110, 0b00000010, 0b01000010, 0b00111100, 0],
    // 0x3A ':'
    [0,0b00011000, 0b00011000, 0,0,0b00011000, 0b00011000, 0],
    // 0x3B ';'
    [0,0b00011000, 0b00011000, 0,0,0b00011000, 0b00011000, 0b00110000],
    // 0x3C '<'
    [0b00000110, 0b00001100, 0b00011000, 0b00110000, 0b00011000, 0b00001100, 0b00000110, 0],
    // 0x3D '='
    [0,0,0b01111110, 0,0b01111110, 0,0,0],
    // 0x3E '>'
    [0b01100000, 0b00110000, 0b00011000, 0b00001100, 0b00011000, 0b00110000, 0b01100000, 0],
    // 0x3F '?'
    [0b00111100, 0b01000010, 0b00000100, 0b00001000, 0b00011000, 0,0b00011000, 0],
    // 0x40 '@'
    [0b00111100, 0b01000010, 0b01011110, 0b01010010, 0b01011110, 0b01000000, 0b00111100, 0],
    // 0x41 'A'
    [0b00011000, 0b00100100, 0b01000010, 0b01000010, 0b01111110, 0b01000010, 0b01000010, 0],
    // 0x42 'B'
    [0b01111100, 0b01000010, 0b01000010, 0b01111100, 0b01000010, 0b01000010, 0b01111100, 0],
    // 0x43 'C'
    [0b00111100, 0b01000010, 0b01000000, 0b01000000, 0b01000000, 0b01000010, 0b00111100, 0],
    // 0x44 'D'
    [0b01111000, 0b01000100, 0b01000010, 0b01000010, 0b01000010, 0b01000100, 0b01111000, 0],
    // 0x45 'E'
    [0b01111110, 0b01000000, 0b01000000, 0b01111100, 0b01000000, 0b01000000, 0b01111110, 0],
    // 0x46 'F'
    [0b01111110, 0b01000000, 0b01000000, 0b01111100, 0b01000000, 0b01000000, 0b01000000, 0],
    // 0x47 'G'
    [0b00111100, 0b01000010, 0b01000000, 0b01001110, 0b01000010, 0b01000010, 0b00111100, 0],
    // 0x48 'H'
    [0b01000010, 0b01000010, 0b01000010, 0b01111110, 0b01000010, 0b01000010, 0b01000010, 0],
    // 0x49 'I'
    [0b00111100, 0b00011000, 0b00011000, 0b00011000, 0b00011000, 0b00011000, 0b00111100, 0],
    // 0x4A 'J'
    [0b00000110, 0b00000010, 0b00000010, 0b00000010, 0b00000010, 0b01000010, 0b00111100, 0],
    // 0x4B 'K'
    [0b01000010, 0b01000100, 0b01001000, 0b01110000, 0b01001000, 0b01000100, 0b01000010, 0],
    // 0x4C 'L'
    [0b01000000, 0b01000000, 0b01000000, 0b01000000, 0b01000000, 0b01000000, 0b01111110, 0],
    // 0x4D 'M'
    [0b01000010, 0b01100110, 0b01011010, 0b01000010, 0b01000010, 0b01000010, 0b01000010, 0],
    // 0x4E 'N'
    [0b01000010, 0b01100010, 0b01010010, 0b01001010, 0b01000110, 0b01000010, 0b01000010, 0],
    // 0x4F 'O'
    [0b00111100, 0b01000010, 0b01000010, 0b01000010, 0b01000010, 0b01000010, 0b00111100, 0],
    // 0x50 'P'
    [0b01111100, 0b01000010, 0b01000010, 0b01111100, 0b01000000, 0b01000000, 0b01000000, 0],
    // 0x51 'Q'
    [0b00111100, 0b01000010, 0b01000010, 0b01000010, 0b01001010, 0b01000100, 0b00111010, 0],
    // 0x52 'R'
    [0b01111100, 0b01000010, 0b01000010, 0b01111100, 0b01001000, 0b01000100, 0b01000010, 0],
    // 0x53 'S'
    [0b00111100, 0b01000010, 0b01000000, 0b00111100, 0b00000010, 0b01000010, 0b00111100, 0],
    // 0x54 'T'
    [0b01111110, 0b00011000, 0b00011000, 0b00011000, 0b00011000, 0b00011000, 0b00011000, 0],
    // 0x55 'U'
    [0b01000010, 0b01000010, 0b01000010, 0b01000010, 0b01000010, 0b01000010, 0b00111100, 0],
    // 0x56 'V'
    [0b01000010, 0b01000010, 0b01000010, 0b01000010, 0b01000010, 0b00100100, 0b00011000, 0],
    // 0x57 'W'
    [0b01000010, 0b01000010, 0b01000010, 0b01000010, 0b01011010, 0b01100110, 0b01000010, 0],
    // 0x58 'X'
    [0b01000010, 0b01000010, 0b00100100, 0b00011000, 0b00100100, 0b01000010, 0b01000010, 0],
    // 0x59 'Y'
    [0b01000010, 0b01000010, 0b00100100, 0b00011000, 0b00011000, 0b00011000, 0b00011000, 0],
    // 0x5A 'Z'
    [0b01111110, 0b00000010, 0b00000100, 0b00011000, 0b00100000, 0b01000000, 0b01111110, 0],
    // 0x5B '['
    [0b00111100, 0b00110000, 0b00110000, 0b00110000, 0b00110000, 0b00110000, 0b00111100, 0],
    // 0x5C '\\'
    [0b10000000, 0b01000000, 0b00100000, 0b00010000, 0b00001000, 0b00000100, 0b00000010, 0],
    // 0x5D ']'
    [0b00111100, 0b00001100, 0b00001100, 0b00001100, 0b00001100, 0b00001100, 0b00111100, 0],
    // 0x5E '^'
    [0b00011000, 0b00100100, 0b01000010, 0,0,0,0,0],
    // 0x5F '_'
    [0,0,0,0,0,0,0, 0b11111111],
    // 0x60 '`'
    [0b00110000, 0b00011000, 0,0,0,0,0,0],
    // 0x61 'a'
    [0,0, 0b00111100, 0b00000010, 0b00111110, 0b01000010, 0b00111110, 0],
    // 0x62 'b'
    [0b01000000, 0b01000000, 0b01111100, 0b01000010, 0b01000010, 0b01000010, 0b01111100, 0],
    // 0x63 'c'
    [0,0, 0b00111100, 0b01000010, 0b01000000, 0b01000010, 0b00111100, 0],
    // 0x64 'd'
    [0b00000010, 0b00000010, 0b00111110, 0b01000010, 0b01000010, 0b01000010, 0b00111110, 0],
    // 0x65 'e'
    [0,0, 0b00111100, 0b01000010, 0b01111110, 0b01000000, 0b00111100, 0],
    // 0x66 'f'
    [0b00001100, 0b00010010, 0b00010000, 0b01111000, 0b00010000, 0b00010000, 0b00010000, 0],
    // 0x67 'g'
    [0,0, 0b00111110, 0b01000010, 0b00111110, 0b00000010, 0b00111100, 0],
    // 0x68 'h'
    [0b01000000, 0b01000000, 0b01111100, 0b01000010, 0b01000010, 0b01000010, 0b01000010, 0],
    // 0x69 'i'
    [0b00011000, 0,    0b00111000, 0b00011000, 0b00011000, 0b00011000, 0b00111100, 0],
    // 0x6A 'j'
    [0b00001100, 0,    0b00001100, 0b00001100, 0b00001100, 0b01001100, 0b00111000, 0],
    // 0x6B 'k'
    [0b01000000, 0b01000000, 0b01000100, 0b01001000, 0b01110000, 0b01001000, 0b01000100, 0],
    // 0x6C 'l'
    [0b00111000, 0b00011000, 0b00011000, 0b00011000, 0b00011000, 0b00011000, 0b00111100, 0],
    // 0x6D 'm'
    [0,0, 0b01100100, 0b01011010, 0b01000010, 0b01000010, 0b01000010, 0],
    // 0x6E 'n'
    [0,0, 0b01111100, 0b01000010, 0b01000010, 0b01000010, 0b01000010, 0],
    // 0x6F 'o'
    [0,0, 0b00111100, 0b01000010, 0b01000010, 0b01000010, 0b00111100, 0],
    // 0x70 'p'
    [0,0, 0b01111100, 0b01000010, 0b01111100, 0b01000000, 0b01000000, 0],
    // 0x71 'q'
    [0,0, 0b00111110, 0b01000010, 0b00111110, 0b00000010, 0b00000010, 0],
    // 0x72 'r'
    [0,0, 0b01011100, 0b01100010, 0b01000000, 0b01000000, 0b01000000, 0],
    // 0x73 's'
    [0,0, 0b00111110, 0b01000000, 0b00111100, 0b00000010, 0b01111100, 0],
    // 0x74 't'
    [0b00010000, 0b00010000, 0b01111100, 0b00010000, 0b00010000, 0b00010010, 0b00001100, 0],
    // 0x75 'u'
    [0,0, 0b01000010, 0b01000010, 0b01000010, 0b01000010, 0b00111110, 0],
    // 0x76 'v'
    [0,0, 0b01000010, 0b01000010, 0b01000010, 0b00100100, 0b00011000, 0],
    // 0x77 'w'
    [0,0, 0b01000010, 0b01000010, 0b01011010, 0b01100110, 0b01000010, 0],
    // 0x78 'x'
    [0,0, 0b01000010, 0b00100100, 0b00011000, 0b00100100, 0b01000010, 0],
    // 0x79 'y'
    [0,0, 0b01000010, 0b01000010, 0b00111110, 0b00000010, 0b00111100, 0],
    // 0x7A 'z'
    [0,0, 0b01111110, 0b00000100, 0b00011000, 0b00100000, 0b01111110, 0],
    // 0x7B '{'
    [0b00001100, 0b00011000, 0b00011000, 0b01110000, 0b00011000, 0b00011000, 0b00001100, 0],
    // 0x7C '|'
    [0b00011000, 0b00011000, 0b00011000, 0b00011000, 0b00011000, 0b00011000, 0b00011000, 0],
    // 0x7D '}'
    [0b00110000, 0b00011000, 0b00011000, 0b00001110, 0b00011000, 0b00011000, 0b00110000, 0],
    // 0x7E '~'
    [0b00110010, 0b01001100, 0,0,0,0,0,0],
];

/// Look up the 8×8 bitmap for an ASCII code point. Returns the
/// all-zero space glyph for code points outside the printable ASCII
/// subset this font covers — matching the §25.e fallback ("display a
/// Space character (0x20)") for inputs outside 0x20..=0xF7 *and* for
/// the 0x80..=0xF7 range this minimal font does not provide.
pub fn glyph(ch: u8) -> [u8; 8] {
    if (0x20..=0x7E).contains(&ch) {
        PRINTABLE_ASCII[(ch - 0x20) as usize]
    } else {
        // §25.e fallback: render a space.
        PRINTABLE_ASCII[0]
    }
}

/// True when `(col, row)` inside an 8×8 glyph cell is set.
///
/// Column 0 is the leftmost pixel (bit 7 of the row byte), row 0 is
/// the topmost. Out-of-range coordinates evaluate to `false`.
pub fn pixel(glyph: &[u8; 8], col: u8, row: u8) -> bool {
    if col >= GLYPH_WIDTH || row >= GLYPH_HEIGHT {
        return false;
    }
    let row_byte = glyph[row as usize];
    let mask = 1u8 << (7 - col);
    (row_byte & mask) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_table_length_matches_printable_ascii() {
        // 0x20..=0x7E is exactly 95 entries.
        assert_eq!(PRINTABLE_ASCII.len(), 0x7E - 0x20 + 1);
        assert_eq!(PRINTABLE_ASCII.len(), 95);
    }

    #[test]
    fn space_glyph_is_all_zero() {
        // §25.e fallback would render space for unsupported chars; the
        // space glyph itself must therefore be blank.
        assert_eq!(glyph(b' '), [0u8; 8]);
    }

    #[test]
    fn control_chars_fall_back_to_space() {
        // §25.e — anything < 0x20 or > 0xF7 is rendered as space.
        for ch in 0u8..0x20 {
            assert_eq!(glyph(ch), [0u8; 8], "ch=0x{ch:02X}");
        }
    }

    #[test]
    fn delete_and_high_chars_fall_back_to_space() {
        // 0x7F (DEL) and 0x80..=0xFF — this minimal font covers only
        // printable ASCII (0x20..=0x7E); everything else returns space.
        assert_eq!(glyph(0x7F), [0u8; 8]);
        assert_eq!(glyph(0xFF), [0u8; 8]);
        assert_eq!(glyph(0x80), [0u8; 8]);
        assert_eq!(glyph(0xF7), [0u8; 8]);
    }

    #[test]
    fn pixel_reads_from_msb_first() {
        // Construct an all-MSB-set glyph: every row has only bit 7
        // (the leftmost pixel) set.
        let g: [u8; 8] = [0b1000_0000; 8];
        for row in 0..GLYPH_HEIGHT {
            assert!(pixel(&g, 0, row));
            for col in 1..GLYPH_WIDTH {
                assert!(!pixel(&g, col, row), "col={col} row={row}");
            }
        }
    }

    #[test]
    fn pixel_out_of_range_is_false() {
        let g: [u8; 8] = [0xFF; 8];
        assert!(!pixel(&g, 8, 0));
        assert!(!pixel(&g, 0, 8));
        assert!(!pixel(&g, 255, 255));
    }

    #[test]
    fn printable_ascii_glyphs_are_nonempty_except_space() {
        // Every non-space printable character should set at least one
        // pixel; a blank glyph for, say, 'A' would silently degrade
        // rendering and indicate a font-table bug.
        for ch in 0x21u8..=0x7E {
            let g = glyph(ch);
            assert!(
                g.iter().any(|&row| row != 0),
                "glyph for 0x{ch:02X} ({}) is unexpectedly blank",
                ch as char
            );
        }
    }
}
