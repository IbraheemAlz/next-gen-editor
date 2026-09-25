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
    ListItem, MarkerRole, ParaProperties, RunPad, SourceAttr, SourceMarker, SourceMarkup,
    SourcePPr, SourceRun, SpanStyle,
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
/// level (`DocumentTree::comment_ranges`), and a verbatim copy would
/// resurrect a comment the user deleted.
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
/// to the run (field machinery, note / comment references). A text-less
/// run holding one of these regenerates from the model; any OTHER
/// text-less run is kept verbatim as a marker.
pub fn is_modeled_textless_run_child(qname: &[u8]) -> bool {
    matches!(
        qname,
        b"w:fldChar"
            | b"w:instrText"
            | b"w:delInstrText"
            | b"w:commentReference"
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
    /// Issue #245 — pretty-print whitespace between the run's children.
    pad: RunPad,
    pad_state: PadState,
    /// Whitespace since the last child ended (becomes `pad.close` at the
    /// run end).
    trail: Vec<u8>,
}

/// Issue #245 — where run-level whitespace lands in [`RunPad`].
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum PadState {
    /// No child yet: `open`.
    #[default]
    Start,
    /// Right after the `<w:rPr>`: `after_rpr`.
    AfterRpr,
    /// After a content child: the trailing whitespace, if no child
    /// follows.
    Content,
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
    /// Issue #244 — complex fields begun in this paragraph, innermost last.
    field_spans: Vec<FieldSpanCapture>,
    /// Issue #244 — a zero-result field whose `end` fldChar sits in the
    /// open run: kept whole when that run closes without text.
    field_due: Option<DueFieldSpan>,
    /// Issue #245 — run-level `<w:sdt>` elements open in this paragraph,
    /// innermost last.
    sdts: Vec<SdtCapture>,
}

/// Issue #245 — one run-level `<w:sdt>` being read.
struct SdtCapture {
    /// Byte offset of `<w:sdt`.
    start: usize,
    /// Index of its opener in `markers` once `<w:sdtContent>` began
    /// (`None`: still in the `sdtPr` head, or an unbindable opener).
    opener: Option<usize>,
    /// The opener's bytes were unusable: the content is kept, the
    /// wrapper is not (the pre-#245 flattening).
    unusable: bool,
    /// Byte offset of `</w:sdtContent>` once it began.
    content_end: Option<usize>,
}

impl SdtCapture {
    /// Between `<w:sdt>` and `<w:sdtContent>`, or after `</w:sdtContent>`:
    /// whitespace there is inside the opener / closer bytes.
    fn outside_content(&self) -> bool {
        (self.opener.is_none() && !self.unusable) || self.content_end.is_some()
    }
}

/// Issue #244 — one complex field begun inside the current paragraph.
struct FieldSpanCapture {
    depth: usize,
    at: u32,
    /// Byte offset of the `<w:r` holding the `begin`; `None` when the
    /// field is not eligible to be kept whole.
    xml_start: Option<usize>,
    /// `markers.len()` when the field began: markers captured inside the
    /// field are replaced by the whole span.
    marker_mark: usize,
}

struct DueFieldSpan {
    xml_start: usize,
    at: u32,
    marker_mark: usize,
}

/// `true` when `frag` is a sequence of complete, balanced elements (every
/// end tag closes the element its start opened, nothing is left open), so
/// it can be re-emitted between two runs without breaking the part.
pub fn is_balanced_fragment(frag: &[u8]) -> bool {
    let mut reader = Reader::from_reader(frag);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut stack: Vec<Vec<u8>> = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => stack.push(e.name().as_ref().to_vec()),
            Ok(Event::End(e)) => {
                if stack.pop().as_deref() != Some(e.name().as_ref()) {
                    return false;
                }
            }
            Ok(Event::Eof) => return stack.is_empty(),
            Ok(_) => {}
            Err(_) => return false,
        }
        buf.clear();
    }
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
                self.markers.push(SourceMarker::verbatim(at, ws));
            }
        }
    }

    /// Whitespace character data between paragraph children.
    pub fn whitespace(&mut self, at: u32, frag: Vec<u8>) {
        if !self.open || self.run.is_some() {
            return;
        }
        if self.sdts.last().is_some_and(SdtCapture::outside_content) {
            return;
        }
        if self.content_seen {
            self.markers.push(SourceMarker::verbatim(at, frag));
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
            self.markers.push(SourceMarker::verbatim(at, frag));
        }
    }

    /// Issue #245 — a run-level `<w:sdt>` start tag at byte `start`.
    pub fn sdt_start(&mut self, start: usize) {
        if !self.open || self.run.is_some() {
            return;
        }
        self.sdts.push(SdtCapture {
            start,
            opener: None,
            unusable: false,
            content_end: None,
        });
    }

    /// Issue #245 — the `<w:sdtContent>` start tag of the innermost open
    /// `<w:sdt>` ended at byte `end` (text offset `at`): everything since
    /// `<w:sdt` is the opener. `empty` for a self-closing
    /// `<w:sdtContent/>` (the content also ends here).
    pub fn sdt_content_start(
        &mut self,
        xml: &[u8],
        end: usize,
        at: u32,
        ns: &NamespaceScope,
        empty: bool,
    ) {
        let Some(top) = self.sdts.last() else {
            return;
        };
        if top.opener.is_some() || top.unusable || top.content_end.is_some() {
            return;
        }
        let id = top.start as u32;
        let bytes = xml.get(top.start..end).map(<[u8]>::to_vec);
        self.content(at);
        let top = self.sdts.last_mut().expect("checked above");
        match bytes {
            Some(bytes) if bound_by_root(&bytes, ns) && !empty => {
                top.opener = Some(self.markers.len());
                self.markers.push(SourceMarker {
                    at,
                    xml: bytes,
                    role: MarkerRole::Open {
                        id,
                        close_xml: b"</w:sdtContent></w:sdt>".to_vec(),
                    },
                });
            }
            /* `<w:sdtContent/>`: nothing to wrap, the element is whole at
            its end tag. */
            Some(_) if empty => top.content_end = Some(end),
            _ => top.unusable = true,
        }
    }

    /// Issue #245 — `</w:sdtContent>` of the innermost open `<w:sdt>`
    /// starts at byte `start`.
    pub fn sdt_content_end(&mut self, start: usize) {
        if let Some(top) = self.sdts.last_mut()
            && top.content_end.is_none()
        {
            top.content_end = Some(start);
        }
    }

    /// Issue #245 — `</w:sdt>` (or a self-closing `<w:sdt/>`) of the
    /// innermost open `<w:sdt>` ended at byte `end`, text offset `at`.
    /// With an opener: the closer marker (`</w:sdtContent>…</w:sdt>`),
    /// whose bytes also become the opener's fallback `close_xml`. Without
    /// content (`<w:sdtContent/>`, no `<w:sdtContent>` at all): the whole
    /// element is one content marker.
    pub fn sdt_end(&mut self, xml: &[u8], end: usize, at: u32, ns: &NamespaceScope) {
        let Some(top) = self.sdts.pop() else {
            return;
        };
        if top.unusable {
            return;
        }
        if let Some(opener) = top.opener {
            let close = top
                .content_end
                .and_then(|s| xml.get(s..end))
                .filter(|b| bound_by_root(b, ns))
                .map_or_else(|| b"</w:sdtContent></w:sdt>".to_vec(), <[u8]>::to_vec);
            if let Some(MarkerRole::Open { close_xml, .. }) =
                self.markers.get_mut(opener).map(|m| &mut m.role)
            {
                close_xml.clone_from(&close);
            }
            self.markers.push(SourceMarker {
                at,
                xml: close,
                role: MarkerRole::Close {
                    id: top.start as u32,
                },
            });
        } else if let Some(frag) = xml.get(top.start..end)
            && bound_by_root(frag, ns)
            && is_balanced_fragment(frag)
        {
            self.content(at);
            self.markers.push(SourceMarker {
                at,
                xml: frag.to_vec(),
                role: MarkerRole::Content,
            });
        }
    }

    /// Issue #245 — whitespace character data directly inside the open
    /// run (between its children).
    pub fn run_whitespace(&mut self, frag: &[u8]) {
        if let Some(r) = self.run.as_mut() {
            match r.pad_state {
                PadState::Start => r.pad.open.extend_from_slice(frag),
                PadState::AfterRpr => r.pad.after_rpr.extend_from_slice(frag),
                PadState::Content => r.trail.extend_from_slice(frag),
            }
        }
    }

    /// Issue #245 — a content child (not the `<w:rPr>`) of the open run
    /// starts: whitespace before it is not trailing.
    pub fn run_child_start(&mut self) {
        if let Some(r) = self.run.as_mut() {
            r.pad_state = PadState::Content;
            r.trail.clear();
        }
    }

    /// Issue #245 — an element inside the open run ended: whitespace
    /// before it was nested, not trailing.
    pub fn run_child_end(&mut self) {
        if let Some(r) = self.run.as_mut()
            && r.pad_state == PadState::Content
        {
            r.trail.clear();
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
        if let Some(r) = self.run.as_mut()
            && r.pad_state == PadState::Start
        {
            r.pad_state = PadState::AfterRpr;
        }
    }

    /// `<w:rPr/>` inside the open run.
    pub fn run_rpr_empty(&mut self, frag: Vec<u8>) {
        if let Some(r) = self.run.as_mut()
            && r.rpr.is_none()
        {
            r.rpr = Some(frag);
            if r.pad_state == PadState::Start {
                r.pad_state = PadState::AfterRpr;
            }
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
    pub fn close_text_run(&mut self, start: u32, end: u32, style: &SpanStyle, text: &str) {
        /* Text after the `end` fldChar in the same run: not a clean span. */
        self.field_due = None;
        if let Some(mut r) = self.run.take() {
            r.pad.close = r.trail;
            let pad = (r.pad != RunPad::default()).then(|| Box::new(r.pad));
            let bare_edge_ws = r
                .t_attrs
                .as_ref()
                .is_some_and(|a| !a.iter().any(|a| a.name == "xml:space"))
                && text_needs_preserve(text);
            self.runs.push(SourceRun {
                start,
                end,
                attrs: r.attrs,
                rpr: r.rpr,
                style: style.clone(),
                lead: r.lead,
                t_attrs: r.t_attrs,
                pad,
                bare_edge_ws,
            });
        }
    }

    /// `</w:r>` of a run that produced no text, ending at byte `end`
    /// (text offset `at`): unless it held modeled content, the whole run
    /// is kept verbatim as a marker.
    pub fn close_textless_run(&mut self, xml: &[u8], end: usize, at: u32, ns: &NamespaceScope) {
        let Some(r) = self.run.take() else {
            return;
        };
        /* Issue #244 — the run holding the `end` fldChar of a zero-result
        field: the whole `begin … end` range becomes ONE content marker,
        replacing the markers captured inside it (the textless runs, the
        form field's name bookmark). */
        if let Some(span) = self.field_due.take()
            && span.at == at
            && let Some(frag) = xml.get(span.xml_start..end)
            && frag.starts_with(b"<w:r")
            && is_balanced_fragment(frag)
            && bound_by_root(frag, ns)
        {
            self.markers.truncate(span.marker_mark);
            self.markers.push(SourceMarker {
                at,
                xml: frag.to_vec(),
                role: MarkerRole::Content,
            });
            return;
        }
        if !r.has_modeled
            && let Some(frag) = xml.get(r.xml_start..end)
            && frag.starts_with(b"<w:r")
            && bound_by_root(frag, ns)
        {
            self.markers.push(SourceMarker::verbatim(at, frag.to_vec()));
        }
    }

    /// Issue #244 — a `<w:fldChar w:fldCharType="begin">` inside the open
    /// run, at text offset `at`; `depth` is the field nesting depth it
    /// opened (1 = outermost). `eligible` is false when the run already
    /// produced text before the `begin` or an enclosing field is still in
    /// its instruction part (a field nested in an instruction must stay
    /// inside it; only the enclosing field's own span may keep it).
    pub fn field_begin(&mut self, at: u32, depth: usize, eligible: bool) {
        if !self.open {
            return;
        }
        let xml_start = match self.run.as_ref() {
            Some(r) if eligible => Some(r.xml_start),
            _ => None,
        };
        self.field_spans.push(FieldSpanCapture {
            depth,
            at,
            xml_start,
            marker_mark: self.markers.len(),
        });
    }

    /// Issue #244 — the `end` fldChar of the field at nesting `depth`, at
    /// text offset `at`. `modeled` is true when the reader turned the
    /// field into a model overlay (it has a result, or it is a TOC): only
    /// an UNMODELED zero-result field is kept whole, at the close of the
    /// run holding its `end`.
    pub fn field_end(&mut self, depth: usize, at: u32, modeled: bool) {
        self.field_due = None;
        while self.field_spans.last().is_some_and(|f| f.depth > depth) {
            self.field_spans.pop();
        }
        if self.field_spans.last().is_some_and(|f| f.depth == depth)
            && let Some(span) = self.field_spans.pop()
            && let Some(xml_start) = span.xml_start
            && !modeled
            && span.at == at
            && self.run.is_some()
        {
            self.field_due = Some(DueFieldSpan {
                xml_start,
                at,
                marker_mark: span.marker_mark,
            });
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
