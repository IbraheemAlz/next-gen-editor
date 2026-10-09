//! `CT_RPr` (run properties) — read-side helpers.
//!
//! Each `<w:r>`'s `<w:rPr>` element holds the run-level character formatting.
//! `apply_rpr` folds one child element into an accumulating `SpanStyle`. The
//! same helpers serve the paragraph-mark `<w:pPr>/<w:rPr>` (Phase 2) and
//! `styles.xml`'s `<w:style>/<w:rPr>` (Phase 3); kept here so the cascade
//! resolver can reuse them without depending on `parts::document`.

use engine::{FontFamily, SpanStyle, UnderlineStyle, VertAlign};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

/// Issue #84 — `true` for every `<w:rPr>` child the model expresses (the
/// arms of [`apply_rpr`] plus `<w:rStyle>`, which the part parsers fold
/// through the character-style cascade). Everything else is captured
/// verbatim into the run's grab bag.
pub fn rpr_child_is_modeled(name: &[u8]) -> bool {
    matches!(
        name,
        b"w:rStyle"
            | b"w:rFonts"
            | b"w:b"
            | b"w:bCs"
            | b"w:i"
            | b"w:iCs"
            | b"w:caps"
            | b"w:smallCaps"
            | b"w:strike"
            | b"w:color"
            | b"w:sz"
            | b"w:szCs"
            | b"w:highlight"
            | b"w:u"
            | b"w:shd"
            | b"w:vertAlign"
    )
}

/// Issue #84 — rank of a `<w:rPr>` child in the `EG_RPrBase` listing
/// (ECMA-376 §17.3.2). The Transitional schema declares the group as an
/// unbounded choice, so any order validates; Word nonetheless writes the
/// listed order and `<w:rPrChange>` last, and the writer reproduces that
/// when it interleaves grab-bag fragments with the modeled children.
/// Unknown / foreign-namespace children rank just before the change
/// record.
pub fn rpr_child_rank(name: &[u8]) -> u16 {
    const ORDER: &[&[u8]] = &[
        b"w:rStyle",
        b"w:rFonts",
        b"w:b",
        b"w:bCs",
        b"w:i",
        b"w:iCs",
        b"w:caps",
        b"w:smallCaps",
        b"w:strike",
        b"w:dstrike",
        b"w:outline",
        b"w:shadow",
        b"w:emboss",
        b"w:imprint",
        b"w:noProof",
        b"w:snapToGrid",
        b"w:vanish",
        b"w:webHidden",
        b"w:color",
        b"w:spacing",
        b"w:w",
        b"w:kern",
        b"w:position",
        b"w:sz",
        b"w:szCs",
        b"w:highlight",
        b"w:u",
        b"w:effect",
        b"w:bdr",
        b"w:shd",
        b"w:fitText",
        b"w:vertAlign",
        b"w:rtl",
        b"w:cs",
        b"w:em",
        b"w:lang",
        b"w:eastAsianLayout",
        b"w:specVanish",
        b"w:oMath",
    ];
    schema_rank(ORDER, name, b"w:rPrChange")
}

/// Shared rank lookup: position in `order`, `u16::MAX` for the
/// change-tracking tail element, `u16::MAX - 1` for anything unknown (so
/// foreign-namespace extensions land after every schema child but before
/// the `*Change` record, matching Word's own layout).
pub(crate) fn schema_rank(order: &[&[u8]], name: &[u8], change_elem: &[u8]) -> u16 {
    if name == change_elem {
        return u16::MAX;
    }
    order
        .iter()
        .position(|n| *n == name)
        .map_or(u16::MAX - 1, |i| i as u16)
}

/// Issue #84 — fold the top-level children of a captured `<w:rPr>…</w:rPr>`
/// fragment into `style` through [`apply_rpr`]. Used for the
/// paragraph-mark run properties: the whole element rides the paragraph's
/// grab bag for round-trip, while its modeled children still seed the
/// run baseline exactly as before. Nested subtrees (`<w:rPrChange>`) are
/// skipped so the recorded *previous* formatting never overrides the
/// live one.
pub fn fold_rpr_fragment(fragment: &[u8], style: &mut SpanStyle) {
    let mut reader = Reader::from_reader(fragment);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut depth = 0u32;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if depth == 1 {
                    apply_rpr(e.name().as_ref(), &e, style);
                    let end = e.to_end().into_owned();
                    let mut skip = Vec::new();
                    if reader.read_to_end_into(end.name(), &mut skip).is_err() {
                        break;
                    }
                } else {
                    depth += 1;
                }
            }
            Ok(Event::Empty(e)) if depth == 1 => apply_rpr(e.name().as_ref(), &e, style),
            Ok(Event::End(_)) => depth = depth.saturating_sub(1),
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
}

/// Issue #293 — the paragraph mark's modeled run properties
/// (`engine::Paragraph::mark_style`) a captured `<w:pPr>/<w:rPr>`
/// fragment spells: its top-level children folded through
/// [`apply_rpr`] (no grab bag — the unmodeled children stay in the
/// fragment). The reader models the mark with it and the writer
/// verifies the fragment against the live mark with it, so the two can
/// never disagree.
pub fn mark_rpr_style(fragment: &[u8]) -> SpanStyle {
    let mut style = SpanStyle::default();
    fold_rpr_fragment(fragment, &mut style);
    style.grab_bag = None;
    style
}

/// Issue #293 — the top-level children of a captured `<w:rPr>` fragment
/// that [`mark_rpr_style`] does NOT model (everything outside
/// [`apply_rpr`]'s arms: `<w:lang>`, `<w:rtl>`, `<w:rStyle>`, foreign
/// extensions, the `<w:rPrChange>` history), verbatim, in source order —
/// what a regenerated mark `<w:rPr>` must keep.
pub fn unmodeled_rpr_children(fragment: &[u8]) -> Vec<Vec<u8>> {
    /* `<w:rStyle>` is "modeled" for runs (through the character-style
    cascade) but nothing models it on a mark. */
    let kept = |name: &[u8]| !rpr_child_is_modeled(name) || name == b"w:rStyle";
    let mut reader = Reader::from_reader(fragment);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut depth = 0u32;
    let mut out = Vec::new();
    loop {
        let start = reader.buffer_position() as usize;
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if depth == 1 => {
                let keep = kept(e.name().as_ref());
                let end = e.to_end().into_owned();
                let mut skip = Vec::new();
                if reader.read_to_end_into(end.name(), &mut skip).is_err() {
                    break;
                }
                if keep {
                    let stop = reader.buffer_position() as usize;
                    out.push(fragment[start..stop].to_vec());
                }
            }
            Ok(Event::Start(_)) => depth += 1,
            Ok(Event::Empty(e)) if depth == 1 && kept(e.name().as_ref()) => {
                let stop = reader.buffer_position() as usize;
                out.push(fragment[start..stop].to_vec());
            }
            Ok(Event::End(_)) => depth = depth.saturating_sub(1),
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

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

/// `<w:sz>` / `<w:szCs>` `w:val` (half-points) as points.
fn half_points(e: &BytesStart) -> Option<f32> {
    attr_val(e, b"w:val")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .map(|half_pts| (half_pts as f32) / 2.0)
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
        /* Issue #104 — the complex-script twins Word bolds / italicizes
        Arabic, Hebrew, Thai, … text by. */
        b"w:bCs" => style.bold_cs = Some(toggle_on(e)),
        b"w:i" => style.italic = Some(toggle_on(e)),
        b"w:iCs" => style.italic_cs = Some(toggle_on(e)),
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
        b"w:color" => {
            style.color = attr_val(e, b"w:val").and_then(|v| parse_hex_color(&v));
            /* Issue #355 — the theme colour layout resolves through. */
            style.color_theme = crate::parts::theme::theme_color_ref(e);
        }
        b"w:highlight" => {
            style.bg_color = attr_val(e, b"w:val").and_then(|v| highlight_color(&v));
        }
        b"w:shd" => style.bg_color = attr_val(e, b"w:fill").and_then(|v| parse_hex_color(&v)),
        b"w:rFonts" => {
            /* Audit gap A.M2 — accept ANY font name, not just the
            three the engine has loaded. Resolved names hit
            `font_family`; unresolved names park in `raw_font_family`
            so the writer round-trips them verbatim (Word reopens with
            the original face the author chose). The Latin theme
            attributes (`asciiTheme` / `hAnsiTheme`) still park in the
            legacy single-slot `font_theme`; every slot's binding rides
            `font_bindings` (issue #355), which the writer prefers. */
            let name = attr_val(e, b"w:ascii").or_else(|| attr_val(e, b"w:hAnsi"));
            if let Some(n) = name {
                match family_from_docx(&n) {
                    Some(fam) => style.font_family = Some(fam),
                    None => style.raw_font_family = Some(n),
                }
            }
            let theme = attr_val(e, b"w:asciiTheme").or_else(|| attr_val(e, b"w:hAnsiTheme"));
            if let Some(t) = theme {
                style.font_theme = Some(t);
            }
            /* Issue #249 — the complex-script slot stays apart: an
            Arabic run naming `w:ascii="Times New Roman"
            w:cs="Simplified Arabic"` shapes its Latin with the first and
            its Arabic with the second (`w:cs` alone no longer becomes the
            Latin face either). `w:eastAsia` / `w:hint` stay unmodeled —
            the writer carries them over from the source element. */
            let cs_family = attr_val(e, b"w:cs").and_then(|n| family_from_docx(&n));
            let cs_modeled = cs_family.is_some();
            if let Some(fam) = cs_family {
                style.font_family_cs = Some(fam);
            }
            /* Issue #355 — the per-slot bindings layout resolves through
            (name vs theme reference per ascii / hAnsi / eastAsia / cs);
            `w:cstheme` lives only here, on the `cs` slot. */
            style.font_bindings = crate::parts::theme::rfonts_bindings(e, cs_modeled);
        }
        /* `<w:sz w:val="N"/>` and `<w:szCs w:val="N"/>` — N is half-points
        (Word's native encoding; `w:val="24"` = 12 pt). `w:sz` sizes the
        Latin (ASCII + high-ANSI) characters, `w:szCs` the complex-script
        ones (Arabic, Hebrew, Thai, …). Issue #359 — two slots: Arabic
        documents routinely carry `w:sz="22" w:szCs="28"` on one run, and
        folding both into one size laid the Latin text out at 14 pt. */
        b"w:sz" => {
            if let Some(pt) = half_points(e) {
                style.font_size = Some(pt);
            }
        }
        b"w:szCs" => {
            if let Some(pt) = half_points(e) {
                style.font_size_cs = Some(pt);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #84 — the rank table is the EG_RPrBase listing (+ `rPrChange`
    /// last); every writer-emitted child has a rank, unknowns sort
    /// before the change record.
    #[test]
    fn rpr_ranks_follow_eg_rpr_base_listing() {
        let seq: [&[u8]; 14] = [
            b"w:rStyle",
            b"w:rFonts",
            b"w:b",
            b"w:i",
            b"w:caps",
            b"w:smallCaps",
            b"w:strike",
            b"w:noProof",
            b"w:color",
            b"w:sz",
            b"w:szCs",
            b"w:u",
            b"w:shd",
            b"w:vertAlign",
        ];
        for w in seq.windows(2) {
            assert!(rpr_child_rank(w[0]) < rpr_child_rank(w[1]));
        }
        assert!(rpr_child_rank(b"w:vertAlign") < rpr_child_rank(b"w:lang"));
        assert!(rpr_child_rank(b"w:lang") < rpr_child_rank(b"w:eastAsianLayout"));
        assert!(rpr_child_rank(b"w:eastAsianLayout") < rpr_child_rank(b"w14:glow"));
        assert!(rpr_child_rank(b"w14:glow") < rpr_child_rank(b"w:rPrChange"));
        assert!(rpr_child_is_modeled(b"w:highlight"));
        assert!(rpr_child_is_modeled(b"w:rStyle"));
        assert!(rpr_child_is_modeled(b"w:bCs"));
        assert!(rpr_child_is_modeled(b"w:iCs"));
        assert!(!rpr_child_is_modeled(b"w:lang"));
    }

    /// Issue #84 — folding a captured paragraph-mark `<w:rPr>` applies
    /// its top-level modeled children and skips nested history.
    #[test]
    fn fold_rpr_fragment_applies_top_level_children_only() {
        let frag = br#"<w:rPr><w:b/><w:sz w:val="28"/><w:rPrChange w:id="1"><w:rPr><w:i/><w:sz w:val="48"/></w:rPr></w:rPrChange></w:rPr>"#;
        let mut style = SpanStyle::default();
        fold_rpr_fragment(frag, &mut style);
        assert_eq!(style.bold, Some(true));
        assert_eq!(style.font_size, Some(14.0));
        assert_eq!(style.italic, None, "nested rPrChange must not apply");
        let mut empty = SpanStyle::default();
        fold_rpr_fragment(b"<w:rPr/>", &mut empty);
        assert_eq!(empty, SpanStyle::default());
    }

    /// Issue #293 — a mark `<w:rPr>` splits into the modeled style and the
    /// verbatim unmodeled children (`<w:rStyle>` included: nothing models
    /// it on a mark; nested `<w:rPrChange>` content never folds).
    #[test]
    fn mark_rpr_splits_into_modeled_style_and_unmodeled_children() {
        let frag = br#"<w:rPr><w:rStyle w:val="Strong"/><w:b/><w:lang w:val="en-GB"/><w:sz w:val="20"/><w:rPrChange w:id="1" w:author="A"><w:rPr><w:i/></w:rPr></w:rPrChange></w:rPr>"#;
        let style = mark_rpr_style(frag);
        assert_eq!(style.bold, Some(true));
        assert_eq!(style.font_size, Some(10.0));
        assert_eq!(style.italic, None, "history never folds");
        assert!(style.grab_bag.is_none());
        let kept: Vec<String> = unmodeled_rpr_children(frag)
            .into_iter()
            .map(|c| String::from_utf8(c).unwrap())
            .collect();
        assert_eq!(
            kept,
            vec![
                r#"<w:rStyle w:val="Strong"/>"#.to_string(),
                r#"<w:lang w:val="en-GB"/>"#.to_string(),
                r#"<w:rPrChange w:id="1" w:author="A"><w:rPr><w:i/></w:rPr></w:rPrChange>"#
                    .to_string(),
            ]
        );
        assert!(unmodeled_rpr_children(b"<w:rPr/>").is_empty());
    }
}
