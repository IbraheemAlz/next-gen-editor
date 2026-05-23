//! `CT_PPr` (paragraph properties) — read-side helpers.
//!
//! Folds one `<w:pPr>` child element into an accumulating `ParaProperties`.
//! Phase 2 covers `<w:jc>`, `<w:ind>`, `<w:spacing>`, `<w:bidi>`,
//! `<w:keepNext>`, `<w:keepLines>`, `<w:pageBreakBefore>`. Later phases
//! grow the surface: `<w:pStyle>` (Phase 3), `<w:numPr>` (Phase 4),
//! `<w:pBdr>` / `<w:shd>` / `<w:tabs>` (later).
//!
//! Twip values land in the engine model verbatim; layout converts to px.

use crate::schema::ct_rpr::{attr_val, toggle_on};
use engine::{Alignment, Indent, LineHeight, ParaProperties, Spacing, TextDirection};
use quick_xml::events::BytesStart;

/// `<w:jc w:val="…"/>` → engine `Alignment`. Unknown values are dropped.
fn parse_jc(v: &str) -> Option<Alignment> {
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
        b"w:keepNext" => props.keep_next = toggle_on(e),
        b"w:keepLines" => props.keep_lines = toggle_on(e),
        b"w:pageBreakBefore" => props.page_break_before = toggle_on(e),
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
    fn keep_and_page_break_flags() {
        let p = parse_ppr(br#"<w:pPr><w:keepNext/><w:keepLines/><w:pageBreakBefore/></w:pPr>"#);
        assert!(p.keep_next);
        assert!(p.keep_lines);
        assert!(p.page_break_before);
    }
}
