//! Issues #199 / #106 — attribute-level grab bag + in-paragraph source
//! markup, read and write sides.
//!
//! The element-level grab bags of #84 ([`crate::schema::grab_bag`]) keep
//! every unmodeled `<w:pPr>` / `<w:rPr>` *child*. What a regenerated
//! paragraph still lost is everything that is not a property child:
//!
//! - the attributes of `<w:p>` (`w:rsidR`, `w:rsidRDefault`, `w14:paraId`,
//!   `w14:textId`, …), `<w:r>` (`w:rsidR`, `w:rsidRPr`, `w:rsidDel`) and
//!   `<w:t>` (a bare `<w:t>` vs `xml:space="preserve"`);
//! - the unread attributes of *modeled* property children (`<w:u
//!   w:color>`, `<w:rFonts w:eastAsia w:hint>`, `<w:color w:themeColor>`,
//!   `<w:shd w:themeFill>`, …) and the source spelling of the property
//!   elements (`w:left` vs `w:start`, `<w:b w:val="1"/>`, attribute order);
//! - the source run boundaries (adjacent runs with equal formatting were
//!   coalesced into one span);
//! - in-paragraph markers the model does not represent (`<w:proofErr/>`,
//!   non-TOC bookmarks, permission ranges, text-less runs such as a lone
//!   `<w:lastRenderedPageBreak/>`, pretty-print whitespace).
//!
//! The reader records them on [`engine::SourceMarkup`] ([`MarkupCapture`]);
//! the writer ([`attrs_xml`], [`adopt_source_children`], and
//! `crate::writer`'s run walk) re-emits them:
//!
//! - `<w:p>` / `<w:r>` attributes: always (after any modeled attribute —
//!   neither element has one);
//! - the source `<w:pPr>` / `<w:rPr>` bytes: verbatim only while the model
//!   state they produced is unchanged (a *verified* passthrough); otherwise
//!   the element regenerates and each regenerated *empty* child adopts its
//!   source twin when that twin carries every regenerated attribute with
//!   the same value and differs only by extra attributes other than `w:val`
//!   ([`adopt_source_children`] — the #106 attribute bag for modeled
//!   elements: `<w:u w:val="single" w:color="FF0000"/>` keeps its colour
//!   when the run is bolded, but a changed font never inherits the source's
//!   `w:asciiTheme`, which would override it);
//! - markers and run boundaries: at their (remapped) text offsets.

use crate::schema::grab_bag::{NamespaceScope, bound_by_root};
use engine::{
    CommentAnchor, CommentAnchorKind, ListItem, ParaProperties, SourceAttr, SourceMarker,
    SourceMarkup, SourcePPr, SourceRun, SpanStyle,
};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

/// Raw attributes of `e`, source order, values in their escaped form (a
/// `"` inside a single-quoted source value is re-escaped so the writer can
/// always paste into double quotes). An attribute whose prefix the writer
/// cannot bind (neither `w`, `xml`, declared on `e` itself, nor bound on
/// the part root) is dropped; the element's own `xmlns:*` declarations are
/// kept with it.
pub fn raw_attrs(e: &BytesStart, ns: &NamespaceScope) -> Vec<SourceAttr> {
    let own: Vec<Vec<u8>> = e
        .attributes()
        .flatten()
        .filter_map(|a| a.key.as_ref().strip_prefix(b"xmlns:").map(<[u8]>::to_vec))
        .collect();
    let mut out = Vec::new();
    for a in e.attributes().with_checks(false).flatten() {
        let key = a.key.as_ref();
        let bound = match key.iter().position(|&b| b == b':') {
            None => true,
            Some(i) => {
                let p = &key[..i];
                p == b"w"
                    || p == b"xml"
                    || p == b"xmlns"
                    || own.iter().any(|o| o == p)
                    || ns.uri(&String::from_utf8_lossy(p)).is_some()
            }
        };
        if !bound {
            continue;
        }
        let value = String::from_utf8_lossy(&a.value).replace('"', "&quot;");
        out.push(SourceAttr {
            name: String::from_utf8_lossy(key).into_owned(),
            value,
        });
    }
    out
}

/// ` name="value"` for every attribute, in order.
pub fn attrs_xml(attrs: &[SourceAttr], out: &mut String) {
    for a in attrs {
        out.push(' ');
        out.push_str(&a.name);
        out.push_str("=\"");
        out.push_str(&a.value);
        out.push('"');
    }
}

/// In-paragraph empty elements the model does not represent that ride a
/// regenerated paragraph as positioned verbatim markers. Comment ranges
/// and references are deliberately NOT here: they are modeled at the tree
/// level (`DocumentTree::comment_ranges`), and a blind verbatim copy would
/// resurrect a comment the user deleted — they ride as *comment-anchor*
/// markers the writer verifies against the tree instead (issue #243,
/// [`MarkupCapture::comment_marker`]).
pub fn is_inline_marker(qname: &[u8]) -> bool {
    matches!(
        qname,
        b"w:proofErr"
            | b"w:bookmarkStart"
            | b"w:bookmarkEnd"
            | b"w:permStart"
            | b"w:permEnd"
            | b"w:moveFromRangeStart"
            | b"w:moveFromRangeEnd"
            | b"w:moveToRangeStart"
            | b"w:moveToRangeEnd"
            /* Only ever reached for the EMPTY form: a result-less
            `<w:fldSimple …/>` produces no field in the model. */
            | b"w:fldSimple"
    )
}

/// Run children that carry modeled content even though they add no text
/// to the run (field machinery, note references). A text-less run holding
/// one of these regenerates from the model; any OTHER text-less run is
/// kept verbatim as a marker — a `<w:commentReference/>` run as a
/// *comment-anchor* marker (issue #243, [`MarkupCapture::run_comment_reference`]).
pub fn is_modeled_textless_run_child(qname: &[u8]) -> bool {
    matches!(
        qname,
        b"w:fldChar"
            | b"w:instrText"
            | b"w:delInstrText"
            | b"w:footnoteReference"
            | b"w:endnoteReference"
            | b"w:footnoteRef"
            | b"w:endnoteRef"
            | b"w:drawing"
            | b"w:pict"
            | b"w:object"
            | b"mc:AlternateContent"
    )
}

#[derive(Default)]
struct RunCapture {
    xml_start: usize,
    attrs: Vec<SourceAttr>,
    rpr_start: Option<usize>,
    rpr: Option<Vec<u8>>,
    lead: Vec<u8>,
    t_attrs: Option<Vec<SourceAttr>>,
    has_modeled: bool,
    /// Issue #243 — the `w:id` of a `<w:commentReference>` in the run.
    comment_ref: Option<u32>,
}

/// Reader-side accumulator for one paragraph's [`SourceMarkup`]. The part
/// parser drives it from its event loop; every method is a no-op while no
/// paragraph is open, so nested-story guards stay the parser's business.
#[derive(Default)]
pub struct MarkupCapture {
    open: bool,
    attrs: Vec<SourceAttr>,
    /// Byte just past the `<w:p …>` start tag.
    p_open_end: usize,
    ppr_xml: Option<Vec<u8>>,
    ppr_unusable: bool,
    content_seen: bool,
    pending_ws: Vec<Vec<u8>>,
    runs: Vec<SourceRun>,
    markers: Vec<SourceMarker>,
    run: Option<RunCapture>,
}

impl MarkupCapture {
    pub fn new() -> Self {
        Self::default()
    }

    /// A `<w:p>` start tag `e` was read; `open_end` is the byte after it.
    pub fn open_paragraph(&mut self, e: &BytesStart, ns: &NamespaceScope, open_end: usize) {
        *self = Self {
            open: true,
            attrs: raw_attrs(e, ns),
            p_open_end: open_end,
            ..Self::default()
        };
    }

    /// `</w:pPr>` (or `<w:pPr/>`) ended at byte `end`: the paragraph's
    /// property region is `p_open_end..end` (leading whitespace included).
    pub fn close_ppr(&mut self, xml: &[u8], end: usize, ns: &NamespaceScope) {
        if !self.open || self.ppr_xml.is_some() || self.content_seen {
            return;
        }
        let Some(bytes) = xml.get(self.p_open_end..end) else {
            return;
        };
        /* Leading whitespace went into the region; it is no marker. */
        self.pending_ws.clear();
        if bound_by_root(bytes, ns) {
            self.ppr_xml = Some(bytes.to_vec());
        } else {
            /* Unbindable prefix inside the pPr: regenerate instead. */
            self.ppr_unusable = true;
        }
    }

    /// A `<w:sectPr>` inside the paragraph's `<w:pPr>`: the section
    /// marker moves on edits, so the pPr bytes are never reused.
    pub fn note_sect_in_ppr(&mut self) {
        self.ppr_unusable = true;
    }

    fn content(&mut self, at: u32) {
        if !self.content_seen {
            self.content_seen = true;
            for ws in std::mem::take(&mut self.pending_ws) {
                self.markers.push(SourceMarker {
                    at,
                    xml: ws,
                    ..SourceMarker::default()
                });
            }
        }
    }

    /// Whitespace character data between paragraph children.
    pub fn whitespace(&mut self, at: u32, frag: Vec<u8>) {
        if !self.open || self.run.is_some() {
            return;
        }
        if self.content_seen {
            self.markers.push(SourceMarker {
                at,
                xml: frag,
                ..SourceMarker::default()
            });
        } else {
            self.pending_ws.push(frag);
        }
    }

    /// An unmodeled in-paragraph element ([`is_inline_marker`]).
    pub fn marker(&mut self, at: u32, frag: Vec<u8>, ns: &NamespaceScope) {
        if !self.open || self.run.is_some() {
            return;
        }
        self.content(at);
        if bound_by_root(&frag, ns) {
            self.markers.push(SourceMarker {
                at,
                xml: frag,
                ..SourceMarker::default()
            });
        }
    }

    /// `<w:r …>` start tag `e` read at byte `start` (text offset `at`).
    pub fn open_run(&mut self, e: &BytesStart, ns: &NamespaceScope, start: usize, at: u32) {
        if !self.open {
            return;
        }
        self.content(at);
        self.run = Some(RunCapture {
            xml_start: start,
            attrs: raw_attrs(e, ns),
            ..RunCapture::default()
        });
    }

    /// `<w:rPr>` start tag inside the open run, at byte `start`.
    pub fn run_rpr_start(&mut self, start: usize) {
        if let Some(r) = self.run.as_mut()
            && r.rpr.is_none()
        {
            r.rpr_start = Some(start);
        }
    }

    /// `</w:rPr>` of the open run ended at byte `end`.
    pub fn run_rpr_end(&mut self, xml: &[u8], end: usize, ns: &NamespaceScope) {
        if let Some(r) = self.run.as_mut()
            && let Some(start) = r.rpr_start.take()
            && let Some(bytes) = xml.get(start..end)
            && bound_by_root(bytes, ns)
        {
            r.rpr = Some(bytes.to_vec());
        }
    }

    /// `<w:rPr/>` inside the open run.
    pub fn run_rpr_empty(&mut self, frag: Vec<u8>) {
        if let Some(r) = self.run.as_mut()
            && r.rpr.is_none()
        {
            r.rpr = Some(frag);
        }
    }

    /// `<w:t …>` / `<w:delText …>` start tag inside the open run.
    pub fn run_text_elt(&mut self, e: &BytesStart, ns: &NamespaceScope) {
        if let Some(r) = self.run.as_mut()
            && r.t_attrs.is_none()
        {
            r.t_attrs = Some(raw_attrs(e, ns));
        }
    }

    /// Unmodeled leading run content (`<w:lastRenderedPageBreak/>`) seen
    /// before the run produced any text; later occurrences are dropped
    /// (Word recomputes them).
    pub fn run_lead(&mut self, frag: &[u8], run_text_empty: bool) {
        if let Some(r) = self.run.as_mut()
            && run_text_empty
        {
            r.lead.extend_from_slice(frag);
        }
    }

    /// The open run holds modeled text-less content
    /// ([`is_modeled_textless_run_child`]).
    pub fn run_modeled(&mut self) {
        if let Some(r) = self.run.as_mut() {
            r.has_modeled = true;
        }
    }

    /// `</w:r>` of a run that produced text bytes `[start, end)` with the
    /// resolved span `style`.
    pub fn close_text_run(&mut self, start: u32, end: u32, style: &SpanStyle) {
        if let Some(r) = self.run.take() {
            self.runs.push(SourceRun {
                start,
                end,
                attrs: r.attrs,
                rpr: r.rpr,
                style: style.clone(),
                lead: r.lead,
                t_attrs: r.t_attrs,
            });
        }
    }

    /// `</w:r>` of a run that produced no text, ending at byte `end`
    /// (text offset `at`): unless it held modeled content, the whole run
    /// is kept verbatim as a marker.
    pub fn close_textless_run(&mut self, xml: &[u8], end: usize, at: u32, ns: &NamespaceScope) {
        if let Some(r) = self.run.take()
            && !r.has_modeled
            && let Some(frag) = xml.get(r.xml_start..end)
            && frag.starts_with(b"<w:r")
            && bound_by_root(frag, ns)
        {
            self.markers.push(SourceMarker {
                at,
                xml: frag.to_vec(),
                comment: r.comment_ref.map(|id| CommentAnchor {
                    kind: CommentAnchorKind::Reference,
                    id,
                }),
            });
        }
    }

    /// Issue #243 — a `<w:commentRangeStart/>` / `<w:commentRangeEnd/>`
    /// between runs: a comment-anchor marker the writer verifies against
    /// the tree-level `comment_ranges` before replaying it.
    pub fn comment_marker(
        &mut self,
        at: u32,
        frag: Vec<u8>,
        anchor: CommentAnchor,
        ns: &NamespaceScope,
    ) {
        if !self.open || self.run.is_some() {
            return;
        }
        self.content(at);
        if bound_by_root(&frag, ns) {
            self.markers.push(SourceMarker {
                at,
                xml: frag,
                comment: Some(anchor),
            });
        }
    }

    /// Issue #243 — the open run holds `<w:commentReference w:id>`: a
    /// text-less one is kept whole as a comment-anchor marker.
    pub fn run_comment_reference(&mut self, id: u32) {
        if let Some(r) = self.run.as_mut() {
            r.comment_ref = Some(id);
        }
    }

    /// `</w:p>`: the finished record, `None` when there is nothing worth
    /// keeping. `props` / `style_id` / `list_item` are the model state the
    /// paragraph was built with — the writer's verification baseline.
    pub fn finish(
        &mut self,
        text_len: u32,
        props: &ParaProperties,
        style_id: &Option<String>,
        list_item: Option<ListItem>,
    ) -> Option<Box<SourceMarkup>> {
        if !self.open {
            return None;
        }
        let mut done = std::mem::take(self);
        let at = text_len;
        done.content(at);
        let ppr = (!done.ppr_unusable).then(|| SourcePPr {
            xml: done.ppr_xml.unwrap_or_default(),
            props: props.clone(),
            style_id: style_id.clone(),
            list_item,
        });
        Some(Box::new(SourceMarkup {
            text_len,
            attrs: done.attrs,
            ppr,
            runs: done.runs,
            markers: done.markers,
        }))
    }
}

/// Raw `(qualified name, value)` attribute pairs of one element.
type RawAttrs = Vec<(Vec<u8>, Vec<u8>)>;

/// One top-level child of a property element: `(qname, attributes, raw
/// bytes, empty)`.
type Child = (Vec<u8>, RawAttrs, Vec<u8>, bool);

/// The complete top-level children of a `<w:pPr>` / `<w:rPr>` fragment
/// (`xml` includes the wrapper).
fn top_level_children(xml: &[u8]) -> Vec<Child> {
    let mut out = Vec::new();
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut depth = 0usize;
    let mut open: Option<(usize, Vec<u8>, RawAttrs)> = None;
    let mut prev = 0usize;
    loop {
        let ev = match reader.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(_) => return Vec::new(),
        };
        let pos = reader.buffer_position() as usize;
        match ev {
            Event::Start(e) => {
                if depth == 1 {
                    open = Some((prev, e.name().as_ref().to_vec(), attrs_of(&e)));
                }
                depth += 1;
            }
            Event::Empty(e) if depth == 1 => {
                out.push((
                    e.name().as_ref().to_vec(),
                    attrs_of(&e),
                    xml[prev..pos].to_vec(),
                    true,
                ));
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 1
                    && let Some((s, name, attrs)) = open.take()
                {
                    out.push((name, attrs, xml[s..pos].to_vec(), false));
                }
            }
            Event::Eof => break,
            _ => {}
        }
        prev = pos;
        buf.clear();
    }
    out
}

fn attrs_of(e: &BytesStart) -> RawAttrs {
    e.attributes()
        .with_checks(false)
        .flatten()
        .map(|a| (a.key.as_ref().to_vec(), a.value.to_vec()))
        .collect()
}

/// One regenerated child element: `Some((qname, attrs))` when `xml` is a
/// single empty element (`<w:u w:val="single"/>`).
fn parse_empty_element(xml: &str) -> Option<(Vec<u8>, RawAttrs)> {
    let mut reader = Reader::from_reader(xml.as_bytes());
    let mut buf = Vec::new();
    let first = match reader.read_event_into(&mut buf).ok()? {
        Event::Empty(e) => (e.name().as_ref().to_vec(), attrs_of(&e)),
        _ => return None,
    };
    buf.clear();
    matches!(reader.read_event_into(&mut buf).ok()?, Event::Eof).then_some(first)
}

/// Issue #106 — the attribute bag for modeled property elements. For each
/// regenerated `(rank, xml)` child that is a single empty element, find the
/// first empty top-level child of the same name in `source` (the raw
/// `<w:rPr>` / `<w:pPr>` the element was read from). When the source twin
/// carries every regenerated attribute with the identical value and its
/// only extras are attributes other than `w:val` (whose absence means
/// "on" for toggles — `<w:b/>` must never adopt `<w:b w:val="0"/>`), the
/// source bytes replace the regenerated ones: the unread attributes
/// (`w:color` on `<w:u>`, `w:eastAsia` / `w:hint` on `<w:rFonts>`,
/// `w:themeColor` on `<w:color>`, …) survive a formatting edit elsewhere
/// in the element set. A changed modeled value fails the equality test,
/// so a stale theme binding can never override a new explicit value.
pub fn adopt_source_children(children: &mut [(u16, String)], source: &[u8]) {
    let twins = top_level_children(source);
    if twins.is_empty() {
        return;
    }
    let mut used = vec![false; twins.len()];
    for (_, xml) in children.iter_mut() {
        let Some((name, attrs)) = parse_empty_element(xml) else {
            continue;
        };
        let Some(i) = twins
            .iter()
            .enumerate()
            .position(|(i, t)| !used[i] && t.3 && t.0 == name)
        else {
            continue;
        };
        let (_, src_attrs, raw, _) = &twins[i];
        let superset = attrs
            .iter()
            .all(|(k, v)| src_attrs.iter().any(|(sk, sv)| sk == k && sv == v));
        let extras_ok = src_attrs
            .iter()
            .filter(|(sk, _)| !attrs.iter().any(|(k, _)| k == sk))
            .all(|(sk, _)| sk.as_slice() != b"w:val");
        if superset
            && extras_ok
            && let Ok(s) = std::str::from_utf8(raw)
        {
            *xml = s.to_owned();
            used[i] = true;
        }
    }
}

/// `true` when a `<w:t>` holding `text` needs `xml:space="preserve"`:
/// leading / trailing whitespace would otherwise be dropped by a
/// consumer applying default whitespace handling.
pub fn text_needs_preserve(text: &str) -> bool {
    text.starts_with(char::is_whitespace) || text.ends_with(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn children(xs: &[&str]) -> Vec<(u16, String)> {
        xs.iter().map(|x| (0u16, (*x).to_string())).collect()
    }

    #[test]
    fn adopt_keeps_unread_attributes_of_an_unchanged_child() {
        let src = br#"<w:rPr><w:b/><w:u w:val="single" w:color="FF0000"/></w:rPr>"#;
        let mut ch = children(&[r#"<w:i/>"#, r#"<w:u w:val="single"/>"#]);
        adopt_source_children(&mut ch, src);
        assert_eq!(ch[1].1, r#"<w:u w:val="single" w:color="FF0000"/>"#);
        assert_eq!(ch[0].1, "<w:i/>");
    }

    #[test]
    fn adopt_refuses_a_changed_value_and_a_val_extra() {
        let src =
            br#"<w:rPr><w:b w:val="0"/><w:rFonts w:ascii="A" w:asciiTheme="minorHAnsi"/></w:rPr>"#;
        let mut ch = children(&[r#"<w:b/>"#, r#"<w:rFonts w:ascii="B"/>"#]);
        adopt_source_children(&mut ch, src);
        assert_eq!(ch[0].1, "<w:b/>", "a w:val extra is semantic");
        assert_eq!(ch[1].1, r#"<w:rFonts w:ascii="B"/>"#, "changed font");
    }

    #[test]
    fn adopt_ignores_container_children() {
        let src = br#"<w:pPr><w:tabs><w:tab w:val="left" w:pos="720"/></w:tabs></w:pPr>"#;
        let mut ch = children(&[r#"<w:tabs><w:tab w:val="left" w:pos="1440"/></w:tabs>"#]);
        adopt_source_children(&mut ch, src);
        assert!(ch[0].1.contains("1440"));
    }

    #[test]
    fn preserve_is_needed_only_for_edge_whitespace() {
        assert!(!text_needs_preserve("a b"));
        assert!(text_needs_preserve(" a"));
        assert!(text_needs_preserve("a\t"));
        assert!(!text_needs_preserve(""));
    }
}
