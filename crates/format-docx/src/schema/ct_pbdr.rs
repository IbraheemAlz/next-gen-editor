//! `CT_PBdr` (paragraph borders, `<w:pPr><w:pBdr>`) — read-side helpers
//! shared by the body / story parser (`parts::document`) and the style
//! sheet (`parts::styles`, issue #395).
//!
//! - Physical edges (`<w:top>`, `<w:left>`, `<w:bottom>`, `<w:right>`,
//!   `<w:between>`) land in their `CellBorders` slot as read.
//! - An explicit "no border" (`w:val="nil"` / `"none"`) is a SET edge — a
//!   `BorderStyle::None` stroke, which nothing paints — so it overrides an
//!   edge the style cascade would otherwise supply (issue #395; per-edge
//!   cascade in `engine::ParaProperties::merged_with`).
//! - Logical edges (`<w:start>` / `<w:end>`, issue #352) are collected by
//!   [`PbdrLogical`] and folded once the owner's own direction is known
//!   (`<w:bidi>` follows `<w:pBdr>` in `CT_PPrBase`), into the slot that
//!   direction names; the spelling flags record them. The cascade
//!   re-orients them to the paragraph's FINAL direction
//!   (`engine::ParaProperties::cascade`).

use crate::schema::ct_rpr::{attr_val, parse_hex_color};
use engine::{BorderStroke, BorderStyle, CellBorders, ParaProperties, TextDirection};
use quick_xml::events::BytesStart;

/// Parse `<w:top|left|bottom|right|between|start|end w:val w:sz w:color/>`
/// into a stroke. `w:val="none"` / `"nil"` is a `BorderStyle::None`
/// stroke (an explicit "no border"); a missing `w:val` (malformed) is
/// `None` — the edge is ignored.
pub(crate) fn parse_pbdr_stroke(e: &BytesStart) -> Option<BorderStroke> {
    let val = attr_val(e, b"w:val")?.trim().to_ascii_lowercase();
    let style = match val.as_str() {
        "none" | "nil" => BorderStyle::None,
        "single" => BorderStyle::Single,
        "double" => BorderStyle::Double,
        "dotted" => BorderStyle::Dotted,
        "dashed" => BorderStyle::Dashed,
        other => BorderStyle::Other(other.to_string()),
    };
    let none = matches!(style, BorderStyle::None);
    let size_eighth_pt: u16 = attr_val(e, b"w:sz")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(if none { 0 } else { 4 });
    let color = attr_val(e, b"w:color").and_then(|v| {
        if none || v.trim().eq_ignore_ascii_case("auto") {
            None
        } else {
            parse_hex_color(&v)
        }
    });
    Some(BorderStroke {
        style,
        size_eighth_pt,
        color,
    })
}

/// Audit gap A.M4 — fold one PHYSICAL `<w:pBdr>` edge child into
/// `props.borders`. Unknown edge names (`<w:bar>`, future extensions) are
/// ignored; the logical ones go through [`PbdrLogical::accept`] first.
pub(crate) fn apply_pbdr_edge(name: &[u8], e: &BytesStart, props: &mut ParaProperties) {
    let Some(stroke) = parse_pbdr_stroke(e) else {
        return;
    };
    let borders = props.borders.get_or_insert_with(CellBorders::default);
    match name {
        b"w:top" => borders.top = Some(stroke),
        b"w:left" => borders.left = Some(stroke),
        b"w:bottom" => borders.bottom = Some(stroke),
        b"w:right" => borders.right = Some(stroke),
        /* `<w:between>` is the "inside-horizontal" border between
        consecutive same-pBdr paragraphs. The engine has no
        multi-paragraph border collapse yet — stored on `inside_h` for
        round-trip; the renderer ignores it. */
        b"w:between" => borders.inside_h = Some(stroke),
        _ => {}
    }
}

/// Issue #352 — the logical `<w:start>` / `<w:end>` edges of a `<w:pBdr>`
/// (ISO 29500; ECMA-376 2nd ed. and later accept them in Transitional
/// too), collected during the parse and mapped to a physical side only
/// once the owner's direction is final.
#[derive(Default, Debug, Clone)]
pub(crate) struct PbdrLogical {
    start: Option<BorderStroke>,
    end: Option<BorderStroke>,
}

impl PbdrLogical {
    /// Take `<w:start>` / `<w:end>`; `false` for every other edge name
    /// (the caller then applies the physical-edge path).
    pub(crate) fn accept(&mut self, name: &[u8], e: &BytesStart) -> bool {
        match name {
            b"w:start" => {
                self.start = parse_pbdr_stroke(e);
                true
            }
            b"w:end" => {
                self.end = parse_pbdr_stroke(e);
                true
            }
            _ => false,
        }
    }

    /// Land the collected edges on `target` by ITS OWN direction (start =
    /// left unless `target.direction` is right-to-left, end the
    /// opposite) and flag the spelling — the convention
    /// `engine::ParaProperties::oriented_borders` documents. Call it once
    /// `target`'s direct `<w:bidi>` has been read: on a paragraph's direct
    /// properties before the cascade, on a style definition when the
    /// style ends.
    pub(crate) fn fold_into(self, target: &mut ParaProperties) {
        if self.start.is_none() && self.end.is_none() {
            return;
        }
        let rtl = target.direction == Some(TextDirection::Rtl);
        let borders = target.borders.get_or_insert_with(CellBorders::default);
        let (lead, trail) = if rtl {
            (&mut borders.right, &mut borders.left)
        } else {
            (&mut borders.left, &mut borders.right)
        };
        if let Some(s) = self.start {
            *lead = Some(s);
            target.border_spelling.start = true;
        }
        if let Some(s) = self.end {
            *trail = Some(s);
            target.border_spelling.end = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    fn start_tag(xml: &str) -> BytesStart<'static> {
        let mut r = Reader::from_str(xml);
        match r.read_event().unwrap() {
            Event::Empty(e) | Event::Start(e) => e.into_owned(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn nil_and_none_are_explicit_no_border_edges() {
        for v in ["nil", "none", "NONE"] {
            let s = parse_pbdr_stroke(&start_tag(&format!(r#"<w:top w:val="{v}"/>"#)))
                .expect("a set edge");
            assert_eq!(s.style, BorderStyle::None);
            assert_eq!((s.size_eighth_pt, s.color), (0, None));
        }
        assert!(parse_pbdr_stroke(&start_tag(r#"<w:top w:sz="4"/>"#)).is_none());
        let s = parse_pbdr_stroke(&start_tag(
            r#"<w:bottom w:val="single" w:sz="8" w:space="4" w:color="4F81BD"/>"#,
        ))
        .unwrap();
        assert_eq!(s.style, BorderStyle::Single);
        assert_eq!(
            (s.size_eighth_pt, s.color),
            (8, Some([0x4F, 0x81, 0xBD, 255]))
        );
    }

    #[test]
    fn logical_edges_fold_by_the_owners_own_direction() {
        let mut logical = PbdrLogical::default();
        assert!(logical.accept(b"w:start", &start_tag(r#"<w:start w:val="single"/>"#)));
        assert!(!logical.accept(b"w:left", &start_tag(r#"<w:left w:val="single"/>"#)));
        let mut ltr = ParaProperties::default();
        logical.clone().fold_into(&mut ltr);
        let b = ltr.borders.clone().unwrap();
        assert!(b.left.is_some() && b.right.is_none());
        assert!(ltr.border_spelling.start && !ltr.border_spelling.end);
        let mut rtl = ParaProperties {
            direction: Some(TextDirection::Rtl),
            ..Default::default()
        };
        logical.fold_into(&mut rtl);
        let b = rtl.borders.unwrap();
        assert!(b.right.is_some() && b.left.is_none());
    }
}
