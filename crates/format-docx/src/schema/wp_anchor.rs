//! Issue #69 — `CT_Anchor` (`<wp:anchor>`, ECMA-376 Part 1 §20.4.2.3):
//! the WordprocessingDrawing placement of a *floating* picture.
//!
//! Read side: the attribute set of the `<wp:anchor>` start tag, the
//! `relativeFrom` / `<wp:align>` keyword tables (§20.4.3.4 `ST_RelFromH`,
//! §20.4.3.5 `ST_RelFromV`, §20.4.3.1 `ST_AlignH`, §20.4.3.2 `ST_AlignV`)
//! and the wrap-element names. Write side: the whole `<w:r><w:drawing>
//! <wp:anchor>…</wp:anchor></w:drawing></w:r>` run in Word's own child
//! order (`simplePos`, `positionH`, `positionV`, `extent`, `effectExtent`,
//! wrap choice, `docPr`, `cNvGraphicFramePr`, `graphic`) — the schema is
//! sequence-strict and Word rejects a reordered pair with the "unreadable
//! content" recovery prompt, exactly like `CT_Inline`.
//!
//! The typed model is `engine::FloatAnchor`; the two children the writer
//! cannot regenerate from typed fields (`<wp:docPr>` with its id / name /
//! descr / hyperlink, and the wrap element with an optional
//! `<wp:wrapPolygon>`) ride the anchor verbatim (`doc_pr_xml`,
//! `wrap_xml`) so a regenerated paragraph stays byte-faithful.

use engine::{
    FloatAlign, FloatAnchor, FloatOffset, HRelativeFrom, VRelativeFrom, WrapKind, WrapText,
};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use super::ct_rpr::attr_val;

/// Which positioning axis is open while the reader walks an anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorAxis {
    H,
    V,
}

/// Which offset element is collecting text inside an open axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorOffsetKind {
    /// `<wp:posOffset>` — EMU integer.
    PosOffset,
    /// `<wp:align>` — alignment keyword.
    Align,
    /// `<wp14:pctPosHOffset>` / `<wp14:pctPosVOffset>` — thousandths of
    /// a percent of the frame extent.
    Percent,
}

/// `true` for the OOXML boolean spellings `1` / `true` (ST_OnOff).
fn on_off(v: Option<String>) -> bool {
    matches!(v.as_deref(), Some("1" | "true" | "on"))
}

/// Build the typed anchor from the `<wp:anchor …>` start tag. Every
/// attribute is required by the schema; a missing one falls back to the
/// stock Word value so a hand-authored or truncated tag still yields a
/// usable float rather than a dropped picture.
pub fn anchor_from_start_tag(e: &BytesStart) -> FloatAnchor {
    let num = |key: &[u8]| -> Option<i64> { attr_val(e, key).and_then(|v| v.parse().ok()) };
    let flag = |key: &[u8], default: bool| -> bool {
        match attr_val(e, key) {
            Some(v) => on_off(Some(v)),
            None => default,
        }
    };
    let stock = FloatAnchor::default();
    FloatAnchor {
        dist_top_emu: num(b"distT").unwrap_or(0),
        dist_bottom_emu: num(b"distB").unwrap_or(0),
        dist_left_emu: num(b"distL").unwrap_or(0),
        dist_right_emu: num(b"distR").unwrap_or(0),
        simple_pos: flag(b"simplePos", false),
        relative_height: attr_val(e, b"relativeHeight")
            .and_then(|v| v.parse().ok())
            .unwrap_or(stock.relative_height),
        behind_doc: flag(b"behindDoc", false),
        locked: flag(b"locked", false),
        layout_in_cell: flag(b"layoutInCell", true),
        allow_overlap: flag(b"allowOverlap", true),
        hidden: flag(b"hidden", false),
        ..stock
    }
}

/// `relativeFrom` of `<wp:positionH>` (`ST_RelFromH`). Unknown values
/// fall back to Word's default (`column`).
pub fn h_relative_from(v: Option<&str>) -> HRelativeFrom {
    match v {
        Some("character") => HRelativeFrom::Character,
        Some("insideMargin") => HRelativeFrom::InsideMargin,
        Some("leftMargin") => HRelativeFrom::LeftMargin,
        Some("margin") => HRelativeFrom::Margin,
        Some("outsideMargin") => HRelativeFrom::OutsideMargin,
        Some("page") => HRelativeFrom::Page,
        Some("rightMargin") => HRelativeFrom::RightMargin,
        _ => HRelativeFrom::Column,
    }
}

/// `relativeFrom` of `<wp:positionV>` (`ST_RelFromV`). Unknown values
/// fall back to Word's default (`paragraph`).
pub fn v_relative_from(v: Option<&str>) -> VRelativeFrom {
    match v {
        Some("bottomMargin") => VRelativeFrom::BottomMargin,
        Some("insideMargin") => VRelativeFrom::InsideMargin,
        Some("line") => VRelativeFrom::Line,
        Some("margin") => VRelativeFrom::Margin,
        Some("outsideMargin") => VRelativeFrom::OutsideMargin,
        Some("page") => VRelativeFrom::Page,
        Some("topMargin") => VRelativeFrom::TopMargin,
        _ => VRelativeFrom::Paragraph,
    }
}

pub fn h_relative_from_str(v: HRelativeFrom) -> &'static str {
    match v {
        HRelativeFrom::Character => "character",
        HRelativeFrom::Column => "column",
        HRelativeFrom::InsideMargin => "insideMargin",
        HRelativeFrom::LeftMargin => "leftMargin",
        HRelativeFrom::Margin => "margin",
        HRelativeFrom::OutsideMargin => "outsideMargin",
        HRelativeFrom::Page => "page",
        HRelativeFrom::RightMargin => "rightMargin",
    }
}

pub fn v_relative_from_str(v: VRelativeFrom) -> &'static str {
    match v {
        VRelativeFrom::BottomMargin => "bottomMargin",
        VRelativeFrom::InsideMargin => "insideMargin",
        VRelativeFrom::Line => "line",
        VRelativeFrom::Margin => "margin",
        VRelativeFrom::OutsideMargin => "outsideMargin",
        VRelativeFrom::Page => "page",
        VRelativeFrom::Paragraph => "paragraph",
        VRelativeFrom::TopMargin => "topMargin",
    }
}

/// `<wp:align>` keyword (`ST_AlignH` ∪ `ST_AlignV`).
pub fn align_from_str(v: &str) -> Option<FloatAlign> {
    Some(match v {
        "left" => FloatAlign::Left,
        "right" => FloatAlign::Right,
        "center" => FloatAlign::Center,
        "inside" => FloatAlign::Inside,
        "outside" => FloatAlign::Outside,
        "top" => FloatAlign::Top,
        "bottom" => FloatAlign::Bottom,
        _ => return None,
    })
}

pub fn align_str(a: FloatAlign) -> &'static str {
    match a {
        FloatAlign::Left => "left",
        FloatAlign::Right => "right",
        FloatAlign::Center => "center",
        FloatAlign::Inside => "inside",
        FloatAlign::Outside => "outside",
        FloatAlign::Top => "top",
        FloatAlign::Bottom => "bottom",
    }
}

/// Lower the text content of an offset element into a `FloatOffset`.
/// `None` for unparsable content — the axis keeps its previous offset
/// (the stock `Emu(0)`), so a garbled element degrades to "at the
/// frame origin" rather than dropping the picture.
pub fn parse_offset(kind: AnchorOffsetKind, text: &str) -> Option<FloatOffset> {
    let text = text.trim();
    match kind {
        AnchorOffsetKind::PosOffset => text.parse::<i64>().ok().map(FloatOffset::Emu),
        AnchorOffsetKind::Align => align_from_str(text).map(FloatOffset::Align),
        AnchorOffsetKind::Percent => text.parse::<i32>().ok().map(FloatOffset::PercentMilli),
    }
}

/// The wrap kind an anchor's wrap child declares, by element name.
/// `None` for anything that is not one of the five wrap elements.
pub fn wrap_kind_of(qname: &[u8]) -> Option<WrapKind> {
    Some(match qname {
        b"wp:wrapNone" => WrapKind::None,
        b"wp:wrapSquare" => WrapKind::Square,
        b"wp:wrapTight" => WrapKind::Tight,
        b"wp:wrapThrough" => WrapKind::Through,
        b"wp:wrapTopAndBottom" => WrapKind::TopAndBottom,
        _ => return None,
    })
}

/// Is `qname` one of the five wrap elements?
pub fn is_wrap_element(qname: &[u8]) -> bool {
    wrap_kind_of(qname).is_some()
}

/// `wrapText` keyword (`ST_WrapText`, §20.4.3.7). Unknown → `bothSides`.
pub fn wrap_text_from_str(v: Option<&str>) -> WrapText {
    match v {
        Some("left") => WrapText::Left,
        Some("right") => WrapText::Right,
        Some("largest") => WrapText::Largest,
        _ => WrapText::BothSides,
    }
}

pub fn wrap_text_str(v: WrapText) -> &'static str {
    match v {
        WrapText::BothSides => "bothSides",
        WrapText::Left => "left",
        WrapText::Right => "right",
        WrapText::Largest => "largest",
    }
}

/// The typed content of a wrap element: kind, side rule, polygon.
pub type WrapFragment = (WrapKind, WrapText, Option<Vec<(i64, i64)>>);

/// Issue #82 — the typed content of one captured wrap element: its kind
/// (by root element name), `wrapText`, and the `<wp:wrapPolygon>` vertices
/// (`<wp:start>` then every `<wp:lineTo>`, in document order). `None` for
/// a fragment whose root is not a wrap element. Tolerant: an unparsable
/// coordinate drops that vertex, never the whole element.
pub fn parse_wrap_fragment(xml: &str) -> Option<WrapFragment> {
    let mut reader = Reader::from_str(xml);
    let mut kind: Option<WrapKind> = None;
    let mut text = WrapText::BothSides;
    let mut poly: Vec<(i64, i64)> = Vec::new();
    let mut saw_poly = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = e.name();
                let q = name.as_ref();
                if kind.is_none() {
                    kind = Some(wrap_kind_of(q)?);
                    text = wrap_text_from_str(attr_val(&e, b"wrapText").as_deref());
                    continue;
                }
                match q {
                    b"wp:wrapPolygon" => saw_poly = true,
                    b"wp:start" | b"wp:lineTo" => {
                        let c = |k: &[u8]| attr_val(&e, k).and_then(|v| v.trim().parse().ok());
                        if let (Some(x), Some(y)) = (c(b"x"), c(b"y")) {
                            poly.push((x, y));
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    let polygon = (saw_poly && !poly.is_empty()).then_some(poly);
    kind.map(|k| (k, text, polygon))
}

/// Issue #82 — fold the verbatim wrap element the reader captured into
/// the anchor's typed wrap fields (`wrap_text`, `wrap_polygon`).
pub fn apply_wrap_fragment(anchor: &mut FloatAnchor) {
    if let Some((_, text, polygon)) = anchor.wrap_xml.as_deref().and_then(parse_wrap_fragment) {
        anchor.wrap_text = text;
        anchor.wrap_polygon = polygon;
    }
}

/// Word's "not yet edited" wrap polygon: the full object rectangle in the
/// 21600-unit shape space.
const DEFAULT_POLYGON: [(i64, i64); 5] = [(0, 0), (0, 21600), (21600, 21600), (21600, 0), (0, 0)];

/// The wrap element the writer synthesizes from the typed fields (an
/// engine-authored anchor, or one whose wrap was edited). Tight / through
/// need a polygon by schema — the anchor's own, else Word's full-rectangle
/// default.
fn synthesized_wrap_xml(anchor: &FloatAnchor) -> String {
    let tag = match anchor.wrap {
        WrapKind::None => return "<wp:wrapNone/>".into(),
        WrapKind::TopAndBottom => return "<wp:wrapTopAndBottom/>".into(),
        WrapKind::Square => {
            return format!(
                "<wp:wrapSquare wrapText=\"{}\"/>",
                wrap_text_str(anchor.wrap_text)
            );
        }
        WrapKind::Tight => "wp:wrapTight",
        WrapKind::Through => "wp:wrapThrough",
    };
    let (edited, pts): (&str, &[(i64, i64)]) = match anchor.wrap_polygon.as_deref() {
        Some(p) if !p.is_empty() => ("1", p),
        _ => ("0", &DEFAULT_POLYGON),
    };
    let mut out = format!(
        "<{tag} wrapText=\"{}\"><wp:wrapPolygon edited=\"{edited}\">",
        wrap_text_str(anchor.wrap_text)
    );
    for (i, (x, y)) in pts.iter().enumerate() {
        let el = if i == 0 { "wp:start" } else { "wp:lineTo" };
        out.push_str(&format!("<{el} x=\"{x}\" y=\"{y}\"/>"));
    }
    out.push_str("</wp:wrapPolygon></");
    out.push_str(tag);
    out.push('>');
    out
}

/// Issue #82 — may the verbatim wrap element be re-emitted? Only while it
/// still says what the typed fields say; otherwise the model was edited
/// and the element is regenerated. The typed defaults (`wrap_text ==
/// BothSides`, `wrap_polygon == None`) are also what an anchor built
/// before #82 modeled them (a pre-#82 snapshot, an engine-built anchor
/// carrying a verbatim element) — they defer to the verbatim element
/// instead of overwriting a real side rule / polygon with nothing.
fn verbatim_wrap_is_current(anchor: &FloatAnchor, verbatim: &str) -> bool {
    parse_wrap_fragment(verbatim).is_some_and(|(kind, text, polygon)| {
        kind == anchor.wrap
            && (anchor.wrap_text == WrapText::BothSides || text == anchor.wrap_text)
            && (anchor.wrap_polygon.is_none() || polygon == anchor.wrap_polygon)
    })
}

fn on_off_str(b: bool) -> &'static str {
    if b { "1" } else { "0" }
}

/// One positioning axis: `<wp:positionH relativeFrom="…">` + its offset
/// child. `pct_element` is the wp14 percentage element for the axis.
fn emit_axis(
    tag: &str,
    relative_from: &str,
    offset: FloatOffset,
    pct_element: &str,
    out: &mut String,
) {
    out.push('<');
    out.push_str(tag);
    out.push_str(" relativeFrom=\"");
    out.push_str(relative_from);
    out.push_str("\">");
    match offset {
        FloatOffset::Emu(emu) => {
            out.push_str("<wp:posOffset>");
            out.push_str(&emu.to_string());
            out.push_str("</wp:posOffset>");
        }
        FloatOffset::Align(a) => {
            out.push_str("<wp:align>");
            out.push_str(align_str(a));
            out.push_str("</wp:align>");
        }
        FloatOffset::PercentMilli(p) => {
            out.push('<');
            out.push_str(pct_element);
            out.push('>');
            out.push_str(&p.to_string());
            out.push_str("</");
            out.push_str(pct_element);
            out.push('>');
        }
    }
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
}

/// Emit the `<wp:anchor>` element (attributes + every child up to and
/// excluding `<wp:cNvGraphicFramePr>` / `<a:graphic>`, which the caller
/// shares with the inline emitter). Attribute order follows the schema's
/// declaration order, which is also the order Word writes.
pub fn emit_anchor_open(anchor: &FloatAnchor, cx: i64, cy: i64, out: &mut String) {
    out.push_str("<wp:anchor distT=\"");
    out.push_str(&anchor.dist_top_emu.to_string());
    out.push_str("\" distB=\"");
    out.push_str(&anchor.dist_bottom_emu.to_string());
    out.push_str("\" distL=\"");
    out.push_str(&anchor.dist_left_emu.to_string());
    out.push_str("\" distR=\"");
    out.push_str(&anchor.dist_right_emu.to_string());
    out.push_str("\" simplePos=\"");
    out.push_str(on_off_str(anchor.simple_pos));
    out.push_str("\" relativeHeight=\"");
    out.push_str(&anchor.relative_height.to_string());
    out.push_str("\" behindDoc=\"");
    out.push_str(on_off_str(anchor.behind_doc));
    out.push_str("\" locked=\"");
    out.push_str(on_off_str(anchor.locked));
    out.push_str("\" layoutInCell=\"");
    out.push_str(on_off_str(anchor.layout_in_cell));
    if anchor.hidden {
        out.push_str("\" hidden=\"1");
    }
    out.push_str("\" allowOverlap=\"");
    out.push_str(on_off_str(anchor.allow_overlap));
    out.push_str("\">");
    /* `<wp:simplePos>` is a required child even when `simplePos="0"`. */
    out.push_str("<wp:simplePos x=\"");
    out.push_str(&anchor.simple_pos_x_emu.to_string());
    out.push_str("\" y=\"");
    out.push_str(&anchor.simple_pos_y_emu.to_string());
    out.push_str("\"/>");
    emit_axis(
        "wp:positionH",
        h_relative_from_str(anchor.position_h.relative_from),
        anchor.position_h.offset,
        "wp14:pctPosHOffset",
        out,
    );
    emit_axis(
        "wp:positionV",
        v_relative_from_str(anchor.position_v.relative_from),
        anchor.position_v.offset,
        "wp14:pctPosVOffset",
        out,
    );
    out.push_str("<wp:extent cx=\"");
    out.push_str(&cx.to_string());
    out.push_str("\" cy=\"");
    out.push_str(&cy.to_string());
    out.push_str("\"/><wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/>");
    match anchor.wrap_xml.as_deref() {
        Some(verbatim) if verbatim_wrap_is_current(anchor, verbatim) => out.push_str(verbatim),
        _ => out.push_str(&synthesized_wrap_xml(anchor)),
    }
    match anchor.doc_pr_xml.as_deref() {
        Some(verbatim) => out.push_str(verbatim),
        None => out.push_str("<wp:docPr id=\"1\" name=\"Picture\"/>"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_tables_round_trip() {
        for v in [
            HRelativeFrom::Character,
            HRelativeFrom::Column,
            HRelativeFrom::InsideMargin,
            HRelativeFrom::LeftMargin,
            HRelativeFrom::Margin,
            HRelativeFrom::OutsideMargin,
            HRelativeFrom::Page,
            HRelativeFrom::RightMargin,
        ] {
            assert_eq!(h_relative_from(Some(h_relative_from_str(v))), v);
        }
        for v in [
            VRelativeFrom::BottomMargin,
            VRelativeFrom::InsideMargin,
            VRelativeFrom::Line,
            VRelativeFrom::Margin,
            VRelativeFrom::OutsideMargin,
            VRelativeFrom::Page,
            VRelativeFrom::Paragraph,
            VRelativeFrom::TopMargin,
        ] {
            assert_eq!(v_relative_from(Some(v_relative_from_str(v))), v);
        }
        for a in [
            FloatAlign::Left,
            FloatAlign::Right,
            FloatAlign::Center,
            FloatAlign::Inside,
            FloatAlign::Outside,
            FloatAlign::Top,
            FloatAlign::Bottom,
        ] {
            assert_eq!(align_from_str(align_str(a)), Some(a));
        }
        /* Unknown keywords fall back to Word's defaults, never panic. */
        assert_eq!(h_relative_from(Some("bogus")), HRelativeFrom::Column);
        assert_eq!(v_relative_from(None), VRelativeFrom::Paragraph);
        assert_eq!(align_from_str("diagonal"), None);
    }

    #[test]
    fn offsets_parse_by_kind() {
        assert_eq!(
            parse_offset(AnchorOffsetKind::PosOffset, " -914400 "),
            Some(FloatOffset::Emu(-914_400))
        );
        assert_eq!(
            parse_offset(AnchorOffsetKind::Align, "center"),
            Some(FloatOffset::Align(FloatAlign::Center))
        );
        assert_eq!(
            parse_offset(AnchorOffsetKind::Percent, "50000"),
            Some(FloatOffset::PercentMilli(50_000))
        );
        assert_eq!(parse_offset(AnchorOffsetKind::PosOffset, "x"), None);
    }

    #[test]
    fn anchor_open_follows_schema_child_order() {
        let anchor = FloatAnchor {
            behind_doc: true,
            wrap: WrapKind::Square,
            ..FloatAnchor::default()
        };
        let mut out = String::new();
        emit_anchor_open(&anchor, 10, 20, &mut out);
        let order = [
            "<wp:anchor ",
            "<wp:simplePos ",
            "<wp:positionH ",
            "<wp:positionV ",
            "<wp:extent ",
            "<wp:effectExtent ",
            "<wp:wrapSquare ",
            "<wp:docPr ",
        ];
        let mut last = 0;
        for needle in order {
            let at = out[last..]
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} missing or out of order in {out}"));
            last += at;
        }
        assert!(out.contains("behindDoc=\"1\""));
        assert!(out.contains("<wp:posOffset>0</wp:posOffset>"));
        assert!(!out.contains("hidden="), "hidden is omitted when false");
    }
    /// Issue #82 — the verbatim wrap element is re-emitted only while it
    /// still matches the typed fields; an edited mode / side / polygon is
    /// regenerated from the model.
    #[test]
    fn wrap_element_is_verbatim_until_edited_then_regenerated() {
        let verbatim = concat!(
            r#"<wp:wrapTight wrapText="left"><wp:wrapPolygon edited="1">"#,
            r#"<wp:start x="0" y="0"/><wp:lineTo x="21600" y="0"/>"#,
            r#"<wp:lineTo x="10800" y="21600"/></wp:wrapPolygon></wp:wrapTight>"#
        );
        let mut anchor = FloatAnchor {
            wrap: WrapKind::Tight,
            wrap_xml: Some(verbatim.into()),
            ..FloatAnchor::default()
        };
        apply_wrap_fragment(&mut anchor);
        assert_eq!(anchor.wrap_text, WrapText::Left);
        assert_eq!(anchor.wrap_polygon.as_ref().map(Vec::len), Some(3));
        let mut out = String::new();
        emit_anchor_open(&anchor, 1, 1, &mut out);
        assert!(out.contains(verbatim), "unedited ⇒ byte-faithful");

        /* Side rule edited → regenerated, polygon kept. */
        anchor.wrap_text = WrapText::Right;
        let mut out = String::new();
        emit_anchor_open(&anchor, 1, 1, &mut out);
        assert!(!out.contains(verbatim));
        assert!(out.contains(r#"<wp:wrapTight wrapText="right"><wp:wrapPolygon edited="1"><wp:start x="0" y="0"/><wp:lineTo x="21600" y="0"/><wp:lineTo x="10800" y="21600"/></wp:wrapPolygon></wp:wrapTight>"#), "{out}");
        /* Mode edited → regenerated. */
        anchor.wrap = WrapKind::Square;
        let mut out = String::new();
        emit_anchor_open(&anchor, 1, 1, &mut out);
        assert!(
            out.contains(r#"<wp:wrapSquare wrapText="right"/>"#),
            "{out}"
        );

        /* Every mode regenerates to a parseable element with its fields. */
        for kind in [
            WrapKind::None,
            WrapKind::Square,
            WrapKind::Tight,
            WrapKind::Through,
            WrapKind::TopAndBottom,
        ] {
            let a = FloatAnchor {
                wrap: kind,
                wrap_text: WrapText::Right,
                ..FloatAnchor::default()
            };
            let xml = synthesized_wrap_xml(&a);
            let (k, t, poly) = parse_wrap_fragment(&xml).expect("parses");
            assert_eq!(k, kind);
            if matches!(kind, WrapKind::Square | WrapKind::Tight | WrapKind::Through) {
                assert_eq!(t, WrapText::Right);
            }
            assert_eq!(
                poly.is_some(),
                matches!(kind, WrapKind::Tight | WrapKind::Through),
                "{xml}"
            );
        }
        assert!(parse_wrap_fragment("<wp:docPr id=\"1\"/>").is_none());
    }
}
