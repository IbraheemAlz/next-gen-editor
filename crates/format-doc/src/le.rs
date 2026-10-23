//! Bounds-checked little-endian reads. Every structure of the format is
//! little-endian ([MS-DOC] 2.1.1); every offset is attacker-chosen, so a
//! read past the end is `None`, never a panic.

pub(crate) fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

pub(crate) fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// `b[from .. from + len]`, `None` when any of it lies outside `b`.
pub(crate) fn slice(b: &[u8], from: u32, len: u32) -> Option<&[u8]> {
    let from = from as usize;
    b.get(from..from.checked_add(len as usize)?)
}
