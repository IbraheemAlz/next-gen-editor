//! `word/document.xml` — parse paragraphs + runs into a `DocumentTree`.
//!
//! Phase 3 additions:
//!
//! - **Style cascade.** A [`StyleResolver`] (Phase 3) folds doc defaults +
//!   `<w:basedOn>` chain + direct `<w:pPr>` / `<w:rPr>` into the engine's
//!   flat `ParaProperties` / `SpanStyle`. The engine never sees a
//!   `pStyle` / `rStyle` reference.
//! - **Source-byte capture.** Each `<w:p>` records its exact byte range
//!   in the source `document.xml`; the writer emits these bytes verbatim
//!   when the engine has not mutated the paragraph (the passthrough
//!   optimisation; zero document.xml drift on untouched paragraphs).

use crate::error::{DocxError, DocxWarning};
use crate::parts::table::parse_table_bytes_with_warnings;
use crate::schema::ct_ppr::{apply_ppr, ppr_child_is_modeled};
use crate::schema::ct_rpr::{apply_rpr, attr_val, fold_rpr_fragment, rpr_child_is_modeled};
use crate::schema::grab_bag::{
    NamespaceScope, capture_subtree, slice_element, slice_fragment, stash,
};
use crate::schema::wp_anchor::{
    AnchorAxis, AnchorOffsetKind, anchor_from_start_tag, h_relative_from, is_wrap_element,
    parse_offset, v_relative_from, wrap_kind_of,
};
use crate::style_resolver::StyleResolver;
use engine::{
    Block, DocumentTree, HeaderFooterRefs, HeaderFooterRole, ListItem, PageGeometry,
    ParaProperties, Paragraph, Section, SpanStyle, StyleRun, Table,
};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

/// Twips (1/20 pt) → layout pt. OOXML page geometry is encoded in twips.
fn twips_to_pt(s: &str) -> Option<f32> {
    s.trim().parse::<f32>().ok().map(|v| v / 20.0)
}

/// Accumulator for one `<w:sectPr>` while the parser is inside it. Folded into
/// a [`PageGeometry`] + header/footer refs when `</w:sectPr>` closes.
#[derive(Debug, Clone, Default)]
struct SectPrAccum {
    width: Option<f32>,
    height: Option<f32>,
    margin_top: Option<f32>,
    margin_right: Option<f32>,
    margin_bottom: Option<f32>,
    margin_left: Option<f32>,
    header_offset: Option<f32>,
    footer_offset: Option<f32>,
    /// Phase 2 audit — `<w:headerReference>` table keyed by `w:type`.
    /// Replaces the pre-audit single-slot `header_ref: Option<String>`
    /// which silently overwrote on `first` / `even` references.
    header_refs: HeaderFooterRefs,
    footer_refs: HeaderFooterRefs,
    /// `<w:titlePg/>` toggle.
    title_pg: bool,
    /// Audit gap A.H2 — `<w:cols w:num w:space/>` snake-flow descriptor.
    /// `None` ⇒ implicit single column (the writer omits the element so
    /// existing fixtures round-trip byte-identical).
    columns: Option<engine::ColumnSpec>,
    /// Audit gap A.M11 — `<w:pgNumType w:start w:fmt>`. `None` ⇒
    /// inherit doc-wide page numbering.
    page_num: Option<engine::PageNumType>,
    /// Audit gap A.M12 — `<w:type w:val>` section-break discriminator.
    section_type: engine::SectionType,
    /// Issue #80 — `<w:footnotePr>` overrides for this section.
    footnote_props: engine::NoteProps,
    /// Issue #80 — `<w:endnotePr>` overrides.
    endnote_props: engine::NoteProps,
    /// Issue #80 — which `<w:footnotePr>` / `<w:endnotePr>` container is
    /// open, so its leaf children (`<w:pos>`, `<w:numFmt>`, …) route to
    /// the right props. Cleared on the container's end tag.
    note_pr_scope: Option<engine::NoteKind>,
}

/// Per-field accumulator on the [`field_stack`] in
/// [`parse_document_xml`]. One entry per active `<w:fldChar
/// fldCharType="begin">`. Nested fields stack; `</w:fldChar
/// fldCharType="end">` pops the innermost.
#[derive(Debug, Clone, Default)]
struct FieldBuilder {
    /// `<w:instrText>` content — concatenated across as many runs as
    /// the file spreads it over.
    instruction: String,
    /// Byte offset of the first cached display character (the moment
    /// `separate` fires). `None` while still in the begin → separate
    /// phase. Used as the field overlay's `start` on close.
    cached_start: Option<u32>,
}

/// Apply one `<w:fldChar>` event to the field state machine.
///
/// `fldCharType="begin"` pushes a fresh [`FieldBuilder`] onto the stack.
/// `separate` records the cached display text's leading byte offset on
/// the top entry — that's `para_text.len() + run_text.len()` *at the
/// time `separate` fires*, mirroring how every other anchor in this
/// parser maps source-XML position to engine-text position. `end` pops
/// the top entry and (when the field had both a non-empty instruction
/// and a non-empty cached range) emits a [`engine::Field`] overlay.
///
/// Fields with no `separate` (legal — Word omits it for fields that
/// evaluate to nothing, e.g. `<w:fldSimple w:instr="BIBLIOGRAPHY"/>`
/// that hasn't been refreshed) still get parsed: the `cached_start`
/// stays `None`, and `end` discards the entry without emitting an
/// overlay because there is no byte range to anchor.
fn handle_fld_char(
    e: &BytesStart<'_>,
    stack: &mut Vec<FieldBuilder>,
    para_text: &str,
    run_text: &str,
    out_fields: &mut Vec<engine::Field>,
) {
    let kind = attr_val(e, b"w:fldCharType").unwrap_or_default();
    match kind.trim() {
        "begin" => stack.push(FieldBuilder::default()),
        "separate" => {
            if let Some(top) = stack.last_mut() {
                top.cached_start = Some((para_text.len() + run_text.len()) as u32);
            }
        }
        "end" => {
            if let Some(top) = stack.pop()
                && let Some(start) = top.cached_start
            {
                let end = (para_text.len() + run_text.len()) as u32;
                let instruction = top.instruction.trim().to_string();
                if end > start && !instruction.is_empty() {
                    out_fields.push(engine::Field {
                        start,
                        end,
                        instruction,
                    });
                }
            }
        }
        _ => { /* Unknown fldCharType — ignore. */ }
    }
}

/// Map an OOXML `<w:headerReference w:type="…"/>` token to the engine's
/// role enum. Unknown / absent values default to `Default` — matches the
/// spec's behaviour: a `<w:headerReference>` with no `w:type` covers
/// every page where no more-specific variant is selected.
fn parse_header_footer_role(v: Option<&str>) -> HeaderFooterRole {
    match v.map(str::trim) {
        Some("first") => HeaderFooterRole::First,
        Some("even") => HeaderFooterRole::Even,
        _ => HeaderFooterRole::Default,
    }
}

/// Audit gap A.M4 — parse one `<w:pBdr>` per-edge child into the
/// matching `CellBorders` slot. Reuses the table-border parser via
/// `parse_border_stroke` on `<w:top w:val w:sz w:color>`. Unknown
/// edge names are silently ignored — defensive against future spec
/// extensions.
pub(crate) fn apply_pbdr_edge(
    name: &[u8],
    e: &quick_xml::events::BytesStart,
    props: &mut engine::ParaProperties,
) {
    let stroke = parse_border_stroke(e);
    if stroke.is_none() {
        return;
    }
    let borders = props
        .borders
        .get_or_insert_with(engine::CellBorders::default);
    match name {
        b"w:top" => borders.top = stroke,
        b"w:left" => borders.left = stroke,
        b"w:bottom" => borders.bottom = stroke,
        b"w:right" => borders.right = stroke,
        b"w:between" => {
            /* `<w:between>` is the "inside-horizontal" border between
            consecutive same-pBdr paragraphs. The engine has no
            multi-paragraph border collapse yet — store on `inside_h`
            for round-trip; renderer ignores it. */
            borders.inside_h = stroke;
        }
        _ => {}
    }
}

/// Audit gap A.M3 — parse one `<w:tab w:val w:pos/>` child.
/// `w:val` defaults to `left`; `w:pos` is twips (signed integer per
/// spec). Returns `None` for malformed entries (missing pos) so they
/// don't pollute the stop list with NaNs.
pub(crate) fn parse_tab_stop(e: &quick_xml::events::BytesStart) -> Option<engine::TabStop> {
    use crate::schema::ct_rpr::attr_val;
    let pos_twips: i32 = attr_val(e, b"w:pos")?.trim().parse().ok()?;
    let kind = match attr_val(e, b"w:val").as_deref().map(str::trim) {
        Some("center") => engine::TabKind::Center,
        Some("right") | Some("end") => engine::TabKind::Right,
        Some("decimal") => engine::TabKind::Decimal,
        Some("clear") => engine::TabKind::Clear,
        _ => engine::TabKind::Left,
    };
    Some(engine::TabStop {
        position_pt: (pos_twips as f32) / 20.0,
        kind,
    })
}

/// Audit gap A.M4 — parse `<w:top|left|bottom|right|between
/// w:val w:sz w:color w:space/>` into a `BorderStroke`. Mirrors the
/// table-cell border parser semantics; `w:val="none"` returns `None`
/// so the edge stays absent in the engine model.
fn parse_border_stroke(e: &quick_xml::events::BytesStart) -> Option<engine::BorderStroke> {
    use crate::schema::ct_rpr::{attr_val, parse_hex_color};
    let val = attr_val(e, b"w:val")?.trim().to_ascii_lowercase();
    if val == "none" || val == "nil" {
        return None;
    }
    let style = match val.as_str() {
        "single" => engine::BorderStyle::Single,
        "double" => engine::BorderStyle::Double,
        "dotted" => engine::BorderStyle::Dotted,
        "dashed" => engine::BorderStyle::Dashed,
        other => engine::BorderStyle::Other(other.to_string()),
    };
    let size_eighth_pt: u16 = attr_val(e, b"w:sz")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(4);
    let color = attr_val(e, b"w:color").and_then(|v| {
        if v.trim().eq_ignore_ascii_case("auto") {
            None
        } else {
            parse_hex_color(&v)
        }
    });
    Some(engine::BorderStroke {
        style,
        size_eighth_pt,
        color,
    })
}

impl SectPrAccum {
    /// Apply one `<w:sectPr>` child element. Both `Empty(...)` and the
    /// `Start(...)` of `<w:headerReference>...</w:headerReference>`-style tags
    /// route here — every child the parser cares about is leaf-shaped.
    fn apply(&mut self, name: &[u8], e: &BytesStart) {
        /* Issue #80 — `<w:footnotePr>` / `<w:endnotePr>` open a scope;
        their leaf children fold into the scoped props. */
        match name {
            b"w:footnotePr" => {
                self.note_pr_scope = Some(engine::NoteKind::Footnote);
                return;
            }
            b"w:endnotePr" => {
                self.note_pr_scope = Some(engine::NoteKind::Endnote);
                return;
            }
            _ => {}
        }
        if let Some(kind) = self.note_pr_scope {
            let props = match kind {
                engine::NoteKind::Footnote => &mut self.footnote_props,
                engine::NoteKind::Endnote => &mut self.endnote_props,
            };
            if crate::parts::footnotes::apply_note_pr_child(name, e, props) {
                return;
            }
        }
        match name {
            b"w:pgSz" => {
                if let Some(v) = attr_val(e, b"w:w").as_deref().and_then(twips_to_pt) {
                    self.width = Some(v);
                }
                if let Some(v) = attr_val(e, b"w:h").as_deref().and_then(twips_to_pt) {
                    self.height = Some(v);
                }
            }
            b"w:pgMar" => {
                if let Some(v) = attr_val(e, b"w:top").as_deref().and_then(twips_to_pt) {
                    self.margin_top = Some(v);
                }
                if let Some(v) = attr_val(e, b"w:right").as_deref().and_then(twips_to_pt) {
                    self.margin_right = Some(v);
                }
                if let Some(v) = attr_val(e, b"w:bottom").as_deref().and_then(twips_to_pt) {
                    self.margin_bottom = Some(v);
                }
                if let Some(v) = attr_val(e, b"w:left").as_deref().and_then(twips_to_pt) {
                    self.margin_left = Some(v);
                }
                if let Some(v) = attr_val(e, b"w:header").as_deref().and_then(twips_to_pt) {
                    self.header_offset = Some(v);
                }
                if let Some(v) = attr_val(e, b"w:footer").as_deref().and_then(twips_to_pt) {
                    self.footer_offset = Some(v);
                }
            }
            b"w:headerReference" => {
                if let Some(rid) = attr_val(e, b"r:id") {
                    let role = parse_header_footer_role(attr_val(e, b"w:type").as_deref());
                    self.header_refs.set(role, rid);
                }
            }
            b"w:footerReference" => {
                if let Some(rid) = attr_val(e, b"r:id") {
                    let role = parse_header_footer_role(attr_val(e, b"w:type").as_deref());
                    self.footer_refs.set(role, rid);
                }
            }
            b"w:cols" => {
                /* Audit gap A.H2 — `<w:cols w:num="N" w:space="T"/>`.
                `w:num` defaults to 1 (single column); `w:space` defaults
                to 720 twips (½ inch) per OOXML §17.6.4 when absent.
                Per-column `<w:col>` children that would request unequal
                widths are not yet honoured — the sprint ships equal
                partitioning only; uneven widths degrade gracefully. */
                let num = attr_val(e, b"w:num")
                    .and_then(|v| v.trim().parse::<u8>().ok())
                    .unwrap_or(1);
                let space = attr_val(e, b"w:space")
                    .and_then(|v| v.trim().parse::<i32>().ok())
                    .unwrap_or(720);
                if num > 1 {
                    self.columns = Some(engine::ColumnSpec::from_twips(num, space));
                }
            }
            b"w:titlePg" => {
                /* Toggle — bare `<w:titlePg/>` is on, explicit `w:val="0"`
                or `"false"` off. Mirrors the OOXML toggle convention the
                rest of the schema uses. */
                self.title_pg = match attr_val(e, b"w:val").as_deref() {
                    Some(v) => !matches!(v.to_ascii_lowercase().as_str(), "false" | "0" | "off"),
                    None => true,
                };
            }
            /* Audit gap A.M11 — `<w:pgNumType w:start w:fmt/>`. */
            b"w:pgNumType" => {
                let start = attr_val(e, b"w:start").and_then(|v| v.trim().parse::<u32>().ok());
                let format = match attr_val(e, b"w:fmt").as_deref().map(str::trim) {
                    Some("lowerRoman") => engine::PageNumFormat::LowerRoman,
                    Some("upperRoman") => engine::PageNumFormat::UpperRoman,
                    Some("lowerLetter") => engine::PageNumFormat::LowerLetter,
                    Some("upperLetter") => engine::PageNumFormat::UpperLetter,
                    _ => engine::PageNumFormat::Decimal,
                };
                self.page_num = Some(engine::PageNumType { start, format });
            }
            /* Audit gap A.M12 — `<w:sectPr><w:type w:val>`. */
            b"w:type" => {
                self.section_type = match attr_val(e, b"w:val").as_deref().map(str::trim) {
                    Some("continuous") => engine::SectionType::Continuous,
                    Some("evenPage") => engine::SectionType::EvenPage,
                    Some("oddPage") => engine::SectionType::OddPage,
                    _ => engine::SectionType::NextPage,
                };
            }
            _ => {}
        }
    }

    /// Bake into the engine-facing `PageGeometry`. Missing fields fall back
    /// to the A4 defaults so a `<w:sectPr/>` with only a header reference
    /// still produces a usable section.
    fn into_geometry(self) -> PageGeometry {
        let d = PageGeometry::a4();
        PageGeometry {
            width: self.width.unwrap_or(d.width),
            height: self.height.unwrap_or(d.height),
            margin_top: self.margin_top.unwrap_or(d.margin_top),
            margin_right: self.margin_right.unwrap_or(d.margin_right),
            margin_bottom: self.margin_bottom.unwrap_or(d.margin_bottom),
            margin_left: self.margin_left.unwrap_or(d.margin_left),
            header_offset: self.header_offset.unwrap_or(d.header_offset),
            footer_offset: self.footer_offset.unwrap_or(d.footer_offset),
        }
    }
}

/// Parse `word/document.xml` into paragraphs.
///
/// `resolver` folds the OOXML cascade so each paragraph / run hits the
/// engine as fully-resolved flat properties. Non-fatal degradations (the
/// issue #111 table-nesting cap) are discarded here;
/// [`parse_document_xml_with_warnings`] surfaces them.
pub fn parse_document_xml(
    xml: &[u8],
    resolver: &StyleResolver<'_>,
) -> Result<DocumentTree, DocxError> {
    let mut warnings = Vec::new();
    parse_document_xml_with_warnings(xml, resolver, &mut warnings)
}

/// Strip a leading UTF-8 byte-order mark (`EF BB BF`).
///
/// Issue #110 — quick-xml silently drops the BOM from its input but does
/// **not** count it in `buffer_position()`, so every offset the reader
/// reports is relative to the BOM-less stream. Every passthrough /
/// grab-bag capture in this crate indexes the raw part with those offsets;
/// the part has to be BOM-stripped first so both live in the same byte
/// space. docx4j and Apache POI write BOM-prefixed parts; before this,
/// each `<w:p>` was captured three bytes early (`dy><w:p>…</w` instead of
/// `<w:p>…</w:p>`) and a zero-edit resave was unparseable.
pub(crate) fn strip_utf8_bom(xml: &[u8]) -> &[u8] {
    xml.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(xml)
}

/// [`parse_document_xml`], appending every non-fatal reader diagnostic to
/// `warnings` (see [`DocxWarning`]).
pub fn parse_document_xml_with_warnings(
    xml: &[u8],
    resolver: &StyleResolver<'_>,
    warnings: &mut Vec<DocxWarning>,
) -> Result<DocumentTree, DocxError> {
    /* Issue #110 — see `strip_utf8_bom`: `reader.buffer_position()` and
    every `xml[..]` slice below must share one byte space. */
    let xml = strip_utf8_bom(xml);
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);

    /* Top-level blocks accumulator. Phase 5 PR 1 emits `Block::Paragraph`
    on `</w:p>` and `Block::Table` on `</w:tbl>` (with `rows: vec![]`
    and `source_xml` carrying the raw bytes for the passthrough writer
    — PR 2 parses rows + cells). */
    let mut out_blocks: Vec<Block> = Vec::new();
    let mut para_text = String::new();
    let mut spans: Vec<StyleRun> = Vec::new();
    /* Phase 7 — inline images and hyperlinks accumulators (per-paragraph,
    drained on `</w:p>`). `para_inline_objects` collects each
    `<w:drawing>` we resolve to a picture; the parser injects a
    U+FFFC OBJECT REPLACEMENT CHARACTER into `para_text` at the same
    byte offset so layout sees the placeholder. `para_hyperlinks`
    collects every `<w:hyperlink>`'s byte range plus the relationship
    id; the archive reader maps those rIds to URLs in a second pass. */
    let mut para_inline_objects: Vec<engine::InlineObject> = Vec::new();
    let mut para_hyperlinks: Vec<engine::Hyperlink> = Vec::new();
    let mut para_revisions: Vec<engine::Revision> = Vec::new();
    let mut para_fields: Vec<engine::Field> = Vec::new();

    /* Phase 2 audit (gap D.1) — complex-field state machine. A field
    is the triplet `<w:fldChar fldCharType="begin">` ...
    `<w:instrText>…</w:instrText>` ... `<w:fldChar fldCharType="separate">`
    ... cached `<w:r><w:t>…</w:t></w:r>` runs ... `<w:fldChar
    fldCharType="end">`. The four moving parts (begin, instrText
    accumulator, separate marker, end) can each sit in different `<w:r>`
    elements so the state lives outside the per-run loop. Stack-shaped
    because OOXML permits nested fields (e.g. an `IF` that evaluates
    another field as one of its arguments).

    Per stack entry: the leading byte offset where the cached display
    text begins (set when `separate` arrives) and the accumulating
    instruction string. `cached_start: None` before `separate`. */
    let mut field_stack: Vec<FieldBuilder> = Vec::new();
    /* Issue #43 — `<w:fldSimple w:instr="…">cached runs</w:fldSimple>`,
    the compact single-element field form Word emits for simple PAGE /
    DATE fields. Stack of (instruction, cached-start); the close tag
    seals the byte range exactly like fldChar's begin/separate/end. */
    let mut fld_simple_stack: Vec<(String, u32)> = Vec::new();
    let mut in_instr_text = false;

    /* Phase 8b — tracked-change wrapper state. A `<w:ins>` or `<w:del>`
    can wrap several `<w:r>` elements; we capture the wrapper's
    author / date attrs on open and emit a `Revision` on close
    covering the byte range produced inside. Stack-shaped so a
    nested ins/del (rare but legal — an insertion inside a deletion)
    still resolves correctly. */
    let mut revision_stack: Vec<(engine::RevisionKind, String, String, Option<u32>, u32)> =
        Vec::new();
    /* Phase 8b — `<w:delText>` is the OOXML synonym for `<w:t>` inside a
    `<w:del>` wrapper. The parser collapses both into `run_text` so
    deleted text rides alongside live content; the `Revision` overlay
    flags the byte range for the strikethrough renderer. */
    let mut in_del_text_elt = false;

    /* Phase 8a — comment ranges. `<w:commentRangeStart w:id="N"/>` opens
    a range; `<w:commentRangeEnd w:id="N"/>` closes it. Each may live in
    a different paragraph, so we capture the `(block_idx_at_open,
    open_paragraph_byte_offset)` snapshot and consume it on the matching
    end. `open_ranges` maps comment id → (start_block_idx, start_offset).
    `out_comment_ranges` is the document-wide table the parser hands the
    engine. */
    let mut open_comment_ranges: std::collections::HashMap<u32, (u32, u32)> =
        std::collections::HashMap::new();
    let mut out_comment_ranges: Vec<engine::CommentRange> = Vec::new();

    /* Table state. `in_tbl` is a depth counter so nested tables (inside
    cells) don't trigger early `Block::Table` emission — only the
    outermost `</w:tbl>` flushes. `tbl_start_byte` captures the
    leading `<` byte offset of the outermost `<w:tbl>` for source
    capture. While `in_tbl > 0`, every `<w:p>` / `<w:r>` / etc. is
    *ignored* at the block level — the parser reads them but doesn't
    accumulate them into the top-level block list. The bytes are
    preserved verbatim in the captured `source_xml`. */
    let mut in_tbl: u32 = 0;
    let mut tbl_start_byte: Option<usize> = None;

    /* Per-paragraph parser state. */
    let mut p_style_id: Option<String> = None;
    let mut direct_ppr = ParaProperties::default();
    let mut pmark_rpr = SpanStyle::default();
    /* Phase 4 — `<w:numPr>/<w:numId>` + `<w:ilvl>` accumulators. We don't
    inherit either field from a paragraph style here; that's a separate
    cascade source Phase 4 ships without modelling. */
    let mut list_num_id: Option<u32> = None;
    let mut list_ilvl: Option<u8> = None;
    let mut in_num_pr = false;
    /* Audit gap A.M3 / A.M4 — `<w:pBdr>` / `<w:tabs>` depth tracking.
    The container elements wrap per-edge / per-stop empty children that
    fold into `direct_ppr`. */
    let mut in_pbdr = false;
    let mut in_tabs = false;

    /* Phase 7 — `<w:drawing>` accumulators. A drawing element wraps an
    inline image (`<wp:inline>`) or a floating image (`<wp:anchor>`,
    deferred). Inside, `<wp:extent cx=".." cy=".."/>` carries EMU
    dimensions and `<a:blip r:embed="rId.."/>` carries the image's
    relationship id. When `</w:drawing>` closes, if we were inside a
    `<wp:inline>` AND have both an rId and extents, push U+FFFC into
    the current run text and queue an `InlineObject`. */
    let mut in_drawing = false;
    let mut in_wp_inline = false;
    let mut cur_drawing_rel_id: Option<String> = None;
    let mut cur_drawing_cx: Option<i64> = None;
    let mut cur_drawing_cy: Option<i64> = None;
    /* Issue #69 — `<wp:anchor>` (floating picture) accumulators. The
    anchor's attributes seed `cur_anchor` on the start tag; `<wp:positionH>`
    / `<wp:positionV>` open an axis (`anchor_axis`) whose `<wp:posOffset>` /
    `<wp:align>` / `<wp14:pctPos*Offset>` child collects text into
    `anchor_offset` until its end tag. The wrap element and `<wp:docPr>`
    are captured VERBATIM (the writer cannot regenerate a wrap polygon or
    a docPr's descr / hyperlink from typed fields). On `</w:drawing>` an
    anchored picture with a blip + extents becomes an `InlineObject` whose
    `anchor` is `Some` — same U+FFFC sentinel as an inline picture, so
    every offset-shifting edit path already carries it. A text box or
    shape anchor (no `<a:blip>`) is dropped exactly as before (#119). */
    let mut in_wp_anchor = false;
    let mut cur_anchor: Option<Box<engine::FloatAnchor>> = None;
    let mut anchor_axis: Option<AnchorAxis> = None;
    let mut anchor_offset: Option<(AnchorOffsetKind, String)> = None;

    /* Phase 7 — `<w:hyperlink>` overlays. Word lays paragraph text out
    paragraph-flat with `<w:hyperlink>` spanning a contiguous slice of
    `<w:r>` elements that all share the link target. Capture the rId
    when the element opens; mark the byte range covered when it
    closes. `target` is the rId at this stage — the archive resolver
    swaps it to a URL via the rels map in a second pass. */
    let mut hyperlink_stack: Vec<(String, u32)> = Vec::new();

    /* Phase 6 — `<w:sectPr>` accumulators. A sectPr can live in two places:
    inside a paragraph's `<w:pPr>` (ends a section *at* that paragraph,
    inclusive — every paragraph since the previous sectPr belongs to it) or
    directly in `<w:body>` after the final paragraph (the body-level sectPr
    covers everything left). Both flow through the same accumulator.

    - `in_sect_pr` — depth flag (a sectPr is leaf-shaped at this level).
    - `cur_sect` — the accumulator being filled.
    - `pending_paragraph_sect` — set when `</w:sectPr>` closes inside a
      `<w:pPr>`; consumed on the matching `</w:p>` to emit a `Section`
      `[sect_start..=this_paragraph_idx]`.
    - `out_sections` — the section table the parser hands to the engine.
    - `sect_start_block` — first top-level block of the next section. */
    let mut in_sect_pr = false;
    let mut cur_sect = SectPrAccum::default();
    let mut pending_paragraph_sect: Option<SectPrAccum> = None;
    let mut out_sections: Vec<Section> = Vec::new();
    let mut sect_start_block: u32 = 0;

    /* Per-run parser state. */
    let mut in_run = false;
    let mut in_rpr = false;
    let mut in_ppr = false;
    let mut in_text_elt = false;
    let mut r_style_id: Option<String> = None;
    let mut direct_rpr = SpanStyle::default();
    let mut run_text = String::new();

    /* Source-byte capture for the passthrough optimisation. `prev_pos` is
    the byte offset of the just-yielded event's end — equivalently the
    start of the next event we are about to read. */
    let mut prev_pos: usize = 0;
    let mut p_start_byte: Option<usize> = None;

    /* Issue #84 — namespace prefixes the part's root element binds. Grab-bag
    fragments in a foreign namespace (`w14:`, `mc:`, …) re-bind their
    prefixes from here so they stay well-formed under the writer's
    synthesized root. */
    let mut ns = NamespaceScope::default();
    let mut root_seen = false;

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                let name = e.name();
                if !root_seen {
                    root_seen = true;
                    ns = NamespaceScope::from_root(&e);
                }
                /* Phase 5 PR 1 — outermost `<w:tbl>` opens. Capture leading
                byte offset for the source-byte passthrough; ignore every
                child event (`<w:p>` / `<w:r>` etc. inside cells) until the
                matching `</w:tbl>` brings the depth back to 0. Nested
                tables inside cells just bump the counter further. */
                if name.as_ref() == b"w:tbl" {
                    if in_tbl == 0 {
                        tbl_start_byte = Some(prev_pos);
                    }
                    in_tbl += 1;
                    prev_pos = reader.buffer_position() as usize;
                    buf.clear();
                    continue;
                }
                if in_tbl > 0 {
                    /* Inside a table — skip the entire body. We still need
                    to advance `prev_pos` below. */
                    prev_pos = reader.buffer_position() as usize;
                    buf.clear();
                    continue;
                }
                /* Issue #119 rider (shipped with #69) — drawing sub-stories
                are NOT body content. A `<w:p>` inside `<w:txbxContent>`
                (a DrawingML text box), `<w:pict>` (VML: legacy pictures,
                `<v:textbox>`) or `<mc:Fallback>` (the VML duplicate of an
                `mc:AlternateContent` choice) used to be hoisted into the
                body as a paragraph of its own, and its `</w:p>` ended the
                ENCLOSING paragraph early — losing that paragraph's
                `p_start_byte`, so the passthrough could not fire and the
                regenerated paragraph dropped the whole drawing. Consume the
                subtree instead: the enclosing paragraph keeps its byte
                capture (a zero-edit resave stays byte-identical), and a
                `<wp:anchor>` / `<wp:inline>` picture around it still parses
                — only its inner story is skipped. Modeling text boxes is
                issue #83. */
                match name.as_ref() {
                    b"w:txbxContent" | b"w:pict" | b"mc:Fallback" => {
                        let _ = capture_subtree(xml, prev_pos, &mut reader, &e)?;
                        prev_pos = reader.buffer_position() as usize;
                        buf.clear();
                        continue;
                    }
                    _ => {}
                }
                match name.as_ref() {
                    b"w:p" => {
                        /* Capture the byte offset of the `<w:p` opening
                        delimiter — that's `prev_pos`, the position
                        *before* this event was read (= where the `<` of
                        `<w:p>` lives in the source). */
                        p_start_byte = Some(prev_pos);
                        p_style_id = None;
                        direct_ppr = ParaProperties::default();
                        pmark_rpr = SpanStyle::default();
                    }
                    b"w:r" => {
                        in_run = true;
                        r_style_id = None;
                        direct_rpr = SpanStyle::default();
                        run_text.clear();
                    }
                    b"w:rPr" if in_ppr && !in_run => {
                        /* Issue #84 — paragraph-mark run properties
                        (`<w:pPr>/<w:rPr>`). The writer never regenerates
                        this element, so the WHOLE subtree rides the
                        paragraph's grab bag verbatim; its modeled children
                        still seed the run baseline (`pmark_rpr`) exactly
                        as before via `fold_rpr_fragment`, which also
                        stops a nested `<w:rPrChange>/<w:rPr>` history from
                        overriding the live formatting. */
                        if let Some(frag) = capture_subtree(xml, prev_pos, &mut reader, &e)? {
                            fold_rpr_fragment(&frag, &mut pmark_rpr);
                            stash(&mut direct_ppr.grab_bag, frag, &ns);
                        }
                    }
                    b"w:rPr" => in_rpr = true,
                    /* A `<w:pPr>` only counts when it's the paragraph's own
                    properties — not a nested element under a `<w:r>`. */
                    b"w:pPr" if !in_run => in_ppr = true,
                    b"w:numPr" if in_ppr => in_num_pr = true,
                    /* Audit gap A.M4 — `<w:pBdr>` is a container element
                    holding per-edge `<w:top>` / `<w:left>` / `<w:bottom>`
                    / `<w:right>` empty children. Toggle the depth
                    tracker so the per-edge handlers below know to fold
                    into `direct_ppr.borders` instead of cell borders. */
                    b"w:pBdr" if in_ppr => in_pbdr = true,
                    /* Audit gap A.M3 — `<w:tabs>` container of `<w:tab/>`
                    children. The child handlers parse position + kind
                    and push onto `direct_ppr.tab_stops`. */
                    b"w:tabs" if in_ppr => in_tabs = true,
                    b"w:sectPr" => {
                        /* Phase 6 — open a fresh accumulator. Inline (inside
                        a `<w:pPr>`) and body-level both route here. */
                        in_sect_pr = true;
                        cur_sect = SectPrAccum::default();
                    }
                    b"w:drawing" => {
                        in_drawing = true;
                        in_wp_inline = false;
                        cur_drawing_rel_id = None;
                        cur_drawing_cx = None;
                        cur_drawing_cy = None;
                    }
                    b"wp:inline" if in_drawing => {
                        in_wp_inline = true;
                    }
                    b"wp:anchor" if in_drawing => {
                        in_wp_anchor = true;
                        cur_anchor = Some(Box::new(anchor_from_start_tag(&e)));
                    }
                    b"wp:positionH" if in_wp_anchor => {
                        anchor_axis = Some(AnchorAxis::H);
                        if let Some(a) = cur_anchor.as_mut() {
                            a.position_h.relative_from =
                                h_relative_from(attr_val(&e, b"relativeFrom").as_deref());
                        }
                    }
                    b"wp:positionV" if in_wp_anchor => {
                        anchor_axis = Some(AnchorAxis::V);
                        if let Some(a) = cur_anchor.as_mut() {
                            a.position_v.relative_from =
                                v_relative_from(attr_val(&e, b"relativeFrom").as_deref());
                        }
                    }
                    b"wp:posOffset" if anchor_axis.is_some() => {
                        anchor_offset = Some((AnchorOffsetKind::PosOffset, String::new()));
                    }
                    b"wp:align" if anchor_axis.is_some() => {
                        anchor_offset = Some((AnchorOffsetKind::Align, String::new()));
                    }
                    b"wp14:pctPosHOffset" | b"wp14:pctPosVOffset" if anchor_axis.is_some() => {
                        anchor_offset = Some((AnchorOffsetKind::Percent, String::new()));
                    }
                    n if in_wp_anchor && is_wrap_element(n) => {
                        /* Wrap element with children (`<wp:wrapTight>` +
                        `<wp:wrapPolygon>`, or `<wp:wrapSquare>` carrying
                        an `<wp:effectExtent>`): consume the subtree and
                        keep its bytes. */
                        let frag = capture_subtree(xml, prev_pos, &mut reader, &e)?;
                        if let Some(a) = cur_anchor.as_mut() {
                            a.wrap = wrap_kind_of(n).unwrap_or_default();
                            a.wrap_xml = frag.and_then(|f| String::from_utf8(f).ok());
                        }
                    }
                    b"wp:docPr" if in_wp_anchor => {
                        let frag = capture_subtree(xml, prev_pos, &mut reader, &e)?;
                        if let Some(a) = cur_anchor.as_mut() {
                            a.doc_pr_xml = frag.and_then(|f| String::from_utf8(f).ok());
                        }
                    }
                    b"wp:extent" if in_drawing => {
                        cur_drawing_cx = attr_val(&e, b"cx").and_then(|v| v.parse().ok());
                        cur_drawing_cy = attr_val(&e, b"cy").and_then(|v| v.parse().ok());
                    }
                    b"a:blip" if in_drawing => {
                        cur_drawing_rel_id = attr_val(&e, b"r:embed");
                    }
                    b"w:hyperlink" => {
                        if let Some(rid) = attr_val(&e, b"r:id") {
                            let start = (para_text.len() + run_text.len()) as u32;
                            hyperlink_stack.push((rid, start));
                        }
                    }
                    b"w:t" => in_text_elt = true,
                    b"w:delText" => in_del_text_elt = true,
                    b"w:instrText" => in_instr_text = true,
                    b"w:fldChar" => {
                        /* fldChar drives the field state machine. The
                        attribute value lives on the start tag's `w:fldCharType`
                        attribute. `Start(...)` and `Empty(...)` both end up
                        here — match `Empty` below as well for completeness. */
                        handle_fld_char(
                            &e,
                            &mut field_stack,
                            &para_text,
                            &run_text,
                            &mut para_fields,
                        );
                    }
                    b"w:fldSimple" => {
                        let instr = attr_val(&e, b"w:instr").unwrap_or_default();
                        let start = (para_text.len() + run_text.len()) as u32;
                        fld_simple_stack.push((instr, start));
                    }
                    b"w:ins" | b"w:del" => {
                        let kind = if name.as_ref() == b"w:ins" {
                            engine::RevisionKind::Insert
                        } else {
                            engine::RevisionKind::Delete
                        };
                        let author = attr_val(&e, b"w:author").unwrap_or_default();
                        let date = attr_val(&e, b"w:date").unwrap_or_default();
                        let id = attr_val(&e, b"w:id").and_then(|v| v.trim().parse().ok());
                        let start = (para_text.len() + run_text.len()) as u32;
                        revision_stack.push((kind, author, date, id, start));
                    }
                    b"w:pStyle" if in_ppr => {
                        p_style_id = attr_val(&e, b"w:val");
                    }
                    b"w:rStyle" if in_run && in_rpr => {
                        r_style_id = attr_val(&e, b"w:val");
                    }
                    b"w:numId" if in_num_pr => {
                        list_num_id = attr_val(&e, b"w:val").and_then(|v| v.parse().ok());
                    }
                    b"w:ilvl" if in_num_pr => {
                        list_ilvl = attr_val(&e, b"w:val").and_then(|v| v.parse().ok());
                    }
                    n if in_sect_pr => cur_sect.apply(n, &e),
                    n if in_run && in_rpr && !rpr_child_is_modeled(n) => {
                        /* Issue #84 — unmodeled `<w:rPr>` container child
                        (`<w:rPrChange>`, `<w:bdr>` with content, `mc:`
                        wrappers, …): the whole subtree goes into the run's
                        grab bag and the parser skips it, so nothing inside
                        can masquerade as live run formatting. */
                        if let Some(frag) = capture_subtree(xml, prev_pos, &mut reader, &e)? {
                            stash(&mut direct_rpr.grab_bag, frag, &ns);
                        }
                    }
                    n if in_run && in_rpr => apply_rpr(n, &e, &mut direct_rpr),
                    n if in_ppr
                        && !in_rpr
                        && !in_num_pr
                        && !in_pbdr
                        && !in_tabs
                        && !ppr_child_is_modeled(n) =>
                    {
                        /* Issue #84 — unmodeled `<w:pPr>` container child
                        (`<w:pPrChange>`, `<w:framePr>` with content, …). */
                        if let Some(frag) = capture_subtree(xml, prev_pos, &mut reader, &e)? {
                            stash(&mut direct_ppr.grab_bag, frag, &ns);
                        }
                    }
                    n if in_ppr && !in_rpr && !in_num_pr => {
                        apply_ppr(n, &e, &mut direct_ppr);
                    }
                    _ => {}
                }
            }
            Event::Empty(e) => {
                let name = e.name();
                if in_tbl > 0 {
                    prev_pos = reader.buffer_position() as usize;
                    buf.clear();
                    continue;
                }
                match name.as_ref() {
                    b"w:pStyle" if in_ppr => {
                        p_style_id = attr_val(&e, b"w:val");
                    }
                    b"w:rStyle" if in_run && in_rpr => {
                        r_style_id = attr_val(&e, b"w:val");
                    }
                    b"w:numId" if in_num_pr => {
                        list_num_id = attr_val(&e, b"w:val").and_then(|v| v.parse().ok());
                    }
                    b"w:ilvl" if in_num_pr => {
                        list_ilvl = attr_val(&e, b"w:val").and_then(|v| v.parse().ok());
                    }
                    b"wp:extent" if in_drawing => {
                        cur_drawing_cx = attr_val(&e, b"cx").and_then(|v| v.parse().ok());
                        cur_drawing_cy = attr_val(&e, b"cy").and_then(|v| v.parse().ok());
                    }
                    b"a:blip" if in_drawing => {
                        cur_drawing_rel_id = attr_val(&e, b"r:embed");
                    }
                    b"wp:simplePos" if in_wp_anchor => {
                        if let Some(a) = cur_anchor.as_mut() {
                            a.simple_pos_x_emu =
                                attr_val(&e, b"x").and_then(|v| v.parse().ok()).unwrap_or(0);
                            a.simple_pos_y_emu =
                                attr_val(&e, b"y").and_then(|v| v.parse().ok()).unwrap_or(0);
                        }
                    }
                    n if in_wp_anchor && is_wrap_element(n) => {
                        /* Empty wrap element (`<wp:wrapNone/>`,
                        `<wp:wrapSquare wrapText="bothSides"/>`, …). */
                        let end = reader.buffer_position() as usize;
                        if let Some(a) = cur_anchor.as_mut() {
                            a.wrap = wrap_kind_of(n).unwrap_or_default();
                            a.wrap_xml = slice_fragment(xml, prev_pos, end)
                                .and_then(|f| String::from_utf8(f).ok());
                        }
                    }
                    b"wp:docPr" if in_wp_anchor => {
                        let end = reader.buffer_position() as usize;
                        if let Some(a) = cur_anchor.as_mut() {
                            a.doc_pr_xml = slice_fragment(xml, prev_pos, end)
                                .and_then(|f| String::from_utf8(f).ok());
                        }
                    }
                    b"w:tab" if in_run => {
                        /* Audit gap A.M5 — `<w:tab/>` inside a `<w:r>`.
                        Stored as the literal U+0009 TAB byte so the
                        writer can recognise the anchor and emit the
                        structural element back. Geometric tab-stop
                        layout (audit A.M3) is deferred to a later
                        sprint; the shaper substitutes U+0009 → U+0020
                        at shape time so the user sees a single space
                        instead of a `.notdef` tofu glyph. */
                        run_text.push('\u{0009}');
                    }
                    b"w:br" if in_run => {
                        /* Phase 2 audit (gap A.12) — `<w:br/>` inside a
                        `<w:r>`. Maps to the Unicode mandatory-break
                        character that matches the requested break type:
                        U+2028 LINE SEPARATOR for line/textWrapping
                        breaks (UAX-14 BK class — ICU LineSegmenter
                        produces a hard break opportunity) and U+000C
                        FORM FEED for page breaks (same UAX-14 BK
                        class; the paginator inspects the cluster to
                        force a page flush). Column breaks fall back
                        to U+2028 — column layout is deferred (audit
                        C.4) so a column break degrades visually to a
                        line break. */
                        let kind = attr_val(&e, b"w:type").unwrap_or_default();
                        let ch = match kind.trim() {
                            "page" => '\u{000C}',
                            _ => '\u{2028}',
                        };
                        run_text.push(ch);
                    }
                    b"w:footnoteReference" | b"w:endnoteReference" => {
                        /* Phase 8a / issue #80 — inject U+FFFC at the
                        run's current byte offset and queue the reference
                        inline object. The displayed number is derived at
                        layout time in document order
                        (`DocumentTree::note_markers`); only the OOXML
                        `w:id` and the custom-mark flag are stored. */
                        if let Some(id) = attr_val(&e, b"w:id").and_then(|v| v.trim().parse().ok())
                        {
                            let custom_mark_follows = attr_val(&e, b"w:customMarkFollows")
                                .is_some_and(|v| {
                                    !matches!(
                                        v.trim().to_ascii_lowercase().as_str(),
                                        "false" | "0" | "off"
                                    )
                                });
                            let at = (para_text.len() + run_text.len()) as u32;
                            run_text.push('\u{FFFC}');
                            let kind = if name.as_ref() == b"w:footnoteReference" {
                                engine::InlineKind::FootnoteRef {
                                    id,
                                    custom_mark_follows,
                                }
                            } else {
                                engine::InlineKind::EndnoteRef {
                                    id,
                                    custom_mark_follows,
                                }
                            };
                            para_inline_objects.push(engine::InlineObject {
                                at,
                                kind,
                                anchor: None,
                            });
                        }
                    }
                    b"w:footnoteRef" | b"w:endnoteRef" => {
                        /* Issue #80 — the self-mark at the head of a note
                        body (only ever seen while parsing a note story).
                        Anchored like a reference so the body paints its
                        own number and a regenerated body re-emits the
                        element. */
                        let at = (para_text.len() + run_text.len()) as u32;
                        run_text.push('\u{FFFC}');
                        let kind = if name.as_ref() == b"w:footnoteRef" {
                            engine::NoteKind::Footnote
                        } else {
                            engine::NoteKind::Endnote
                        };
                        para_inline_objects.push(engine::InlineObject {
                            at,
                            kind: engine::InlineKind::NoteSelfRef { kind },
                            anchor: None,
                        });
                    }
                    b"w:commentRangeStart" => {
                        if let Some(id) = attr_val(&e, b"w:id").and_then(|v| v.parse().ok()) {
                            let block_idx = out_blocks.len() as u32;
                            let off = (para_text.len() + run_text.len()) as u32;
                            open_comment_ranges.insert(id, (block_idx, off));
                        }
                    }
                    b"w:commentRangeEnd" => {
                        if let Some(id) = attr_val(&e, b"w:id").and_then(|v| v.parse().ok())
                            && let Some((start_block, start_off)) = open_comment_ranges.remove(&id)
                        {
                            let end_block = out_blocks.len() as u32;
                            let end_off = (para_text.len() + run_text.len()) as u32;
                            out_comment_ranges.push(engine::CommentRange {
                                id,
                                start: engine::LogicalPos {
                                    path: engine::BlockPath::top(start_block),
                                    offset: start_off,
                                },
                                end: engine::LogicalPos {
                                    path: engine::BlockPath::top(end_block),
                                    offset: end_off,
                                },
                            });
                        }
                    }
                    b"w:commentReference" => {
                        /* The reference marker itself is invisible in the
                        canvas (the sidebar UI shows the comment); the
                        passthrough writer round-trips the markup byte-
                        identical. Nothing to record here. */
                    }
                    b"w:fldChar" => {
                        handle_fld_char(
                            &e,
                            &mut field_stack,
                            &para_text,
                            &run_text,
                            &mut para_fields,
                        );
                    }
                    b"w:rPr" if in_ppr && !in_run => {
                        /* Issue #84 — an empty paragraph-mark `<w:rPr/>`
                        still rides the bag (byte-stable regeneration). */
                        let end = reader.buffer_position() as usize;
                        if let Some(frag) = slice_fragment(xml, prev_pos, end) {
                            stash(&mut direct_ppr.grab_bag, frag, &ns);
                        }
                    }
                    n if in_sect_pr => cur_sect.apply(n, &e),
                    n if in_run && in_rpr && !rpr_child_is_modeled(n) => {
                        /* Issue #84 — unmodeled `<w:rPr>` leaf child
                        (`<w:lang>`, `<w:fitText>`, `<w:eastAsianLayout>`,
                        `<w14:glow>`, …) → the run's grab bag, verbatim. */
                        let end = reader.buffer_position() as usize;
                        if let Some(frag) = slice_fragment(xml, prev_pos, end) {
                            stash(&mut direct_rpr.grab_bag, frag, &ns);
                        }
                    }
                    n if in_run && in_rpr => apply_rpr(n, &e, &mut direct_rpr),
                    /* Audit gap A.M4 — `<w:pBdr>` per-edge children. */
                    n if in_ppr && in_pbdr => apply_pbdr_edge(n, &e, &mut direct_ppr),
                    /* Audit gap A.M3 — `<w:tabs>` per-stop children. */
                    n if in_ppr && in_tabs && n == b"w:tab" => {
                        if let Some(stop) = parse_tab_stop(&e) {
                            direct_ppr.tab_stops.push(stop);
                        }
                    }
                    n if in_ppr
                        && !in_rpr
                        && !in_num_pr
                        && !in_pbdr
                        && !in_tabs
                        && !ppr_child_is_modeled(n) =>
                    {
                        /* Issue #84 — unmodeled `<w:pPr>` leaf child
                        (`<w:framePr>`, `<w:cnfStyle>`, `<w:widowControl>`,
                        `<w:outlineLvl>`, …) → the paragraph's grab bag. */
                        let end = reader.buffer_position() as usize;
                        if let Some(frag) = slice_fragment(xml, prev_pos, end) {
                            stash(&mut direct_ppr.grab_bag, frag, &ns);
                        }
                    }
                    n if in_ppr && !in_rpr && !in_num_pr && !in_pbdr && !in_tabs => {
                        apply_ppr(n, &e, &mut direct_ppr);
                    }
                    _ => {}
                }
            }
            Event::Text(t) if (in_text_elt || in_del_text_elt) && in_tbl == 0 => {
                run_text.push_str(&t.unescape()?);
            }
            Event::Text(t) if anchor_offset.is_some() && in_tbl == 0 => {
                /* Issue #69 — `<wp:posOffset>` / `<wp:align>` / wp14
                percentage text of the open positioning axis. */
                if let Some((_, buf_s)) = anchor_offset.as_mut() {
                    buf_s.push_str(&t.unescape()?);
                }
            }
            Event::Text(t) if in_instr_text && in_tbl == 0 => {
                /* `<w:instrText>` content accumulates onto the innermost
                open field's instruction buffer. The text may straddle
                multiple `<w:r><w:instrText>` runs — accumulating across
                runs is exactly the point of the stack-based state
                machine, otherwise a `PAGE \* MERGEFORMAT` split across
                `PAGE` and ` \* MERGEFORMAT` runs would lose its switch. */
                if let Some(top) = field_stack.last_mut() {
                    top.instruction.push_str(&t.unescape()?);
                }
            }
            Event::End(e) => {
                let name = e.name();
                /* Match the outermost `</w:tbl>` first — bring `in_tbl`
                back to 0 and flush a `Block::Table` with captured source
                bytes. Nested table closes just decrement the depth. */
                if name.as_ref() == b"w:tbl" {
                    if in_tbl > 0 {
                        in_tbl -= 1;
                        if in_tbl == 0 {
                            let tbl_end_byte = reader.buffer_position() as usize;
                            let source_xml = tbl_start_byte
                                .take()
                                .and_then(|s| slice_element(xml, s, tbl_end_byte, b"w:tbl"));
                            /* Phase 5 PR 2 — full row/cell parse via
                            `parts::table::parse_table_bytes`. Source bytes
                            still ride the passthrough so the writer is
                            byte-stable for unedited tables. */
                            /* Audit gap A.M18 — thread the resolver into
                            cell parsing so cell paragraphs pick up the
                            doc-defaults + pStyle cascade. Without this,
                            cells silently drop list bindings + paragraph
                            styles, breaking visual fidelity on numbered
                            tables. */
                            let (grid, props, rows) = source_xml
                                .as_deref()
                                .map(|b| {
                                    parse_table_bytes_with_warnings(b, resolver, &ns, warnings)
                                        .unwrap_or_default()
                                })
                                .unwrap_or_default();
                            out_blocks.push(Block::Table(Table {
                                grid,
                                props,
                                rows,
                                dirty: false,
                                source_xml,
                            }));
                        }
                    }
                    prev_pos = reader.buffer_position() as usize;
                    buf.clear();
                    continue;
                }
                if in_tbl > 0 {
                    prev_pos = reader.buffer_position() as usize;
                    buf.clear();
                    continue;
                }
                match name.as_ref() {
                    b"w:t" => in_text_elt = false,
                    b"w:delText" => in_del_text_elt = false,
                    b"w:instrText" => in_instr_text = false,
                    b"w:ins" | b"w:del" => {
                        if let Some((kind, author, date, id, start)) = revision_stack.pop() {
                            let end = (para_text.len() + run_text.len()) as u32;
                            if end > start {
                                para_revisions.push(engine::Revision {
                                    start,
                                    end,
                                    kind,
                                    author,
                                    date,
                                    id,
                                    prev_attrs: None,
                                });
                            }
                        }
                    }
                    b"w:rPr" => in_rpr = false,
                    b"w:pPr" => in_ppr = false,
                    b"w:numPr" => in_num_pr = false,
                    b"w:pBdr" => in_pbdr = false,
                    b"w:tabs" => in_tabs = false,
                    b"wp:posOffset"
                    | b"wp:align"
                    | b"wp14:pctPosHOffset"
                    | b"wp14:pctPosVOffset" => {
                        /* Issue #69 — seal the offset onto the open axis. */
                        if let (Some((kind, text)), Some(axis), Some(a)) =
                            (anchor_offset.take(), anchor_axis, cur_anchor.as_mut())
                            && let Some(offset) = parse_offset(kind, &text)
                        {
                            match axis {
                                AnchorAxis::H => a.position_h.offset = offset,
                                AnchorAxis::V => a.position_v.offset = offset,
                            }
                        }
                    }
                    b"wp:positionH" | b"wp:positionV" => anchor_axis = None,
                    b"wp:anchor" => {
                        /* Like `</wp:inline>` below: the flag is read by
                        the `</w:drawing>` handler, which does the reset. */
                    }
                    b"wp:inline" => {
                        /* Don't clear `in_wp_inline` on the inline close —
                        it's structurally a child of `<w:drawing>`, so the
                        outer `</w:drawing>` handler is the one that needs
                        to see "this drawing wrapped an inline picture"
                        (vs anchor / floating) when it decides whether to
                        push the inline object. Clearing here would race
                        the two close handlers: `</wp:inline>` always
                        fires before `</w:drawing>`, leaving the outer
                        check with `in_wp_inline == false` and the image
                        silently dropped. The drawing-close handler does
                        the full reset for both flags. */
                    }
                    b"w:drawing" => {
                        if in_drawing
                            && (in_wp_inline || in_wp_anchor)
                            && let Some(rid) = cur_drawing_rel_id.take()
                            && let (Some(cx), Some(cy)) =
                                (cur_drawing_cx.take(), cur_drawing_cy.take())
                        {
                            /* Inject U+FFFC as the inline object's anchor
                            character. Position in the eventual `para_text` =
                            `para_text.len()` (already flushed runs) +
                            `run_text.len()` (this run's text so far).
                            Issue #69 — a `<wp:anchor>` picture takes the
                            same sentinel; its placement rides `anchor`. */
                            let at = (para_text.len() + run_text.len()) as u32;
                            run_text.push('\u{FFFC}');
                            para_inline_objects.push(engine::InlineObject {
                                at,
                                kind: engine::InlineKind::Image {
                                    rel_id: rid,
                                    width_emu: cx,
                                    height_emu: cy,
                                },
                                anchor: if in_wp_anchor {
                                    cur_anchor.take()
                                } else {
                                    None
                                },
                            });
                        } else {
                            /* Shape / text box (no blip) or malformed — drop. */
                            cur_drawing_rel_id = None;
                            cur_drawing_cx = None;
                            cur_drawing_cy = None;
                        }
                        in_drawing = false;
                        in_wp_inline = false;
                        in_wp_anchor = false;
                        cur_anchor = None;
                        anchor_axis = None;
                        anchor_offset = None;
                    }
                    b"w:fldSimple" => {
                        /* Issue #43 — seal the compact field. A result-
                        less or instruction-less fldSimple leaves no
                        anchor to re-evaluate; skip it (matches the
                        fldChar machine's guards). */
                        if let Some((instr, start)) = fld_simple_stack.pop() {
                            let end = (para_text.len() + run_text.len()) as u32;
                            let instr = instr.trim().to_string();
                            if end > start && !instr.is_empty() {
                                para_fields.push(engine::Field {
                                    start,
                                    end,
                                    instruction: instr,
                                });
                            }
                        }
                    }
                    b"w:hyperlink" => {
                        if let Some((target, start)) = hyperlink_stack.pop() {
                            let end = (para_text.len() + run_text.len()) as u32;
                            if end > start {
                                para_hyperlinks.push(engine::Hyperlink { start, end, target });
                            }
                        }
                    }
                    b"w:footnotePr" | b"w:endnotePr" if in_sect_pr => {
                        /* Issue #80 — close the note-props scope. */
                        cur_sect.note_pr_scope = None;
                    }
                    b"w:sectPr" => {
                        /* Inline (inside a paragraph's `<w:pPr>`) → stash for
                        the matching `</w:p>` end. Body-level (the sectPr that
                        lives directly under `<w:body>`, after every
                        paragraph) → finalize a section covering everything
                        from `sect_start_block` to the current block count. */
                        in_sect_pr = false;
                        let taken = std::mem::take(&mut cur_sect);
                        if in_ppr {
                            pending_paragraph_sect = Some(taken);
                        } else {
                            let end = out_blocks.len() as u32;
                            if end > sect_start_block {
                                let header_refs = taken.header_refs.clone();
                                let footer_refs = taken.footer_refs.clone();
                                let title_pg = taken.title_pg;
                                let columns = taken.columns.unwrap_or_default();
                                let page_num = taken.page_num.unwrap_or_default();
                                let section_type = taken.section_type;
                                let footnote_props = taken.footnote_props;
                                let endnote_props = taken.endnote_props;
                                out_sections.push(Section {
                                    geometry: taken.into_geometry(),
                                    start_block: sect_start_block,
                                    end_block: end,
                                    header_refs,
                                    footer_refs,
                                    title_pg,
                                    columns,
                                    page_num,
                                    section_type,
                                    footnote_props,
                                    endnote_props,
                                });
                                sect_start_block = end;
                            }
                        }
                    }
                    b"w:r" => {
                        in_run = false;
                        let start = para_text.len() as u32;
                        para_text.push_str(&run_text);
                        let end = para_text.len() as u32;
                        /* Issue #59 — clear immediately, not deferred to the
                        next `<w:r>` Start (`run_text.clear()` above). Every
                        paragraph-level wrapper that opens/closes AROUND
                        runs rather than inside one — `<w:hyperlink>`,
                        `<w:ins>`/`<w:del>`, `<w:commentRangeStart>`/`End` —
                        reads `para_text.len() + run_text.len()` at a moment
                        when no run is open, expecting that sum to equal
                        `para_text.len()` alone. Leaving the just-flushed
                        text sitting in `run_text` until the NEXT run's
                        Start event shifted every one of those byte ranges
                        by the length of whichever run most recently closed. */
                        run_text.clear();
                        if start == end {
                            continue;
                        }
                        /* Issue #29 — spans carry only what the STYLE TABLE
                        cannot re-derive: paragraph-mark rPr + character-style
                        chain + direct rPr. The docDefaults <w:rPr> and the
                        pStyle-chain <w:rPr> are deliberately NOT baked here —
                        the engine folds them at span-materialize time
                        (`build_style_spans`), which is what lets ModifyStyle
                        re-cascade loaded documents and stops dirty-paragraph
                        saves from writing style-derived props as direct
                        formatting. Final precedence is unchanged:
                        defaults → pStyle chain → pmark → rStyle → direct. */
                        let style = resolver.resolve_run(
                            pmark_rpr.clone(),
                            r_style_id.as_deref(),
                            direct_rpr.clone(),
                        );
                        if style != SpanStyle::default() {
                            match spans.last_mut() {
                                Some(last) if last.end == start && last.style == style => {
                                    last.end = end;
                                }
                                _ => spans.push(StyleRun { start, end, style }),
                            }
                        }
                    }
                    b"w:p" => {
                        /* Read position after `</w:p>` — that's where the `>`
                        closes — gives us the end byte. */
                        let p_end_byte = reader.buffer_position() as usize;
                        let source_xml = p_start_byte
                            .take()
                            .and_then(|s| slice_element(xml, s, p_end_byte, b"w:p"));

                        /* Sprint 12 (#11) — preserve the direct `<w:pPr>`
                        and the `<w:pStyle>` reference on the Paragraph
                        so the live editor can re-resolve the cascade on
                        a future ApplyStyle. The resolver still consumes
                        the originals to produce the up-front resolved
                        view; we clone before consumption. */
                        let style_id_for_paragraph = p_style_id.clone();
                        let direct_overrides_for_paragraph = direct_ppr.clone();
                        /* Paragraph cascade: bake direct_ppr on top of doc
                        defaults + pStyle chain. The baseline rPr we computed
                        per-run is informational here. */
                        let (props, _) = resolver.resolve_paragraph(
                            p_style_id.take().as_deref(),
                            std::mem::take(&mut direct_ppr),
                            std::mem::take(&mut pmark_rpr),
                        );
                        /* Compose `ListItem` from the per-paragraph numPr
                        accumulators; partial refs (numId without ilvl, or
                        vice versa) default the missing field to 0 — Word
                        treats absent `<w:ilvl>` as level 0.

                        Audit gap A.M17 — when the paragraph has no
                        direct `<w:pPr><w:numPr>` but its resolved style
                        cascade (`props.list_item`) carries one, inherit
                        the binding. Direct numPr still wins (highest
                        specificity). The cascade was folded by
                        `resolve_paragraph` above. */
                        let list_item = match (list_num_id.take(), list_ilvl.take()) {
                            (Some(num_id), ilvl) => Some(ListItem {
                                num_id,
                                ilvl: ilvl.unwrap_or(0),
                            }),
                            (None, _) => props.list_item,
                        };
                        out_blocks.push(Block::Paragraph(Paragraph {
                            text: std::mem::take(&mut para_text),
                            spans: std::mem::take(&mut spans),
                            props,
                            list_item,
                            /* Resolver fills this in a second pass once the full
                            doc order is known (see `opc::archive::read_docx`). */
                            resolved_marker: None,
                            resolved_list_indent: None,
                            dirty: false,
                            source_xml,
                            inline_objects: std::mem::take(&mut para_inline_objects),
                            hyperlinks: std::mem::take(&mut para_hyperlinks),
                            revisions: std::mem::take(&mut para_revisions),
                            fields: std::mem::take(&mut para_fields),
                            style_id: style_id_for_paragraph,
                            direct_overrides: direct_overrides_for_paragraph,
                            /* Phase 3 (#40) — the range→marker conversion
                            happens in `from_blocks_with_sections`; the
                            parser keeps emitting range-stamped `Section`s
                            below. */
                            section_end: None,
                        }));
                        /* Phase 6 — inline `<w:sectPr>` ends the section at this
                        paragraph. Emit a `Section` covering everything since
                        the last break; the next paragraph starts a fresh
                        section. */
                        if let Some(sect) = pending_paragraph_sect.take() {
                            let end = out_blocks.len() as u32;
                            if end > sect_start_block {
                                let header_refs = sect.header_refs.clone();
                                let footer_refs = sect.footer_refs.clone();
                                let title_pg = sect.title_pg;
                                let columns = sect.columns.unwrap_or_default();
                                let page_num = sect.page_num.unwrap_or_default();
                                let section_type = sect.section_type;
                                let footnote_props = sect.footnote_props;
                                let endnote_props = sect.endnote_props;
                                out_sections.push(Section {
                                    geometry: sect.into_geometry(),
                                    start_block: sect_start_block,
                                    end_block: end,
                                    header_refs,
                                    footer_refs,
                                    title_pg,
                                    columns,
                                    page_num,
                                    section_type,
                                    footnote_props,
                                    endnote_props,
                                });
                                sect_start_block = end;
                            }
                        }
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

    /* Phase 6 — close out any unsectioned trailing blocks. A document with
    no `<w:sectPr>` at all (rare — the spec requires at least one, but the
    reader is lenient) lands here with `sect_start_block == 0` and the
    block count as the upper bound, producing one implicit section. A
    document whose final body-level sectPr already covered every block
    leaves `sect_start_block == out_blocks.len()`, so this branch is a
    no-op. */
    let total = out_blocks.len() as u32;
    if total > sect_start_block {
        out_sections.push(Section {
            geometry: PageGeometry::a4(),
            start_block: sect_start_block,
            end_block: total,
            header_refs: HeaderFooterRefs::default(),
            footer_refs: HeaderFooterRefs::default(),
            title_pg: false,
            columns: engine::ColumnSpec::single(),
            page_num: engine::PageNumType::default(),
            section_type: engine::SectionType::default(),
            footnote_props: engine::NoteProps::default(),
            endnote_props: engine::NoteProps::default(),
        });
    }

    let mut tree = DocumentTree::from_blocks_with_sections(out_blocks, out_sections);
    tree.comment_ranges = out_comment_ranges;
    Ok(tree)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parts::styles::StyleTable;

    /// Issue #110 — a `word/document.xml` that starts with a UTF-8 BOM
    /// (docx4j / Apache POI output) must still capture each `<w:p>`'s
    /// EXACT source bytes. quick-xml strips the BOM from its input without
    /// counting it in `buffer_position()`; the capture offsets have to
    /// live in the same byte space as the slice they index.
    #[test]
    fn bom_prefixed_part_captures_exact_paragraph_bytes() {
        let body = r#"<?xml version="1.0" encoding="utf-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>first</w:t></w:r></w:p><w:p><w:r><w:t>second</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/></w:body></w:document>"#;
        let mut xml = b"\xEF\xBB\xBF".to_vec();
        xml.extend_from_slice(body.as_bytes());

        let table = StyleTable::default();
        let resolver = StyleResolver::new(&table);
        let tree = parse_document_xml(&xml, &resolver).expect("parse");

        let p0 = tree.blocks[0].as_paragraph().expect("paragraph 0");
        assert_eq!(p0.text, "first");
        assert_eq!(
            p0.source_xml.as_deref(),
            Some(b"<w:p><w:r><w:t>first</w:t></w:r></w:p>".as_slice()),
            "paragraph 0 source bytes must be the exact <w:p> element"
        );
        let p1 = tree.blocks[1].as_paragraph().expect("paragraph 1");
        assert_eq!(
            p1.source_xml.as_deref(),
            Some(b"<w:p><w:r><w:t>second</w:t></w:r></w:p>".as_slice()),
        );
        let t = tree.blocks[2].as_table().expect("table");
        let raw = t.source_xml.as_deref().expect("table source bytes");
        assert!(
            raw.starts_with(b"<w:tbl>"),
            "{:?}",
            String::from_utf8_lossy(raw)
        );
        assert!(
            raw.ends_with(b"</w:tbl>"),
            "{:?}",
            String::from_utf8_lossy(raw)
        );
        assert_eq!(t.rows.len(), 1, "table rows must still parse behind a BOM");
    }

    /* ---------------------------------------------------------------
    Issue #69 — `<wp:anchor>` floating pictures.
    --------------------------------------------------------------- */

    const DRAWING_ROOT: &str = concat!(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
        r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
        r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
        r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
        r#"xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" "#,
        r#"xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing">"#,
    );

    /// The `<a:graphic>` picture body Word writes (compact, no whitespace).
    const PIC_GRAPHIC: &str = concat!(
        r#"<wp:cNvGraphicFramePr/><a:graphic>"#,
        r#"<a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture">"#,
        r#"<pic:pic><pic:nvPicPr><pic:cNvPr id="0" name="Image"/><pic:cNvPicPr/></pic:nvPicPr>"#,
        r#"<pic:blipFill><a:blip r:embed="rId5"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>"#,
        r#"<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="457200"/></a:xfrm>"#,
        r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic>"#,
        r#"</a:graphicData></a:graphic>"#,
    );

    fn parse_body(body: &str) -> DocumentTree {
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>{DRAWING_ROOT}<w:body>{body}<w:sectPr/></w:body></w:document>"
        );
        let table = StyleTable::default();
        let resolver = StyleResolver::new(&table);
        parse_document_xml(xml.as_bytes(), &resolver).expect("parse")
    }

    #[test]
    fn anchored_picture_parses_into_a_floating_inline_object() {
        let body = format!(
            concat!(
                r#"<w:p><w:r><w:t xml:space="preserve">float </w:t></w:r><w:r><w:drawing>"#,
                r#"<wp:anchor distT="0" distB="0" distL="114300" distR="114300" simplePos="0" "#,
                r#"relativeHeight="251659264" behindDoc="1" locked="0" layoutInCell="1" allowOverlap="1">"#,
                r#"<wp:simplePos x="0" y="0"/>"#,
                r#"<wp:positionH relativeFrom="margin"><wp:align>center</wp:align></wp:positionH>"#,
                r#"<wp:positionV relativeFrom="page"><wp:posOffset>1828800</wp:posOffset></wp:positionV>"#,
                r#"<wp:extent cx="914400" cy="457200"/><wp:effectExtent l="0" t="0" r="0" b="0"/>"#,
                r#"<wp:wrapSquare wrapText="bothSides"/>"#,
                r#"<wp:docPr id="7" name="Picture 7" descr="a float"/>"#,
                "{pic}",
                r#"</wp:anchor></w:drawing></w:r><w:r><w:t>here</w:t></w:r></w:p>"#,
            ),
            pic = PIC_GRAPHIC
        );
        let tree = parse_body(&body);
        let p = tree.blocks[0].as_paragraph().expect("paragraph");
        assert_eq!(
            p.text, "float \u{FFFC}here",
            "the anchor takes the same sentinel as an inline"
        );
        assert_eq!(p.inline_objects.len(), 1);
        let obj = &p.inline_objects[0];
        assert_eq!(obj.at, 6);
        match &obj.kind {
            engine::InlineKind::Image {
                rel_id,
                width_emu,
                height_emu,
            } => {
                assert_eq!(rel_id, "rId5");
                assert_eq!((*width_emu, *height_emu), (914_400, 457_200));
            }
            other => panic!("expected an image, got {other:?}"),
        }
        let a = obj.anchor.as_deref().expect("floating");
        assert!(a.behind_doc);
        assert!(!a.locked);
        assert!(a.layout_in_cell);
        assert!(a.allow_overlap);
        assert!(!a.simple_pos);
        assert!(!a.hidden);
        assert_eq!(a.relative_height, 251_659_264);
        assert_eq!((a.dist_left_emu, a.dist_right_emu), (114_300, 114_300));
        assert_eq!((a.dist_top_emu, a.dist_bottom_emu), (0, 0));
        assert_eq!(a.position_h.relative_from, engine::HRelativeFrom::Margin);
        assert_eq!(
            a.position_h.offset,
            engine::FloatOffset::Align(engine::FloatAlign::Center)
        );
        assert_eq!(a.position_v.relative_from, engine::VRelativeFrom::Page);
        assert_eq!(a.position_v.offset, engine::FloatOffset::Emu(1_828_800));
        assert_eq!(a.wrap, engine::WrapKind::Square);
        assert_eq!(
            a.wrap_xml.as_deref(),
            Some(r#"<wp:wrapSquare wrapText="bothSides"/>"#),
            "the wrap element rides verbatim"
        );
        assert_eq!(
            a.doc_pr_xml.as_deref(),
            Some(r#"<wp:docPr id="7" name="Picture 7" descr="a float"/>"#),
            "docPr rides verbatim (id / name / descr are not modeled)"
        );
    }

    #[test]
    fn anchored_picture_reads_percent_offsets_simple_pos_and_wrap_polygon() {
        let wrap = concat!(
            r#"<wp:wrapTight wrapText="bothSides"><wp:wrapPolygon edited="1">"#,
            r#"<wp:start x="0" y="0"/><wp:lineTo x="0" y="21600"/><wp:lineTo x="21600" y="21600"/>"#,
            r#"<wp:lineTo x="21600" y="0"/><wp:lineTo x="0" y="0"/></wp:wrapPolygon></wp:wrapTight>"#,
        );
        let doc_pr = r#"<wp:docPr id="2" name="P"><a:hlinkClick r:id="rId9"/></wp:docPr>"#;
        let body = format!(
            concat!(
                r#"<w:p><w:r><w:drawing>"#,
                r#"<wp:anchor distT="10" distB="20" distL="30" distR="40" simplePos="1" "#,
                r#"relativeHeight="3" behindDoc="0" locked="1" layoutInCell="0" hidden="1" allowOverlap="0">"#,
                r#"<wp:simplePos x="100" y="200"/>"#,
                r#"<wp:positionH relativeFrom="page"><wp14:pctPosHOffset>25000</wp14:pctPosHOffset></wp:positionH>"#,
                r#"<wp:positionV relativeFrom="line"><wp:align>bottom</wp:align></wp:positionV>"#,
                r#"<wp:extent cx="914400" cy="457200"/><wp:effectExtent l="0" t="0" r="0" b="0"/>"#,
                "{wrap}{doc_pr}{pic}",
                r#"</wp:anchor></w:drawing></w:r></w:p>"#,
            ),
            wrap = wrap,
            doc_pr = doc_pr,
            pic = PIC_GRAPHIC
        );
        let tree = parse_body(&body);
        let p = tree.blocks[0].as_paragraph().expect("paragraph");
        assert_eq!(p.text, "\u{FFFC}");
        let a = p.inline_objects[0].anchor.as_deref().expect("floating");
        assert!(a.simple_pos);
        assert_eq!((a.simple_pos_x_emu, a.simple_pos_y_emu), (100, 200));
        assert!(a.locked);
        assert!(!a.layout_in_cell);
        assert!(a.hidden);
        assert!(!a.allow_overlap);
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
        assert_eq!(a.position_h.relative_from, engine::HRelativeFrom::Page);
        assert_eq!(
            a.position_h.offset,
            engine::FloatOffset::PercentMilli(25_000)
        );
        assert_eq!(a.position_v.relative_from, engine::VRelativeFrom::Line);
        assert_eq!(
            a.position_v.offset,
            engine::FloatOffset::Align(engine::FloatAlign::Bottom)
        );
        assert_eq!(a.wrap, engine::WrapKind::Tight);
        assert_eq!(a.wrap_xml.as_deref(), Some(wrap), "polygon rides verbatim");
        assert_eq!(
            a.doc_pr_xml.as_deref(),
            Some(doc_pr),
            "docPr children ride verbatim"
        );
    }

    /// A `<wp:anchor>` around a shape / text box has no `<a:blip>`: it is
    /// dropped exactly as before #69 (issue #119 owns text boxes), and the
    /// paragraph text around it survives with NO stray sentinel.
    #[test]
    fn anchored_shape_without_blip_is_dropped_not_misread_as_a_picture() {
        let body = concat!(
            r#"<w:p><w:r><w:t>a</w:t></w:r><w:r><w:drawing>"#,
            r#"<wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="1" "#,
            r#"behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1">"#,
            r#"<wp:simplePos x="0" y="0"/>"#,
            r#"<wp:positionH relativeFrom="column"><wp:posOffset>0</wp:posOffset></wp:positionH>"#,
            r#"<wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV>"#,
            r#"<wp:extent cx="100" cy="100"/><wp:wrapNone/><wp:docPr id="1" name="Shape"/>"#,
            r#"<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">"#,
            r#"<wps:wsp xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">"#,
            r#"<wps:spPr><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></wps:spPr></wps:wsp>"#,
            r#"</a:graphicData></a:graphic></wp:anchor></w:drawing></w:r>"#,
            r#"<w:r><w:t>b</w:t></w:r></w:p>"#,
        );
        let tree = parse_body(body);
        let p = tree.blocks[0].as_paragraph().expect("paragraph");
        assert_eq!(p.text, "ab");
        assert!(p.inline_objects.is_empty());
    }

    /// Issue #119 rider — a text box's `<w:txbxContent><w:p>` is a sub-
    /// story, never a body paragraph: the enclosing paragraph keeps its
    /// text AND its verbatim source capture (so a zero-edit resave keeps
    /// the drawing byte-for-byte), and the paragraph after it is still
    /// block 1.
    #[test]
    fn text_box_paragraphs_are_not_hoisted_into_the_body() {
        let body = concat!(
            r#"<w:p><w:r><w:t>a</w:t></w:r><w:r><w:drawing>"#,
            r#"<wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="1" "#,
            r#"behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1">"#,
            r#"<wp:simplePos x="0" y="0"/>"#,
            r#"<wp:positionH relativeFrom="column"><wp:posOffset>0</wp:posOffset></wp:positionH>"#,
            r#"<wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV>"#,
            r#"<wp:extent cx="100" cy="100"/><wp:wrapNone/><wp:docPr id="1" name="Text Box 1"/>"#,
            r#"<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">"#,
            r#"<wps:wsp xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">"#,
            r#"<wps:txbx><w:txbxContent>"#,
            r#"<w:p><w:r><w:t>inside the box</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>second box line</w:t></w:r></w:p>"#,
            r#"</w:txbxContent></wps:txbx></wps:wsp>"#,
            r#"</a:graphicData></a:graphic></wp:anchor></w:drawing></w:r>"#,
            r#"<w:r><w:t>b</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>second</w:t></w:r></w:p>"#,
        );
        let tree = parse_body(body);
        assert_eq!(tree.blocks.len(), 2, "only the two body paragraphs");
        let p0 = tree.blocks[0].as_paragraph().expect("paragraph 0");
        assert_eq!(p0.text, "ab");
        assert!(p0.inline_objects.is_empty(), "a text box is not a picture");
        let src = p0
            .source_xml
            .as_deref()
            .expect("passthrough capture intact");
        assert!(src.starts_with(b"<w:p>") && src.ends_with(b"</w:p>"));
        assert!(
            std::str::from_utf8(src).unwrap().contains("inside the box"),
            "the drawing rides the enclosing paragraph's verbatim bytes"
        );
        let p1 = tree.blocks[1].as_paragraph().expect("paragraph 1");
        assert_eq!(p1.text, "second");
    }

    /// The `mc:AlternateContent` shape: the Choice carries the DrawingML
    /// text box, the Fallback the VML `<w:pict><v:textbox>` duplicate —
    /// neither inner `<w:p>` may reach the body.
    #[test]
    fn alternate_content_fallback_and_vml_textbox_are_skipped() {
        let body = concat!(
            r#"<w:p><w:r><w:t>a</w:t></w:r>"#,
            r#"<w:r><mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">"#,
            r#"<mc:Choice Requires="wps"><w:drawing>"#,
            r#"<wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="1" "#,
            r#"behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1">"#,
            r#"<wp:simplePos x="0" y="0"/>"#,
            r#"<wp:positionH relativeFrom="column"><wp:posOffset>0</wp:posOffset></wp:positionH>"#,
            r#"<wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV>"#,
            r#"<wp:extent cx="100" cy="100"/><wp:wrapNone/><wp:docPr id="1" name="Text Box 1"/>"#,
            r#"<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">"#,
            r#"<wps:wsp xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">"#,
            r#"<wps:txbx><w:txbxContent><w:p><w:r><w:t>choice text</w:t></w:r></w:p></w:txbxContent></wps:txbx>"#,
            r#"</wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></mc:Choice>"#,
            r##"<mc:Fallback><w:pict><v:shape xmlns:v="urn:schemas-microsoft-com:vml" id="s1" type="#_x0000_t202">"##,
            r#"<v:textbox><w:txbxContent><w:p><w:r><w:t>fallback text</w:t></w:r></w:p></w:txbxContent></v:textbox>"#,
            r#"</v:shape></w:pict></mc:Fallback>"#,
            r#"</mc:AlternateContent></w:r>"#,
            r#"<w:r><w:t>b</w:t></w:r></w:p>"#,
        );
        let tree = parse_body(body);
        assert_eq!(tree.blocks.len(), 1);
        let p0 = tree.blocks[0].as_paragraph().expect("paragraph 0");
        assert_eq!(p0.text, "ab");
        assert!(p0.inline_objects.is_empty());
        assert!(p0.source_xml.is_some(), "passthrough capture intact");
    }

    /// The same document WITHOUT a BOM is the control — identical capture.
    #[test]
    fn bom_free_part_captures_exact_paragraph_bytes() {
        let xml = br#"<?xml version="1.0" encoding="utf-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>first</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
        let table = StyleTable::default();
        let resolver = StyleResolver::new(&table);
        let tree = parse_document_xml(xml, &resolver).expect("parse");
        let p0 = tree.blocks[0].as_paragraph().expect("paragraph 0");
        assert_eq!(
            p0.source_xml.as_deref(),
            Some(b"<w:p><w:r><w:t>first</w:t></w:r></w:p>".as_slice()),
        );
    }
}
