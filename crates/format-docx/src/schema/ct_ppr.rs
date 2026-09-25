//! `CT_PPr` (paragraph properties) — read-side helpers.
//!
//! Folds one `<w:pPr>` child element into an accumulating `ParaProperties`.
//! Phase 2 covers `<w:jc>`, `<w:ind>`, `<w:spacing>`, `<w:bidi>`,
//! `<w:keepNext>`, `<w:keepLines>`, `<w:pageBreakBefore>`. Later phases
//! grew the surface: `<w:pStyle>` (Phase 3), `<w:numPr>` (Phase 4),
//! `<w:shd>` (fill → `shading`); `<w:pBdr>` / `<w:tabs>` fold in
//! `parts::document`.
//!
//! Twip values land in the engine model verbatim; layout converts to px.

use crate::schema::ct_rpr::{attr_val, parse_hex_color, schema_rank, toggle_on};
use engine::{Alignment, Indent, LineHeight, ParaProperties, Spacing, TextDirection};
use quick_xml::events::BytesStart;

/// Issue #84 — `true` for every `<w:pPr>` child the reader consumes: the
/// arms of [`apply_ppr`] plus the container / reference children the
/// part parsers handle in their own loops (`pStyle`, `numPr`, `pBdr`,
/// `tabs`, `sectPr`). The paragraph-mark `<w:rPr>` is deliberately NOT
/// here — the writer never regenerates it, so the whole element rides
/// the paragraph's grab bag (its modeled children still seed the run
/// baseline via `ct_rpr::fold_rpr_fragment`).
pub fn ppr_child_is_modeled(name: &[u8]) -> bool {
    matches!(
        name,
        b"w:pStyle"
            | b"w:keepNext"
            | b"w:keepLines"
            | b"w:pageBreakBefore"
            | b"w:numPr"
            | b"w:pBdr"
            | b"w:shd"
            | b"w:tabs"
            | b"w:bidi"
            | b"w:spacing"
            | b"w:ind"
            | b"w:jc"
            | b"w:sectPr"
    )
}

/// Issue #84 — rank of a `<w:pPr>` child in the `CT_PPrBase` sequence
/// (ECMA-376 §17.3.1.26) followed by the `CT_PPr` tail (`rPr`, `sectPr`,
/// `pPrChange`). Unlike `rPr`, this IS a strict sequence — strict
/// validators and Word's repair dialog reject out-of-order children — so
/// the writer emits modeled children and grab-bag fragments interleaved
/// by this rank. Unknown children rank just before `pPrChange`.
pub fn ppr_child_rank(name: &[u8]) -> u16 {
    const ORDER: &[&[u8]] = &[
        b"w:pStyle",
        b"w:keepNext",
        b"w:keepLines",
        b"w:pageBreakBefore",
        b"w:framePr",
        b"w:widowControl",
        b"w:numPr",
        b"w:suppressLineNumbers",
        b"w:pBdr",
        b"w:shd",
        b"w:tabs",
        b"w:suppressAutoHyphens",
        b"w:kinsoku",
        b"w:wordWrap",
        b"w:overflowPunct",
        b"w:topLinePunct",
        b"w:autoSpaceDE",
        b"w:autoSpaceDN",
        b"w:bidi",
        b"w:adjustRightInd",
        b"w:snapToGrid",
        b"w:spacing",
        b"w:ind",
        b"w:contextualSpacing",
        b"w:mirrorIndents",
        b"w:suppressOverlap",
        b"w:jc",
        b"w:textDirection",
        b"w:textAlignment",
        b"w:textboxTightWrap",
        b"w:outlineLvl",
        b"w:divId",
        b"w:cnfStyle",
        b"w:rPr",
        b"w:sectPr",
    ];
    schema_rank(ORDER, name, b"w:pPrChange")
}

/// `<w:jc w:val="…"/>` → engine `Alignment`. Unknown values are dropped.
pub(crate) fn parse_jc(v: &str) -> Option<Alignment> {
    match v.trim().to_ascii_lowercase().as_str() {
        /* Word emits `start` / `end` (writing-direction-relative) in modern
        docs and `left` / `right` (absolute) in older / Strict ones; treat
        both forms as writing-direction-relative because that's how the
        engine resolves them at layout time. `both` is justified. */
        "start" | "left" => Some(Alignment::Start),
        "end" | "right" => Some(Alignment::End),
        "center" => Some(Alignment::Center),
        "both" | "distribute" => Some(Alignment::Justify),
        _ => None,
    }
}

/// Parse a signed twip attribute. Returns `None` if absent or malformed.
fn attr_twips(e: &BytesStart, key: &[u8]) -> Option<i32> {
    attr_val(e, key).and_then(|v| v.trim().parse::<i32>().ok())
}

/// Parse an unsigned twip attribute (used for `<w:ind w:firstLine>` /
/// `<w:hanging>` / `<w:spacing w:before>` / `<w:after>`).
fn attr_utwips(e: &BytesStart, key: &[u8]) -> Option<i32> {
    attr_val(e, key)
        .and_then(|v| v.trim().parse::<u32>().ok())
        .map(|v| v as i32)
}

/// `<w:ind w:start|left="…" w:end|right="…" w:firstLine="…" w:hanging="…"/>`.
fn apply_ind(e: &BytesStart, ind: &mut Indent) {
    if let Some(v) = attr_twips(e, b"w:start").or_else(|| attr_twips(e, b"w:left")) {
        ind.start_twips = v;
    }
    if let Some(v) = attr_twips(e, b"w:end").or_else(|| attr_twips(e, b"w:right")) {
        ind.end_twips = v;
    }
    /* firstLine and hanging are mutually exclusive in OOXML — `<w:hanging>`
    overrides `<w:firstLine>`. Engine stores both, zeroing the other. */
    if let Some(v) = attr_utwips(e, b"w:hanging") {
        ind.hanging_twips = v;
        ind.first_line_twips = 0;
    } else if let Some(v) = attr_utwips(e, b"w:firstLine") {
        ind.first_line_twips = v;
        ind.hanging_twips = 0;
    }
}

/// `<w:spacing w:before w:after w:line w:lineRule>` — fills both
/// `Spacing` (vertical gap) and `LineHeight` (line rule).
fn apply_spacing(e: &BytesStart, spacing: &mut Spacing, line_height: &mut Option<LineHeight>) {
    if let Some(v) = attr_utwips(e, b"w:before") {
        spacing.before_twips = v;
    }
    if let Some(v) = attr_utwips(e, b"w:after") {
        spacing.after_twips = v;
    }
    if let Some(line) = attr_twips(e, b"w:line") {
        *line_height = Some(match attr_val(e, b"w:lineRule").as_deref() {
            Some("exact") => LineHeight::Exact { twips: line },
            Some("atLeast") => LineHeight::AtLeast { twips: line },
            /* default rule is `auto` — `w:line` is a 240-ths multiple. */
            _ => LineHeight::Auto { twips: line },
        });
    }
}

/// Fold one `<w:pPr>` child element into `props`.
pub fn apply_ppr(name: &[u8], e: &BytesStart, props: &mut ParaProperties) {
    match name {
        b"w:jc" => {
            props.alignment = attr_val(e, b"w:val").and_then(|v| parse_jc(&v));
        }
        b"w:ind" => apply_ind(e, &mut props.indent),
        b"w:spacing" => apply_spacing(e, &mut props.spacing, &mut props.line_height),
        b"w:bidi" => {
            props.direction = Some(if toggle_on(e) {
                TextDirection::Rtl
            } else {
                TextDirection::Ltr
            });
        }
        b"w:shd" => {
            /* `w:fill="auto"` / malformed hex → `None` (shading cleared),
            matching the cell `<w:tcPr><w:shd>` path. */
            props.shading = attr_val(e, b"w:fill").and_then(|v| parse_hex_color(&v));
        }
        b"w:keepNext" => props.keep_next = toggle_on(e),
        b"w:keepLines" => props.keep_lines = toggle_on(e),
        b"w:pageBreakBefore" => props.page_break_before = toggle_on(e),
        /* Issue #81 — read-only (see `ParaProperties::outline_level`):
        styles.xml feeds the TOC heading cascade; a direct one also
        rides the grab bag verbatim. */
        b"w:outlineLvl" => {
            props.outline_level = attr_val(e, b"w:val")
                .and_then(|v| v.trim().parse::<u8>().ok())
                .filter(|l| *l <= 9);
        }
        /* Issue #95 — read-only like `outlineLvl`: the direct element
        rides the grab bag verbatim (it is not in `ppr_child_is_modeled`),
        the model value feeds layout's widow / orphan control. */
        b"w:widowControl" => props.widow_control = Some(toggle_on(e)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    /// Drive `apply_ppr` from a literal `<w:pPr>` snippet and return the
    /// accumulated properties.
    fn parse_ppr(xml: &[u8]) -> ParaProperties {
        let mut r = Reader::from_reader(xml);
        r.config_mut().trim_text(true);
        let mut props = ParaProperties::default();
        let mut buf = Vec::new();
        let mut in_ppr = false;
        loop {
            match r.read_event_into(&mut buf).unwrap() {
                Event::Start(e) if e.name().as_ref() == b"w:pPr" => in_ppr = true,
                Event::End(e) if e.name().as_ref() == b"w:pPr" => in_ppr = false,
                Event::Empty(e) if in_ppr => apply_ppr(e.name().as_ref(), &e, &mut props),
                Event::Start(e) if in_ppr => apply_ppr(e.name().as_ref(), &e, &mut props),
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
        }
        props
    }

    /// Issue #84 — the rank table is the CT_PPrBase sequence + CT_PPr
    /// tail; every modeled child has a rank and unknowns sort before
    /// `pPrChange`.
    #[test]
    fn ppr_ranks_follow_ct_ppr_sequence() {
        let seq: [&[u8]; 14] = [
            b"w:pStyle",
            b"w:keepNext",
            b"w:pageBreakBefore",
            b"w:framePr",
            b"w:numPr",
            b"w:pBdr",
            b"w:shd",
            b"w:tabs",
            b"w:bidi",
            b"w:spacing",
            b"w:ind",
            b"w:jc",
            b"w:cnfStyle",
            b"w:rPr",
        ];
        for w in seq.windows(2) {
            assert!(
                ppr_child_rank(w[0]) < ppr_child_rank(w[1]),
                "{:?} must precede {:?}",
                String::from_utf8_lossy(w[0]),
                String::from_utf8_lossy(w[1])
            );
        }
        assert!(ppr_child_rank(b"w:rPr") < ppr_child_rank(b"w:sectPr"));
        assert!(ppr_child_rank(b"w:sectPr") < ppr_child_rank(b"mc:AlternateContent"));
        assert!(ppr_child_rank(b"mc:AlternateContent") < ppr_child_rank(b"w:pPrChange"));
        for name in seq {
            if name == b"w:framePr" || name == b"w:cnfStyle" || name == b"w:rPr" {
                assert!(!ppr_child_is_modeled(name));
            } else {
                assert!(
                    ppr_child_is_modeled(name),
                    "{}",
                    String::from_utf8_lossy(name)
                );
            }
        }
    }

    #[test]
    fn jc_center() {
        let p = parse_ppr(br#"<w:pPr><w:jc w:val="center"/></w:pPr>"#);
        assert_eq!(p.alignment, Some(Alignment::Center));
    }

    #[test]
    fn jc_synonyms() {
        assert_eq!(
            parse_ppr(br#"<w:pPr><w:jc w:val="left"/></w:pPr>"#).alignment,
            Some(Alignment::Start)
        );
        assert_eq!(
            parse_ppr(br#"<w:pPr><w:jc w:val="both"/></w:pPr>"#).alignment,
            Some(Alignment::Justify)
        );
    }

    #[test]
    fn ind_first_line_clears_hanging() {
        let p = parse_ppr(br#"<w:pPr><w:ind w:start="720" w:firstLine="360"/></w:pPr>"#);
        assert_eq!(p.indent.start_twips, 720);
        assert_eq!(p.indent.first_line_twips, 360);
        assert_eq!(p.indent.hanging_twips, 0);
    }

    #[test]
    fn ind_hanging_overrides_first_line() {
        let p = parse_ppr(br#"<w:pPr><w:ind w:firstLine="360" w:hanging="240"/></w:pPr>"#);
        assert_eq!(p.indent.hanging_twips, 240);
        assert_eq!(p.indent.first_line_twips, 0);
    }

    #[test]
    fn spacing_before_after_and_line() {
        let p = parse_ppr(
            br#"<w:pPr><w:spacing w:before="120" w:after="240" w:line="360" w:lineRule="auto"/></w:pPr>"#,
        );
        assert_eq!(p.spacing.before_twips, 120);
        assert_eq!(p.spacing.after_twips, 240);
        assert_eq!(p.line_height, Some(LineHeight::Auto { twips: 360 }));
    }

    #[test]
    fn spacing_line_rule_exact() {
        let p = parse_ppr(br#"<w:pPr><w:spacing w:line="480" w:lineRule="exact"/></w:pPr>"#);
        assert_eq!(p.line_height, Some(LineHeight::Exact { twips: 480 }));
    }

    #[test]
    fn bidi_rtl_toggle_on() {
        let p = parse_ppr(br#"<w:pPr><w:bidi/></w:pPr>"#);
        assert_eq!(p.direction, Some(TextDirection::Rtl));
    }

    #[test]
    fn bidi_explicit_off() {
        let p = parse_ppr(br#"<w:pPr><w:bidi w:val="false"/></w:pPr>"#);
        assert_eq!(p.direction, Some(TextDirection::Ltr));
    }

    #[test]
    fn shd_fill_parses_into_shading() {
        let p =
            parse_ppr(br#"<w:pPr><w:shd w:val="clear" w:color="auto" w:fill="A5D6A7"/></w:pPr>"#);
        assert_eq!(p.shading, Some([0xA5, 0xD6, 0xA7, 255]));
    }

    #[test]
    fn shd_fill_auto_clears_shading() {
        let p = parse_ppr(br#"<w:pPr><w:shd w:val="clear" w:fill="auto"/></w:pPr>"#);
        assert_eq!(p.shading, None);
    }

    #[test]
    fn keep_and_page_break_flags() {
        let p = parse_ppr(br#"<w:pPr><w:keepNext/><w:keepLines/><w:pageBreakBefore/></w:pPr>"#);
        assert!(p.keep_next);
        assert!(p.keep_lines);
        assert!(p.page_break_before);
    }

    /// Issue #95 — `<w:widowControl>` is read into the model (tri-state:
    /// an explicit off must be able to override an inherited on) while
    /// the direct element stays unmodeled — it rides the grab bag.
    #[test]
    fn widow_control_is_read_tri_state_and_stays_in_the_grab_bag() {
        assert_eq!(parse_ppr(br#"<w:pPr/>"#).widow_control, None);
        assert_eq!(
            parse_ppr(br#"<w:pPr><w:widowControl/></w:pPr>"#).widow_control,
            Some(true)
        );
        for off in ["0", "false", "off"] {
            let xml = format!(r#"<w:pPr><w:widowControl w:val="{off}"/></w:pPr>"#);
            assert_eq!(parse_ppr(xml.as_bytes()).widow_control, Some(false));
        }
        assert!(!ppr_child_is_modeled(b"w:widowControl"));
        let style_on = ParaProperties {
            widow_control: Some(true),
            ..Default::default()
        };
        let direct_off = ParaProperties {
            widow_control: Some(false),
            ..Default::default()
        };
        assert_eq!(
            style_on.clone().merged_with(direct_off).widow_control,
            Some(false)
        );
        assert_eq!(
            style_on
                .merged_with(ParaProperties::default())
                .widow_control,
            Some(true)
        );
    }
}
