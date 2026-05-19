//! Justification modes.
//!
//! Real Kashida implementation requires Unicode joining_type lookup + per-font
//! priority bands (Microsoft "Arabic Typography Guide" / ISO/IEC TR 14652).
//! Phase 1 PoC: a coarse "elongate connecting Arabic glyphs uniformly"
//! strategy. Documented as approximation; native-typography review will
//! refine in Phase 3 per PHASE_3_RENDER_RTL.md §6.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Start,
    End,
    Center,
    Justify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JustifyMode {
    None,
    /// Distribute extra width across U+0020 spaces (Latin-style).
    Space,
    /// Distribute extra width across cursive-joining Arabic boundaries.
    Kashida,
    /// Use Kashida for the Arabic portion + space for the Latin portion.
    Mixed,
}

/// Quick test for "in the Arabic Unicode block" — used to decide between
/// space-justify and kashida-justify on a per-glyph basis.
pub fn is_arabic_codepoint(c: char) -> bool {
    matches!(
        c as u32,
        0x0600..=0x06FF      // Arabic
        | 0x0750..=0x077F    // Arabic Supplement
        | 0x08A0..=0x08FF    // Arabic Extended-A
        | 0xFB50..=0xFDFF    // Arabic Presentation Forms-A
        | 0xFE70..=0xFEFF    // Arabic Presentation Forms-B
    )
}
