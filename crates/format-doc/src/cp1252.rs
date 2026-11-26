//! Windows-1252 decoding for the 8-bit ("compressed") pieces of the piece
//! table.
//!
//! [MS-DOC] 2.9.73 FcCompressed: when `fCompressed` is 1 the text is one
//! byte per character, and the bytes `0x80..=0x9F` stand for the Unicode
//! characters of the table that section lists; every other byte is the
//! code point of the same value. That table is exactly the Windows-1252
//! upper control range, written out below (the five bytes Windows-1252
//! leaves unassigned decode to themselves: the spec's table does not list
//! them).

/// Unicode scalar for each byte `0x80..=0x9F`.
const HIGH: [u16; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, // 80-87
    0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x008D, 0x017D, 0x008F, // 88-8F
    0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, // 90-97
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178, // 98-9F
];

/// The character a compressed-piece byte stands for.
pub(crate) fn decode(b: u8) -> char {
    match b {
        0x80..=0x9F => char::from_u32(u32::from(HIGH[usize::from(b - 0x80)])).unwrap_or('\u{FFFD}'),
        _ => char::from(b),
    }
}

/// The byte a character encodes to in a compressed piece, if it has one
/// (the test builder's direction).
#[doc(hidden)]
pub fn encode(c: char) -> Option<u8> {
    let u = u32::from(c);
    if u < 0x80 || (0xA0..=0xFF).contains(&u) {
        return Some(u as u8);
    }
    HIGH.iter()
        .position(|&h| u32::from(h) == u)
        .map(|i| 0x80 + i as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_upper_control_range_is_windows_1252() {
        assert_eq!(decode(0x80), '€');
        assert_eq!(decode(0x93), '\u{201C}');
        assert_eq!(decode(0x9F), 'Ÿ');
        assert_eq!(decode(0xE9), 'é');
        assert_eq!(decode(b'A'), 'A');
        for b in 0..=255u8 {
            assert_eq!(encode(decode(b)), Some(b), "byte 0x{b:02X}");
        }
    }
}
