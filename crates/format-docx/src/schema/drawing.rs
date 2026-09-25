//! Issue #119 — run-level drawing objects, scanned from their verbatim
//! source element.
//!
//! A run can carry one of four object elements: `<w:drawing>` (DrawingML:
//! a picture, a shape, a text box, a chart, SmartArt), `<mc:AlternateContent>`
//! (a DrawingML choice plus its VML fallback), `<w:pict>` (VML: a legacy
//! picture, a `<v:textbox>`, Word's horizontal rule) or `<w:object>` (an
//! embedded OLE object with a VML preview). The reader used to walk the
//! DrawingML subtree inline in `parts::document`'s state machine and keep
//! only what the typed model represents — a picture's blip + extent (+
//! anchor) — so a text box, shape or VML object left NOTHING behind and an
//! edited paragraph regenerated without it (7 drawings lost on
//! `shapes-with-text.docx`).
//!
//! Now the reader captures the whole element ([`capture_subtree`]) and
//! [`scan_drawing`] lifts the modeled facts out of the bytes; the bytes
//! ride `InlineObject::source_xml`. The `.docx` writer runs the SAME scan
//! on those bytes at save time and re-emits them verbatim while the model
//! still agrees with them (a *verified* passthrough — the object was not
//! resized, moved or re-wrapped); otherwise it regenerates a picture from
//! the typed fields as before. An object with no picture (`rel_id` empty)
//! has no regeneration path and is always written from its bytes, so the
//! text box / shape / OLE object survives every edit around it. This is
//! the ground issue #83 (text-box stories) builds on: the emitter is
//! general — whatever the model does not fully represent stays in the
//! preserved subtree.

use engine::FloatAnchor;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use super::ct_rpr::attr_val;
use super::grab_bag::slice_fragment;
use super::wp_anchor::{
    AnchorAxis, AnchorOffsetKind, anchor_from_start_tag, apply_wrap_fragment, h_relative_from,
    is_wrap_element, parse_offset, v_relative_from, wrap_kind_of,
};

/// EMU per point (ECMA-376 §20.1.2.1: 914 400 EMU per inch, 72 pt per
/// inch).
const EMU_PER_PT: f64 = 12_700.0;

/// The modeled facts of one run-level object element.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrawingScan {
    /// `<a:blip r:embed>` of the drawing's OWN picture (a text box's
    /// inner story is skipped), or the VML `<v:imagedata r:id>`. `None`
    /// for a shape / text box / chart / OLE object without a preview —
    /// the object then has no typed regeneration.
    pub rel_id: Option<String>,
    /// `<wp:extent cx>` in EMU, or the VML `style` width. `None` when the
    /// element declares no size.
    pub cx: Option<i64>,
    /// `<wp:extent cy>` in EMU, or the VML `style` height.
    pub cy: Option<i64>,
    /// `<wp:anchor>` placement (typed, wrap + docPr verbatim inside);
    /// `None` for `<wp:inline>` and for VML.
    pub anchor: Option<Box<FloatAnchor>>,
    /// `true` when the fragment holds a DrawingML `<w:drawing>` (possibly
    /// inside an `<mc:Choice>`); `false` for VML-only markup.
    pub drawing_ml: bool,
}

impl DrawingScan {
    /// `(rel_id, cx, cy)` as the typed `InlineKind::Image` carries them:
    /// an object without a picture gets an empty `rel_id`, an object
    /// without a declared size a 0×0 extent (layout reserves nothing).
    pub fn image_fields(&self) -> (String, i64, i64) {
        (
            self.rel_id.clone().unwrap_or_default(),
            self.cx.unwrap_or(0),
            self.cy.unwrap_or(0),
        )
    }
}

/// Scan one run-level object element (`<w:drawing>`, `<mc:AlternateContent>`,
/// `<w:pict>`, `<w:object>`) for the facts the typed model carries. Never
/// fails: an unparseable fragment scans as "no picture, no size" and is
/// preserved opaquely.
pub fn scan_drawing(fragment: &[u8]) -> DrawingScan {
    let mut out = DrawingScan::default();
    let mut reader = Reader::from_reader(fragment);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();

    let mut prev_pos: usize = 0;
    /* The first `<w:drawing>` is THE object (the `mc:Choice` in an
    AlternateContent); everything after its end tag — the `mc:Fallback`
    duplicate — is ignored. */
    let mut in_drawing = false;
    let mut drawing_done = false;
    let mut in_anchor = false;
    let mut axis: Option<AnchorAxis> = None;
    let mut offset: Option<(AnchorOffsetKind, String)> = None;
    /* VML sizing applies only when no DrawingML object was found. */
    let mut vml_seen = false;
    let mut vml_absolute = false;

    while let Ok(ev) = reader.read_event_into(&mut buf) {
        let pos = reader.buffer_position() as usize;
        match ev {
            Event::Start(e) => {
                let name = e.name();
                let n = name.as_ref();
                match n {
                    /* A text box's story and the VML fallback are not
                    facts about THIS object — skip them whole so an inner
                    picture cannot masquerade as the object's blip. */
                    b"w:txbxContent" | b"mc:Fallback" => {
                        let end_tag = e.to_end().into_owned();
                        let mut skip = Vec::new();
                        if reader.read_to_end_into(end_tag.name(), &mut skip).is_err() {
                            break;
                        }
                    }
                    b"w:drawing" if !in_drawing && !drawing_done => {
                        in_drawing = true;
                        out.drawing_ml = true;
                    }
                    b"wp:anchor" if in_drawing => {
                        in_anchor = true;
                        out.anchor = Some(Box::new(anchor_from_start_tag(&e)));
                    }
                    b"wp:positionH" if in_anchor => {
                        axis = Some(AnchorAxis::H);
                        if let Some(a) = out.anchor.as_mut() {
                            a.position_h.relative_from =
                                h_relative_from(attr_val(&e, b"relativeFrom").as_deref());
                        }
                    }
                    b"wp:positionV" if in_anchor => {
                        axis = Some(AnchorAxis::V);
                        if let Some(a) = out.anchor.as_mut() {
                            a.position_v.relative_from =
                                v_relative_from(attr_val(&e, b"relativeFrom").as_deref());
                        }
                    }
                    b"wp:posOffset" if axis.is_some() => {
                        offset = Some((AnchorOffsetKind::PosOffset, String::new()));
                    }
                    b"wp:align" if axis.is_some() => {
                        offset = Some((AnchorOffsetKind::Align, String::new()));
                    }
                    b"wp14:pctPosHOffset" | b"wp14:pctPosVOffset" if axis.is_some() => {
                        offset = Some((AnchorOffsetKind::Percent, String::new()));
                    }
                    n if in_anchor && is_wrap_element(n) => {
                        let end_tag = e.to_end().into_owned();
                        let mut skip = Vec::new();
                        if reader.read_to_end_into(end_tag.name(), &mut skip).is_err() {
                            break;
                        }
                        let end = reader.buffer_position() as usize;
                        if let Some(a) = out.anchor.as_mut() {
                            a.wrap = wrap_kind_of(n).unwrap_or_default();
                            a.wrap_xml = slice_fragment(fragment, prev_pos, end)
                                .and_then(|f| String::from_utf8(f).ok());
                            apply_wrap_fragment(a);
                        }
                    }
                    b"wp:docPr" if in_anchor => {
                        let end_tag = e.to_end().into_owned();
                        let mut skip = Vec::new();
                        if reader.read_to_end_into(end_tag.name(), &mut skip).is_err() {
                            break;
                        }
                        let end = reader.buffer_position() as usize;
                        if let Some(a) = out.anchor.as_mut() {
                            a.doc_pr_xml = slice_fragment(fragment, prev_pos, end)
                                .and_then(|f| String::from_utf8(f).ok());
                        }
                    }
                    b"wp:extent" if in_drawing => apply_extent(&e, &mut out),
                    b"a:blip" if in_drawing && out.rel_id.is_none() => {
                        out.rel_id = attr_val(&e, b"r:embed");
                    }
                    n if !in_drawing && !drawing_done && is_vml_shape(n) && !vml_seen => {
                        vml_seen = true;
                        apply_vml_style(&e, &mut out, &mut vml_absolute);
                    }
                    b"v:imagedata" if !in_drawing && !drawing_done && out.rel_id.is_none() => {
                        out.rel_id = attr_val(&e, b"r:id");
                    }
                    _ => {}
                }
            }
            Event::Empty(e) => {
                let name = e.name();
                let n = name.as_ref();
                match n {
                    b"wp:extent" if in_drawing => apply_extent(&e, &mut out),
                    b"a:blip" if in_drawing && out.rel_id.is_none() => {
                        out.rel_id = attr_val(&e, b"r:embed");
                    }
                    b"wp:simplePos" if in_anchor => {
                        if let Some(a) = out.anchor.as_mut() {
                            a.simple_pos_x_emu =
                                attr_val(&e, b"x").and_then(|v| v.parse().ok()).unwrap_or(0);
                            a.simple_pos_y_emu =
                                attr_val(&e, b"y").and_then(|v| v.parse().ok()).unwrap_or(0);
                        }
                    }
                    n if in_anchor && is_wrap_element(n) => {
                        if let Some(a) = out.anchor.as_mut() {
                            a.wrap = wrap_kind_of(n).unwrap_or_default();
                            a.wrap_xml = slice_fragment(fragment, prev_pos, pos)
                                .and_then(|f| String::from_utf8(f).ok());
                            apply_wrap_fragment(a);
                        }
                    }
                    b"wp:docPr" if in_anchor => {
                        if let Some(a) = out.anchor.as_mut() {
                            a.doc_pr_xml = slice_fragment(fragment, prev_pos, pos)
                                .and_then(|f| String::from_utf8(f).ok());
                        }
                    }
                    n if !in_drawing && !drawing_done && is_vml_shape(n) && !vml_seen => {
                        vml_seen = true;
                        apply_vml_style(&e, &mut out, &mut vml_absolute);
                    }
                    b"v:imagedata" if !in_drawing && !drawing_done && out.rel_id.is_none() => {
                        out.rel_id = attr_val(&e, b"r:id");
                    }
                    _ => {}
                }
            }
            Event::Text(t) => {
                if let Some((_, text)) = offset.as_mut()
                    && let Ok(s) = t.unescape()
                {
                    text.push_str(&s);
                }
            }
            Event::End(e) => {
                let name = e.name();
                match name.as_ref() {
                    b"wp:posOffset"
                    | b"wp:align"
                    | b"wp14:pctPosHOffset"
                    | b"wp14:pctPosVOffset" => {
                        if let (Some((kind, text)), Some(ax), Some(a)) =
                            (offset.take(), axis, out.anchor.as_mut())
                            && let Some(o) = parse_offset(kind, &text)
                        {
                            match ax {
                                AnchorAxis::H => a.position_h.offset = o,
                                AnchorAxis::V => a.position_v.offset = o,
                            }
                        }
                    }
                    b"wp:positionH" | b"wp:positionV" => axis = None,
                    b"wp:anchor" => in_anchor = false,
                    b"w:drawing" if in_drawing => {
                        in_drawing = false;
                        drawing_done = true;
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
        prev_pos = reader.buffer_position() as usize;
        buf.clear();
    }
    if !out.drawing_ml && vml_absolute {
        /* An absolutely positioned VML shape floats; the inline model has
        no placement for it, so it reserves no space rather than pushing
        the line apart. Its bytes still ride the object. */
        out.cx = Some(0);
        out.cy = Some(0);
    }
    out
}

fn apply_extent(e: &BytesStart, out: &mut DrawingScan) {
    if out.cx.is_none() && out.cy.is_none() {
        out.cx = attr_val(e, b"cx").and_then(|v| v.trim().parse().ok());
        out.cy = attr_val(e, b"cy").and_then(|v| v.trim().parse().ok());
    }
}

/// The VML shape elements whose `style` carries the object's size.
fn is_vml_shape(qname: &[u8]) -> bool {
    matches!(
        qname,
        b"v:shape" | b"v:rect" | b"v:oval" | b"v:roundrect" | b"v:line" | b"v:group" | b"v:image"
    )
}

/// Lift `width:` / `height:` (and `position:absolute`) out of a VML
/// `style` attribute (`"width:468pt;height:1.5pt"`). Units: pt, in, cm,
/// mm, px, emu; a bare number is points.
fn apply_vml_style(e: &BytesStart, out: &mut DrawingScan, absolute: &mut bool) {
    let Some(style) = attr_val(e, b"style") else {
        return;
    };
    for decl in style.split(';') {
        let Some((key, value)) = decl.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        match key.as_str() {
            "width" => out.cx = vml_length_emu(value),
            "height" => out.cy = vml_length_emu(value),
            "position" if value.eq_ignore_ascii_case("absolute") => *absolute = true,
            _ => {}
        }
    }
}

/// A CSS-style VML length to EMU.
pub fn vml_length_emu(value: &str) -> Option<i64> {
    let value = value.trim();
    let split = value
        .find(|c: char| c.is_ascii_alphabetic() || c == '%')
        .unwrap_or(value.len());
    let (num, unit) = value.split_at(split);
    let num: f64 = num.trim().parse().ok()?;
    let per_unit = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "pt" => EMU_PER_PT,
        "in" => 914_400.0,
        "cm" => 360_000.0,
        "mm" => 36_000.0,
        "px" => 9_525.0,
        "emu" => 1.0,
        _ => return None,
    };
    Some((num * per_unit).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{FloatAlign, FloatOffset, HRelativeFrom, VRelativeFrom, WrapKind, WrapText};

    const PIC: &str = concat!(
        r#"<wp:cNvGraphicFramePr/><a:graphic>"#,
        r#"<a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture">"#,
        r#"<pic:pic><pic:blipFill><a:blip r:embed="rId5"/></pic:blipFill></pic:pic>"#,
        r#"</a:graphicData></a:graphic>"#,
    );

    #[test]
    fn inline_picture_scans_blip_and_extent() {
        let xml = format!(
            r#"<w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="914400" cy="457200"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="1" name="Picture 1"/>{PIC}</wp:inline></w:drawing>"#
        );
        let s = scan_drawing(xml.as_bytes());
        assert_eq!(s.rel_id.as_deref(), Some("rId5"));
        assert_eq!((s.cx, s.cy), (Some(914_400), Some(457_200)));
        assert!(s.anchor.is_none());
        assert!(s.drawing_ml);
        assert_eq!(s.image_fields(), ("rId5".into(), 914_400, 457_200));
    }

    #[test]
    fn anchored_picture_scans_the_typed_anchor_with_verbatim_wrap_and_docpr() {
        let wrap = r#"<wp:wrapTight wrapText="left"><wp:wrapPolygon edited="1"><wp:start x="0" y="0"/><wp:lineTo x="21600" y="0"/><wp:lineTo x="10800" y="21600"/></wp:wrapPolygon></wp:wrapTight>"#;
        let doc_pr = r#"<wp:docPr id="7" name="Picture 7" descr="a float"><a:hlinkClick r:id="rId9"/></wp:docPr>"#;
        let xml = format!(
            concat!(
                r#"<w:drawing><wp:anchor distT="10" distB="20" distL="30" distR="40" simplePos="1" relativeHeight="3" behindDoc="1" locked="0" layoutInCell="1" allowOverlap="0">"#,
                r#"<wp:simplePos x="100" y="200"/>"#,
                r#"<wp:positionH relativeFrom="margin"><wp:align>center</wp:align></wp:positionH>"#,
                r#"<wp:positionV relativeFrom="page"><wp:posOffset>1828800</wp:posOffset></wp:positionV>"#,
                r#"<wp:extent cx="914400" cy="457200"/><wp:effectExtent l="0" t="0" r="0" b="0"/>"#,
                "{wrap}{doc_pr}{pic}</wp:anchor></w:drawing>"
            ),
            wrap = wrap,
            doc_pr = doc_pr,
            pic = PIC
        );
        let s = scan_drawing(xml.as_bytes());
        assert_eq!(s.rel_id.as_deref(), Some("rId5"));
        let a = s.anchor.as_deref().expect("anchor");
        assert!(a.behind_doc && a.simple_pos && !a.allow_overlap);
        assert_eq!((a.simple_pos_x_emu, a.simple_pos_y_emu), (100, 200));
        assert_eq!(a.relative_height, 3);
        assert_eq!(
            (
                a.dist_top_emu,
                a.dist_bottom_emu,
                a.dist_left_emu,
                a.dist_right_emu
            ),
            (10, 20, 30, 40)
        );
        assert_eq!(a.position_h.relative_from, HRelativeFrom::Margin);
        assert_eq!(a.position_h.offset, FloatOffset::Align(FloatAlign::Center));
        assert_eq!(a.position_v.relative_from, VRelativeFrom::Page);
        assert_eq!(a.position_v.offset, FloatOffset::Emu(1_828_800));
        assert_eq!(a.wrap, WrapKind::Tight);
        assert_eq!(a.wrap_text, WrapText::Left);
        assert_eq!(a.wrap_polygon.as_ref().map(Vec::len), Some(3));
        assert_eq!(a.wrap_xml.as_deref(), Some(wrap));
        assert_eq!(a.doc_pr_xml.as_deref(), Some(doc_pr));
    }

    /// A text box: the inner story's picture is NOT the object's blip, the
    /// extent is the box's. An `mc:AlternateContent` scans its choice and
    /// ignores the VML fallback.
    #[test]
    fn text_box_and_alternate_content_scan_the_choice_without_the_inner_story() {
        let xml = concat!(
            r#"<mc:AlternateContent><mc:Choice Requires="wps"><w:drawing>"#,
            r#"<wp:inline><wp:extent cx="100" cy="200"/><wp:docPr id="1" name="Text Box 1"/>"#,
            r#"<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">"#,
            r#"<wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:drawing><wp:inline><wp:extent cx="999" cy="999"/>"#,
            r#"<a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed="rIdInner"/></pic:blipFill></pic:pic></a:graphicData></a:graphic>"#,
            r#"</wp:inline></w:drawing></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp>"#,
            r#"</a:graphicData></a:graphic></wp:inline></w:drawing></mc:Choice>"#,
            r#"<mc:Fallback><w:pict><v:shape style="width:1pt;height:1pt"><v:imagedata r:id="rIdFallback"/></v:shape></w:pict></mc:Fallback>"#,
            r#"</mc:AlternateContent>"#,
        );
        let s = scan_drawing(xml.as_bytes());
        assert_eq!(s.rel_id, None, "the inner story's picture is not the box's");
        assert_eq!((s.cx, s.cy), (Some(100), Some(200)));
        assert!(s.anchor.is_none());
        assert!(s.drawing_ml);
        assert_eq!(s.image_fields(), (String::new(), 100, 200));
    }

    #[test]
    fn vml_picture_and_horizontal_rule_scan_style_sizes() {
        let pict = r##"<w:pict><v:shape id="_x0000_i1025" type="#_x0000_t75" style="width:2in;height:36pt"><v:imagedata r:id="rId8" o:title=""/></v:shape></w:pict>"##;
        let s = scan_drawing(pict.as_bytes());
        assert_eq!(s.rel_id.as_deref(), Some("rId8"));
        assert_eq!((s.cx, s.cy), (Some(1_828_800), Some(457_200)));
        assert!(!s.drawing_ml);
        let hr = r##"<w:pict><v:rect id="_x0000_i1026" style="width:0;height:1.5pt" o:hralign="center" o:hrstd="t" o:hr="t" fillcolor="#a0a0a0" stroked="f"/></w:pict>"##;
        let s = scan_drawing(hr.as_bytes());
        assert_eq!(s.rel_id, None);
        assert_eq!((s.cx, s.cy), (Some(0), Some(19_050)));
        /* Absolutely positioned VML reserves nothing. */
        let abs = r#"<w:pict><v:shape style="position:absolute;margin-left:10pt;width:100pt;height:50pt"><v:textbox/></v:shape></w:pict>"#;
        let s = scan_drawing(abs.as_bytes());
        assert_eq!((s.cx, s.cy), (Some(0), Some(0)));
        let ole = r#"<w:object w:dxaOrig="1440" w:dyaOrig="720"><v:shape style="width:72pt;height:36pt"><v:imagedata r:id="rId4"/></v:shape><o:OLEObject Type="Embed" r:id="rId5"/></w:object>"#;
        let s = scan_drawing(ole.as_bytes());
        assert_eq!(s.rel_id.as_deref(), Some("rId4"));
        assert_eq!((s.cx, s.cy), (Some(914_400), Some(457_200)));
    }

    #[test]
    fn vml_lengths_convert_to_emu() {
        assert_eq!(vml_length_emu("72pt"), Some(914_400));
        assert_eq!(vml_length_emu("1in"), Some(914_400));
        assert_eq!(vml_length_emu("2.54cm"), Some(914_400));
        assert_eq!(vml_length_emu("25.4mm"), Some(914_400));
        assert_eq!(vml_length_emu("96px"), Some(914_400));
        assert_eq!(vml_length_emu("914400emu"), Some(914_400));
        assert_eq!(vml_length_emu("0"), Some(0));
        assert_eq!(vml_length_emu("50%"), None);
        assert_eq!(vml_length_emu("x"), None);
    }

    #[test]
    fn garbage_scans_as_an_opaque_object() {
        let s = scan_drawing(b"<w:drawing><broken");
        assert_eq!(
            s,
            DrawingScan {
                drawing_ml: true,
                ..DrawingScan::default()
            }
        );
        assert_eq!(s.image_fields(), (String::new(), 0, 0));
    }
}
