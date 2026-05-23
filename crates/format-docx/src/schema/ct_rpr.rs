//! `CT_RPr` (run properties) — read-side helpers.
//!
//! Each `<w:r>`'s `<w:rPr>` element holds the run-level character formatting.
//! `apply_rpr` folds one child element into an accumulating `SpanStyle`. The
//! same helpers serve the paragraph-mark `<w:pPr>/<w:rPr>` (Phase 2) and
//! `styles.xml`'s `<w:style>/<w:rPr>` (Phase 3); kept here so the cascade
//! resolver can reuse them without depending on `parts::document`.

use engine::{FontFamily, SpanStyle};
use quick_xml::events::BytesStart;

/// Value of attribute `key` on a start/empty tag, unescaped.
pub fn attr_val(e: &BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

/// A 6-hex-digit `RRGGBB` colour → opaque RGBA. `auto` / malformed → `None`.
pub fn parse_hex_color(v: &str) -> Option<[u8; 4]> {
    let v = v.trim();
    if v.len() != 6 {
        return None;
    }
    let d = |i: usize| u8::from_str_radix(v.get(i..i + 2)?, 16).ok();
    Some([d(0)?, d(2)?, d(4)?, 255])
}

/// Word `<w:highlight>` uses a fixed named palette (arbitrary RGB lives in
/// `<w:shd w:fill>` instead). Map the names the engine can represent.
pub fn highlight_color(name: &str) -> Option<[u8; 4]> {
    let [r, g, b] = match name.trim().to_ascii_lowercase().as_str() {
        "yellow" => [255, 255, 0],
        "green" => [0, 255, 0],
        "cyan" => [0, 255, 255],
        "magenta" => [255, 0, 255],
        "blue" => [0, 0, 255],
        "red" => [255, 0, 0],
        "darkblue" => [0, 0, 139],
        "darkcyan" => [0, 139, 139],
        "darkgreen" => [0, 100, 0],
        "darkmagenta" => [139, 0, 139],
        "darkred" => [139, 0, 0],
        "darkyellow" => [128, 128, 0],
        "darkgray" => [169, 169, 169],
        "lightgray" => [211, 211, 211],
        "black" => [0, 0, 0],
        _ => return None,
    };
    Some([r, g, b, 255])
}

pub fn family_from_docx(name: &str) -> Option<FontFamily> {
    match name.trim().to_ascii_lowercase().as_str() {
        "amiri" => Some(FontFamily::Amiri),
        "liberation sans" | "liberation" => Some(FontFamily::LiberationSans),
        "noto naskh arabic" => Some(FontFamily::NotoNaskhArabic),
        _ => None,
    }
}

/// An OOXML toggle property: bare `<w:b/>` is on; `<w:b w:val="false"/>` off.
pub fn toggle_on(e: &BytesStart) -> bool {
    match attr_val(e, b"w:val") {
        Some(v) => !matches!(v.to_ascii_lowercase().as_str(), "false" | "0" | "off"),
        None => true,
    }
}

/// Fold one `<w:rPr>` child element into the run's accumulating style.
pub fn apply_rpr(name: &[u8], e: &BytesStart, style: &mut SpanStyle) {
    match name {
        b"w:b" => style.bold = Some(toggle_on(e)),
        b"w:i" => style.italic = Some(toggle_on(e)),
        b"w:strike" => style.strike = Some(toggle_on(e)),
        b"w:u" => {
            let on = attr_val(e, b"w:val").is_none_or(|v| !v.eq_ignore_ascii_case("none"));
            style.underline = Some(on);
        }
        b"w:color" => style.color = attr_val(e, b"w:val").and_then(|v| parse_hex_color(&v)),
        b"w:highlight" => {
            style.bg_color = attr_val(e, b"w:val").and_then(|v| highlight_color(&v));
        }
        b"w:shd" => style.bg_color = attr_val(e, b"w:fill").and_then(|v| parse_hex_color(&v)),
        b"w:rFonts" => {
            style.font_family = attr_val(e, b"w:ascii")
                .or_else(|| attr_val(e, b"w:hAnsi"))
                .or_else(|| attr_val(e, b"w:cs"))
                .and_then(|v| family_from_docx(&v));
        }
        _ => {}
    }
}
