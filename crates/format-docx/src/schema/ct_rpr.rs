//! `CT_RPr` (run properties) — read-side helpers.
//!
//! Each `<w:r>`'s `<w:rPr>` element holds the run-level character formatting.
//! `apply_rpr` folds one child element into an accumulating `SpanStyle`. The
//! same helpers serve the paragraph-mark `<w:pPr>/<w:rPr>` (Phase 2) and
//! `styles.xml`'s `<w:style>/<w:rPr>` (Phase 3); kept here so the cascade
//! resolver can reuse them without depending on `parts::document`.

use engine::{FontFamily, SpanStyle, UnderlineStyle, VertAlign};
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
    /* Issue #23 — resolve the three seed faces by name and preserve every
    other `<w:rFonts w:ascii>` value as a `Custom` face (verbatim display
    string + slugified resolution id), so a custom font both renders (when
    its id is loaded) and round-trips byte-identically. `None` only for an
    empty name, which then parks in `raw_font_family`. */
    FontFamily::from_display_name(name)
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
        b"w:caps" => style.caps = Some(toggle_on(e)),
        b"w:smallCaps" => style.small_caps = Some(toggle_on(e)),
        b"w:vertAlign" => {
            /* Audit gap A.M1 — `<w:vertAlign w:val="superscript|subscript|
            baseline"/>`. Unknown values collapse to `Baseline` (defensive). */
            style.vert_align = Some(match attr_val(e, b"w:val").as_deref().map(str::trim) {
                Some("superscript") => VertAlign::Superscript,
                Some("subscript") => VertAlign::Subscript,
                _ => VertAlign::Baseline,
            });
        }
        b"w:u" => {
            /* `<w:u/>` (no `w:val`) → single. `<w:u w:val="none"/>` → none,
            preserved so it can override an inherited underline.
            Everything else maps to the closest variant the engine
            represents; unknown values collapse to single. The spec
            catalogues `single`, `words`, `double`, `thick`, `dotted`,
            `dottedHeavy`, `dash`, `dashedHeavy`, `dashLong`,
            `dashLongHeavy`, `dotDash`, `dashDotHeavy`, `dotDotDash`,
            `dashDotDotHeavy`, `wave`, `wavyHeavy`, `wavyDouble` — most
            collapse to `Dotted` / `Dashed` / `Wavy` for the renderer. */
            let variant = match attr_val(e, b"w:val").as_deref().map(str::trim) {
                None | Some("single") | Some("words") | Some("thick") => UnderlineStyle::Single,
                Some("double") | Some("wavyDouble") => UnderlineStyle::Double,
                Some("dotted") | Some("dottedHeavy") => UnderlineStyle::Dotted,
                Some("dash")
                | Some("dashedHeavy")
                | Some("dashLong")
                | Some("dashLongHeavy")
                | Some("dotDash")
                | Some("dashDotHeavy")
                | Some("dotDotDash")
                | Some("dashDotDotHeavy") => UnderlineStyle::Dashed,
                Some("wave") | Some("wavyHeavy") => UnderlineStyle::Wavy,
                Some(v) if v.eq_ignore_ascii_case("none") => UnderlineStyle::None,
                Some(_) => UnderlineStyle::Single,
            };
            style.underline = Some(variant);
        }
        b"w:color" => style.color = attr_val(e, b"w:val").and_then(|v| parse_hex_color(&v)),
        b"w:highlight" => {
            style.bg_color = attr_val(e, b"w:val").and_then(|v| highlight_color(&v));
        }
        b"w:shd" => style.bg_color = attr_val(e, b"w:fill").and_then(|v| parse_hex_color(&v)),
        b"w:rFonts" => {
            /* Audit gap A.M2 — accept ANY font name, not just the
            three the engine has loaded. Resolved names hit
            `font_family`; unresolved names park in `raw_font_family`
            so the writer round-trips them verbatim (Word reopens with
            the original face the author chose). Theme attributes
            (`asciiTheme` / `hAnsiTheme` / `cstheme`) park in
            `font_theme` — Word's "Update Style" depends on them. */
            let name = attr_val(e, b"w:ascii")
                .or_else(|| attr_val(e, b"w:hAnsi"))
                .or_else(|| attr_val(e, b"w:cs"));
            if let Some(n) = name {
                match family_from_docx(&n) {
                    Some(fam) => style.font_family = Some(fam),
                    None => style.raw_font_family = Some(n),
                }
            }
            let theme = attr_val(e, b"w:asciiTheme")
                .or_else(|| attr_val(e, b"w:hAnsiTheme"))
                .or_else(|| attr_val(e, b"w:cstheme"));
            if let Some(t) = theme {
                style.font_theme = Some(t);
            }
        }
        /* `<w:sz w:val="N"/>` and `<w:szCs w:val="N"/>` — N is half-points
        (Word's native encoding; `w:val="24"` = 12 pt). `w:sz` targets ASCII
        + high-ANSI runs; `w:szCs` targets complex-script (Arabic, Hebrew,
        Thai) runs. The engine carries one `font_size` slot, so both fold
        into it; `w:szCs` wins when both appear because OOXML lists it
        second in CT_RPr and complex-script docs depend on it. */
        b"w:sz" | b"w:szCs" => {
            if let Some(half_pts) = attr_val(e, b"w:val").and_then(|v| v.trim().parse::<u32>().ok())
            {
                style.font_size = Some((half_pts as f32) / 2.0);
            }
        }
        _ => {}
    }
}
