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
use crate::parts::textbox;
use crate::schema::block_envelope::BlockEnvelopes;
use crate::schema::ct_ppr::{apply_ppr, ppr_child_is_modeled};
use crate::schema::ct_rpr::{apply_rpr, attr_val, fold_rpr_fragment, rpr_child_is_modeled};
use crate::schema::drawing::scan_drawing;
use crate::schema::grab_bag::{
    NamespaceScope, bound_by_root, capture_subtree, slice_element, slice_fragment, stash,
};
use crate::schema::source_markup::{
    MarkupCapture, is_inline_marker, is_modeled_textless_run_child,
};
use crate::style_resolver::StyleResolver;
use engine::{
    Block, DocumentEnvelope, DocumentTree, HeaderFooterRefs, HeaderFooterRole, ListItem,
    PageGeometry, ParaProperties, Paragraph, Section, SpanStyle, StyleRun, Table,
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
    /// Issue #112 — the raw `<w:sectPr>…</w:sectPr>` bytes, for the
    /// writer's verified passthrough (`SectionProps::source_xml`).
    source_xml: Option<Vec<u8>>,
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
    /// Issue #81 — `(top-level block index, inside a table)` of the
    /// paragraph `separate` fired in. An `end` in a LATER top-level
    /// paragraph makes this a multi-paragraph field (a TOC): the Head
    /// overlay is stamped back onto that paragraph, the Tail onto the
    /// current one.
    cached_block: Option<(usize, bool)>,
}

/// Issue #81 — where the fldChar machine is in the block stream.
struct FieldCursor<'a> {
    para_text: &'a str,
    run_text: &'a str,
    /// Index the paragraph being parsed will take in `out_blocks`.
    block_idx: usize,
    in_table: bool,
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
    cur: FieldCursor<'_>,
    out_fields: &mut Vec<engine::Field>,
    out_blocks: &mut [Block],
) {
    let kind = attr_val(e, b"w:fldCharType").unwrap_or_default();
    let here = (cur.para_text.len() + cur.run_text.len()) as u32;
    match kind.trim() {
        "begin" => stack.push(FieldBuilder::default()),
        "separate" => {
            if let Some(top) = stack.last_mut() {
                top.cached_start = Some(here);
                top.cached_block = Some((cur.block_idx, cur.in_table));
            }
        }
        "end" => {
            if let Some(top) = stack.pop()
                && let Some(start) = top.cached_start
            {
                let end = here;
                let instruction = top.instruction.trim().to_string();
                /* Issue #81 — a result that crossed into a later top-level
                paragraph: Head on the separate's paragraph (already
                emitted), Tail here. Tables on either end keep the pre-#81
                behaviour (no overlay — the bytes ride the passthrough). */
                if let Some((b, in_table)) = top.cached_block
                    && b < cur.block_idx
                {
                    if !in_table
                        && !cur.in_table
                        && !instruction.is_empty()
                        && let Some(Block::Paragraph(p)) = out_blocks.get_mut(b)
                    {
                        let len = p.text.len() as u32;
                        p.fields.push(engine::Field {
                            start: start.min(len),
                            end: len,
                            instruction,
                            span: Some(engine::FieldSpan::Head),
                        });
                        out_fields.push(engine::Field {
                            start: 0,
                            end,
                            instruction: String::new(),
                            span: Some(engine::FieldSpan::Tail),
                        });
                    }
                    return;
                }
                /* Issue #81 — a TOC is ALWAYS the multi-paragraph shape,
                even when its whole result fits one paragraph, so the
                region / regeneration machinery sees every TOC. */
                if !instruction.is_empty()
                    && engine::FieldInstruction::parse(&instruction).keyword == "TOC"
                    && !cur.in_table
                {
                    let len = end.max(start);
                    out_fields.push(engine::Field {
                        start,
                        end: len,
                        instruction,
                        span: Some(engine::FieldSpan::Head),
                    });
                    out_fields.push(engine::Field {
                        start: 0,
                        end,
                        instruction: String::new(),
                        span: Some(engine::FieldSpan::Tail),
                    });
                    return;
                }
                if end > start && !instruction.is_empty() {
                    out_fields.push(engine::Field {
                        start,
                        end,
                        instruction,
                        span: None,
                    });
                }
            }
        }
        _ => { /* Unknown fldCharType — ignore. */ }
    }
}

/// Issue #120 — the self-contained elements a `<w:body>` / `<w:tc>` may
/// hold between two blocks (ECMA-376 §17.2.2 `EG_RunLevelElements` at
/// block level, plus `<w:altChunk>`) that the typed model does not
/// represent. Each is preserved verbatim on the neighbouring block.
/// `<w:commentRangeStart>` / `End` are deliberately absent: their own
/// arms record the comment range AND preserve the marker.
pub(crate) fn is_block_level_marker(qname: &[u8]) -> bool {
    matches!(
        qname,
        b"w:bookmarkStart"
            | b"w:bookmarkEnd"
            | b"w:proofErr"
            | b"w:permStart"
            | b"w:permEnd"
            | b"w:moveFromRangeStart"
            | b"w:moveFromRangeEnd"
            | b"w:moveToRangeStart"
            | b"w:moveToRangeEnd"
            | b"w:customXmlInsRangeStart"
            | b"w:customXmlInsRangeEnd"
            | b"w:customXmlDelRangeStart"
            | b"w:customXmlDelRangeEnd"
            | b"w:customXmlMoveFromRangeStart"
            | b"w:customXmlMoveFromRangeEnd"
            | b"w:customXmlMoveToRangeStart"
            | b"w:customXmlMoveToRangeEnd"
            | b"w:altChunk"
            | b"w:sdt"
            | b"w:customXml"
    )
}

/// Issue #112 — `true` when `tail` is exactly what may follow the last
/// body child of a `word/document.xml`: `</w:body>`, `</w:document>` and
/// whitespace around them, to EOF. Anything else means the reader's
/// picture of the part's end is off and the writer synthesizes the tail
/// instead of splicing bytes it does not understand.
pub(crate) fn valid_document_tail(tail: &[u8]) -> bool {
    fn skip_ws(s: &[u8]) -> &[u8] {
        let n = s.iter().take_while(|b| b.is_ascii_whitespace()).count();
        &s[n..]
    }
    fn expect<'a>(s: &'a [u8], lit: &[u8]) -> Option<&'a [u8]> {
        s.strip_prefix(lit)
    }
    let s = skip_ws(tail);
    let s = expect(s, b"</w:body").map(skip_ws);
    let s = s.and_then(|s| expect(s, b">")).map(skip_ws);
    let s = s.and_then(|s| expect(s, b"</w:document")).map(skip_ws);
    let s = s.and_then(|s| expect(s, b">")).map(skip_ws);
    matches!(s, Some(rest) if rest.is_empty())
}

/// Issue #83 / #119 — lower a captured DrawingML object element
/// (`<w:drawing>` or `<mc:AlternateContent>`) into a text-box story when
/// its FIRST `<wps:wsp>` carries a `<wps:txbx><w:txbxContent>`: the story
/// blocks parse through the body pipeline (`textbox::parse_story`), the
/// `<wps:bodyPr>` / `<wps:spPr>` / `<a:spAutoFit>` children fill the typed
/// fields. The VML fallback of an AlternateContent is skipped (the choice
/// defines the box; its `<w:txbxContent>` ranges are collected by the
/// caller for the writer's splice). `None` for a picture, a shape without
/// a story, or a box past the nesting cap — the caller keeps the element
/// as an opaque object instead.
fn lower_text_box(
    fragment: &[u8],
    resolver: &StyleResolver<'_>,
    ns: &NamespaceScope,
) -> Option<engine::TextBoxStory> {
    let mut reader = Reader::from_reader(fragment);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut prev_pos: usize = 0;
    let mut tb: Option<engine::TextBoxStory> = None;
    let mut has_story = false;
    let skip_subtree = |reader: &mut Reader<&[u8]>, e: &BytesStart| -> Option<usize> {
        let end_tag = e.to_end().into_owned();
        let mut skip = Vec::new();
        reader.read_to_end_into(end_tag.name(), &mut skip).ok()?;
        Some(reader.buffer_position() as usize)
    };
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) => match e.name().as_ref() {
                b"mc:Fallback" => {
                    skip_subtree(&mut reader, &e)?;
                }
                b"wps:wsp" if tb.is_none() => tb = Some(engine::TextBoxStory::default()),
                b"wps:bodyPr" if tb.is_some() => {
                    if let Some(t) = tb.as_mut() {
                        textbox::apply_body_pr(&e, t);
                    }
                }
                b"wps:spPr" if tb.is_some() => {
                    let end = skip_subtree(&mut reader, &e)?;
                    if let (Some(sub), Some(t)) = (fragment.get(prev_pos..end), tb.as_mut()) {
                        textbox::apply_sp_pr(sub, t);
                    }
                }
                b"a:spAutoFit" if tb.is_some() => {
                    if let Some(t) = tb.as_mut() {
                        t.auto_fit = true;
                    }
                }
                b"w:txbxContent" => {
                    let end = skip_subtree(&mut reader, &e)?;
                    if !has_story
                        && let Some(sub) = fragment.get(prev_pos..end)
                        && let Some(t) = tb.as_mut()
                        && let Some(blocks) = textbox::parse_story(sub, resolver, ns)
                    {
                        t.body = blocks;
                        has_story = true;
                    }
                }
                _ => {}
            },
            Event::Empty(e) => match e.name().as_ref() {
                b"wps:bodyPr" if tb.is_some() => {
                    if let Some(t) = tb.as_mut() {
                        textbox::apply_body_pr(&e, t);
                    }
                }
                b"a:spAutoFit" if tb.is_some() => {
                    if let Some(t) = tb.as_mut() {
                        t.auto_fit = true;
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        prev_pos = reader.buffer_position() as usize;
        buf.clear();
    }
    has_story.then_some(tb).flatten()
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
        /* Issue #81 — `w:leader` (TOC dot leaders). */
        leader: attr_val(e, b"w:leader")
            .map(|v| engine::TabLeader::from_ooxml(&v))
            .unwrap_or_default(),
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
    /// to `default_geometry` (issue #109 — a `<w:sectPr/>` with only a
    /// header reference, or one that omits `<w:pgSz>` altogether, still
    /// produces a usable section). `default_geometry` is
    /// `engine::PageGeometry::a4()` for the stock `read_docx` entry point;
    /// `read_docx_with_settings` threads a host-chosen
    /// `engine::DefaultPageSize::geometry()` down instead.
    fn into_geometry(self, default_geometry: PageGeometry) -> PageGeometry {
        let d = default_geometry;
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

    /// Materialize the range-stamped [`Section`] `[start_block, end_block)`
    /// the parser hands to `DocumentTree::from_blocks_with_sections`.
    fn into_section(
        self,
        start_block: u32,
        end_block: u32,
        default_geometry: PageGeometry,
    ) -> Section {
        Section {
            header_refs: self.header_refs.clone(),
            footer_refs: self.footer_refs.clone(),
            title_pg: self.title_pg,
            columns: self.columns.unwrap_or_default(),
            page_num: self.page_num.unwrap_or_default(),
            section_type: self.section_type,
            footnote_props: self.footnote_props,
            endnote_props: self.endnote_props,
            source_xml: self.source_xml.clone(),
            start_block,
            end_block,
            geometry: self.into_geometry(default_geometry),
        }
    }
}

/// Issue #112 — the typed [`engine::SectionProps`] a standalone
/// `<w:sectPr>…</w:sectPr>` fragment lowers to, exactly as the body parser
/// would lower it in place. The writer runs this over a section's
/// `source_xml` and compares the result to the live properties: equal ⇒
/// the bytes still describe the section and are written verbatim (a
/// *verified* passthrough); different ⇒ page setup, a header reference or
/// the section type changed and the element regenerates. `source_xml` is
/// left `None` on the result so the comparison is purely on properties.
pub(crate) fn parse_sect_pr_fragment(
    xml: &[u8],
    default_geometry: PageGeometry,
) -> engine::SectionProps {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut accum = SectPrAccum::default();
    let mut seen_root = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if !seen_root {
                    seen_root = true;
                } else {
                    accum.apply(e.name().as_ref(), &e);
                }
            }
            Ok(Event::End(e)) => {
                if matches!(e.name().as_ref(), b"w:footnotePr" | b"w:endnotePr") {
                    accum.note_pr_scope = None;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    engine::SectionProps::from(&accum.into_section(0, 1, default_geometry))
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
    parse_document_xml_with_warnings(xml, resolver, &mut warnings, PageGeometry::default())
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
///
/// `default_page_geometry` — issue #109 — is the fallback baked into any
/// `Section` whose `<w:sectPr>` omits `<w:pgSz>` (or has no `<w:sectPr>` at
/// all). `parse_document_xml` passes `PageGeometry::a4()`, matching the
/// pre-#109 hard-coded behavior byte-for-byte; `read_docx_with_settings`
/// (`opc::archive`) is the host-facing entry point that threads a
/// `DefaultPageSize::Letter` geometry down to this parameter instead.
pub fn parse_document_xml_with_warnings(
    xml: &[u8],
    resolver: &StyleResolver<'_>,
    warnings: &mut Vec<DocxWarning>,
    default_page_geometry: PageGeometry,
) -> Result<DocumentTree, DocxError> {
    /* Issue #110 — see `strip_utf8_bom`: `reader.buffer_position()` and
    every `xml[..]` slice below must share one byte space. */
    let xml_raw = xml;
    let xml = strip_utf8_bom(xml_raw);
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
    /* Issue #81 — `_Toc*` bookmarks opened inside the current paragraph. */
    let mut para_bookmarks: Vec<engine::Bookmark> = Vec::new();

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

    /* Issue #119 — run-level drawing objects (`<w:drawing>`,
    `<mc:AlternateContent>`, `<w:pict>`, `<w:object>`) are captured whole
    at their start tag and lowered by `schema::drawing::scan_drawing`
    (picture blip + extent + typed `<wp:anchor>`, exactly what the old
    in-loop state machine collected); the verbatim bytes ride the
    `InlineObject` so a regenerated paragraph re-emits the object — text
    box, shape and OLE object included — instead of dropping it. */

    /* Issue #120 / #112 — block-level passthrough + the part envelope.
    `envelopes` tracks the markup between and around blocks (content-
    control envelopes, bookmarks, inter-block whitespace) for the current
    block container; `in_block_container` is true between the start and
    end tags of `<w:body>` / `<w:hdr>` / `<w:ftr>` / `<w:footnote>` /
    `<w:endnote>` / `<w:comment>`. "Block level" = inside that container
    and outside any `<w:p>`, `<w:tbl>` or `<w:sectPr>`. `envelope`
    collects the `word/document.xml` prolog, root start tag, `<w:body>`
    tag and tail when the root is `<w:document>`, so a resave reproduces
    the bytes the writer used to synthesize. */
    let had_bom = xml_raw.len() != xml.len();
    let mut envelopes = BlockEnvelopes::new();
    let mut in_block_container = false;
    let mut root_is_document = false;
    let mut root_end: usize = 0;
    let mut envelope = DocumentEnvelope::default();
    /* A part with two `<w:body>` elements (Apache POI's `MultipleBodyBug`)
    has no single envelope to reproduce; the writer synthesizes. */
    let mut envelope_invalid = false;
    let mut tail_start: Option<usize> = None;
    let mut sect_pr_start: Option<usize> = None;
    /* A body whose only child is the `<w:sectPr>` (no block at all) has
    no section range to stamp; keep the accumulator so the trailing
    section — and its bytes — still land on `body_section`. */
    let mut trailing_sect_without_blocks: Option<SectPrAccum> = None;

    /* Phase 7 — `<w:hyperlink>` overlays. Word lays paragraph text out
    paragraph-flat with `<w:hyperlink>` spanning a contiguous slice of
    `<w:r>` elements that all share the link target. Capture the rId
    when the element opens; mark the byte range covered when it
    closes. `target` is the rId at this stage — the archive resolver
    swaps it to a URL via the rels map in a second pass. */
    let mut hyperlink_stack: Vec<(String, u32, Vec<engine::SourceAttr>)> = Vec::new();

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

    /* Issues #199 / #106 — the open paragraph's attribute-level grab bag
    and in-paragraph source markup (`schema::source_markup`). `toc_ids`
    holds the `w:id` of every `_Toc*` bookmark the model owns, so its
    `<w:bookmarkEnd>` is not ALSO kept as a verbatim marker. */
    let mut markup = MarkupCapture::new();
    let mut toc_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

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
                    root_end = reader.buffer_position() as usize;
                    match name.as_ref() {
                        b"w:document" => {
                            /* Issue #112 — everything before the root
                            (BOM, declaration, the newline after it) and
                            the root start tag itself, verbatim. */
                            root_is_document = true;
                            if had_bom {
                                envelope.prolog.extend_from_slice(b"\xEF\xBB\xBF");
                            }
                            envelope.prolog.extend_from_slice(&xml[..prev_pos]);
                            envelope.root_tag = xml[prev_pos..root_end].to_vec();
                        }
                        /* A header / footer part holds its blocks directly
                        under the root. */
                        b"w:hdr" | b"w:ftr" => in_block_container = true,
                        _ => {}
                    }
                    prev_pos = root_end;
                    buf.clear();
                    continue;
                }
                let at_block_level =
                    in_block_container && p_start_byte.is_none() && in_tbl == 0 && !in_sect_pr;
                /* Phase 5 PR 1 — outermost `<w:tbl>` opens. Capture leading
                byte offset for the source-byte passthrough; ignore every
                child event (`<w:p>` / `<w:r>` etc. inside cells) until the
                matching `</w:tbl>` brings the depth back to 0. Nested
                tables inside cells just bump the counter further. */
                if name.as_ref() == b"w:tbl" {
                    if in_tbl == 0 {
                        tbl_start_byte = Some(prev_pos);
                        envelopes.note_block_start(prev_pos);
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
                if in_run && is_modeled_textless_run_child(name.as_ref()) {
                    markup.run_modeled();
                }
                match name.as_ref() {
                    /* Issue #119 — a run-level object element: capture the
                    whole subtree, lower the modeled facts out of it, and
                    anchor ONE inline object carrying the bytes. A picture
                    gets its blip + extent (+ typed anchor) exactly as
                    before; a text box, shape, chart or OLE object gets its
                    extent, an empty `rel_id` and the bytes — layout
                    reserves the box, the writer re-emits the element. */
                    b"w:drawing" | b"mc:AlternateContent" | b"w:pict" | b"w:object"
                        if in_run && !in_rpr =>
                    {
                        let start = prev_pos;
                        if let Some(frag) = capture_subtree(xml, start, &mut reader, &e)? {
                            let end = reader.buffer_position() as usize;
                            let scan = scan_drawing(&frag);
                            /* Issue #83 — a text box (a `<wps:wsp>` shape with
                            a `<wps:txbx>` story, or a VML `<v:textbox>`) is
                            modeled as a story: its blocks parse through the
                            body pipeline, the whole element is its verbatim
                            container and the `<w:txbxContent>` ranges are
                            the writer's splice points. */
                            let text_box = if scan.drawing_ml {
                                lower_text_box(&frag, resolver, &ns).map(|tb| {
                                    (
                                        scan.cx.unwrap_or(0),
                                        scan.cy.unwrap_or(0),
                                        scan.anchor.clone(),
                                        tb,
                                    )
                                })
                            } else {
                                textbox::parse_vml(&frag, resolver, &ns)
                                    .map(|v| (v.width_emu, v.height_emu, v.anchor, v.story))
                            };
                            let at = (para_text.len() + run_text.len()) as u32;
                            if let Some((width_emu, height_emu, anchor, mut story)) = text_box {
                                story.story_ranges =
                                    textbox::element_ranges(&frag, b"w:txbxContent")
                                        .into_iter()
                                        .map(|(s, e)| (s as u32, e as u32))
                                        .collect();
                                story.host_range = p_start_byte
                                    .filter(|p| *p <= start)
                                    .map(|p| ((start - p) as u32, (end - p) as u32));
                                story.source_xml = String::from_utf8(frag).ok();
                                run_text.push('\u{FFFC}');
                                para_inline_objects.push(engine::InlineObject {
                                    at,
                                    kind: engine::InlineKind::TextBox {
                                        width_emu,
                                        height_emu,
                                        story: Box::new(story),
                                    },
                                    anchor,
                                    source_xml: None,
                                });
                            } else {
                                let (rel_id, width_emu, height_emu) = scan.image_fields();
                                /* A fragment whose prefixes the writer cannot
                                re-bind (declared on an intermediate ancestor,
                                not the part root) is not kept — the object
                                then regenerates as a picture when it has one
                                and is dropped otherwise, the pre-#119 outcome
                                rather than an unbound-prefix save. */
                                let keep_source = bound_by_root(&frag, &ns);
                                if !rel_id.is_empty() || keep_source {
                                    run_text.push('\u{FFFC}');
                                    para_inline_objects.push(engine::InlineObject {
                                        at,
                                        kind: engine::InlineKind::Image {
                                            rel_id,
                                            width_emu,
                                            height_emu,
                                            media_key: None,
                                        },
                                        anchor: scan.anchor,
                                        source_xml: keep_source.then_some(frag),
                                    });
                                }
                            }
                        }
                        prev_pos = reader.buffer_position() as usize;
                        buf.clear();
                        continue;
                    }
                    /* Issue #119 rider (shipped with #69) — a drawing
                    sub-story outside a run (a text box's `<w:txbxContent>`,
                    the VML `<w:pict>` / `<mc:Fallback>` duplicate) is never
                    body content: skip it whole so its `<w:p>` cannot end
                    the enclosing paragraph. Inside a run the arm above
                    already captured it. */
                    b"w:txbxContent" | b"w:pict" | b"mc:Fallback" => {
                        let _ = capture_subtree(xml, prev_pos, &mut reader, &e)?;
                        prev_pos = reader.buffer_position() as usize;
                        buf.clear();
                        continue;
                    }
                    /* Issue #120 — a block-level container (`<w:sdt>`
                    content control, `<w:customXml>`): its inner blocks stay
                    body blocks; the envelope around them is tracked. */
                    b"w:sdt" | b"w:customXml" if at_block_level => {
                        envelopes.open_container(prev_pos);
                        envelopes.set_blocks_at_open(out_blocks.len());
                        prev_pos = reader.buffer_position() as usize;
                        buf.clear();
                        continue;
                    }
                    /* The container's property children carry `<w:rPr>` /
                    `<w:pPr>`-shaped content that must not leak into the
                    live state; the bytes are inside the envelope's opener. */
                    b"w:sdtPr" | b"w:sdtEndPr" | b"w:customXmlPr"
                        if at_block_level && envelopes.in_container() =>
                    {
                        let _ = capture_subtree(xml, prev_pos, &mut reader, &e)?;
                        prev_pos = reader.buffer_position() as usize;
                        buf.clear();
                        continue;
                    }
                    /* Issue #112 — the block container opens. */
                    b"w:body" if root_is_document && !in_block_container && in_tbl == 0 => {
                        let pos = reader.buffer_position() as usize;
                        if !envelope.body_tag.is_empty() {
                            envelope_invalid = true;
                        }
                        envelope.root_to_body = xml[root_end..prev_pos].to_vec();
                        envelope.body_tag = xml[prev_pos..pos].to_vec();
                        in_block_container = true;
                        prev_pos = pos;
                        buf.clear();
                        continue;
                    }
                    b"w:footnote" | b"w:endnote" | b"w:comment"
                        if !in_block_container && p_start_byte.is_none() && in_tbl == 0 =>
                    {
                        in_block_container = true;
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
                        envelopes.note_block_start(prev_pos);
                        markup.open_paragraph(&e, &ns, reader.buffer_position() as usize);
                        p_style_id = None;
                        direct_ppr = ParaProperties::default();
                        pmark_rpr = SpanStyle::default();
                    }
                    b"w:r" => {
                        in_run = true;
                        r_style_id = None;
                        direct_rpr = SpanStyle::default();
                        run_text.clear();
                        markup.open_run(&e, &ns, prev_pos, para_text.len() as u32);
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
                    b"w:rPr" => {
                        in_rpr = true;
                        if in_run {
                            markup.run_rpr_start(prev_pos);
                        }
                    }
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
                        a `<w:pPr>`) and body-level both route here. Issue
                        #112 — remember where the element starts so its
                        bytes ride the section; at body level, whatever
                        block-level markup is still pending (whitespace,
                        a bookmark end) attaches after the last block, ahead
                        of the sectPr. */
                        in_sect_pr = true;
                        cur_sect = SectPrAccum::default();
                        sect_pr_start = Some(prev_pos);
                        if in_ppr {
                            markup.note_sect_in_ppr();
                        }
                        if at_block_level {
                            envelopes.finish(&mut out_blocks);
                        }
                    }
                    b"w:hyperlink" => {
                        let start = (para_text.len() + run_text.len()) as u32;
                        /* Issue #81 — an internal `w:anchor` link (a TOC
                        entry) rides as a `#name` target; a link with
                        neither pushes an empty marker so its end tag
                        pops the right entry. */
                        let target = attr_val(&e, b"r:id")
                            .or_else(|| attr_val(&e, b"w:anchor").map(|a| format!("#{a}")))
                            .unwrap_or_default();
                        /* Issue #242 — the source attributes (`r:id`, `w:history`,
                        `w:tooltip`, …) ride the link for regeneration. */
                        let attrs = crate::schema::source_markup::raw_attrs(&e, &ns);
                        hyperlink_stack.push((target, start, attrs));
                    }
                    b"w:t" => {
                        in_text_elt = true;
                        markup.run_text_elt(&e, &ns);
                    }
                    b"w:delText" => {
                        in_del_text_elt = true;
                        markup.run_text_elt(&e, &ns);
                    }
                    b"w:instrText" => in_instr_text = true,
                    b"w:fldChar" => {
                        /* fldChar drives the field state machine. The
                        attribute value lives on the start tag's `w:fldCharType`
                        attribute. `Start(...)` and `Empty(...)` both end up
                        here — match `Empty` below as well for completeness. */
                        handle_fld_char(
                            &e,
                            &mut field_stack,
                            FieldCursor {
                                para_text: &para_text,
                                run_text: &run_text,
                                block_idx: out_blocks.len(),
                                in_table: in_tbl > 0,
                            },
                            &mut para_fields,
                            &mut out_blocks,
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
                let at_block_level =
                    in_block_container && p_start_byte.is_none() && in_tbl == 0 && !in_sect_pr;
                if in_run && is_modeled_textless_run_child(name.as_ref()) {
                    markup.run_modeled();
                }
                /* Issues #199 / #106 — in-paragraph markup the model does
                not represent: the empty `<w:pPr/>`, an empty run `<w:rPr/>`,
                a leading `<w:lastRenderedPageBreak/>`, and the positioned
                markers (`<w:proofErr/>`, bookmarks, permission ranges). */
                let in_para = p_start_byte.is_some() && in_tbl == 0;
                let here = reader.buffer_position() as usize;
                match name.as_ref() {
                    b"w:pPr" if in_para && !in_run => markup.close_ppr(xml, here, &ns),
                    b"w:rPr" if in_para && in_run => {
                        if let Some(frag) = slice_fragment(xml, prev_pos, here) {
                            markup.run_rpr_empty(frag);
                        }
                    }
                    b"w:lastRenderedPageBreak" if in_para && in_run => {
                        if let Some(frag) = xml.get(prev_pos..here) {
                            markup.run_lead(frag, run_text.is_empty());
                        }
                    }
                    b"w:bookmarkEnd" if in_para && !in_run && !in_ppr => {
                        let modeled = attr_val(&e, b"w:id").is_some_and(|id| toc_ids.contains(&id));
                        if !modeled && let Some(frag) = slice_fragment(xml, prev_pos, here) {
                            markup.marker(para_text.len() as u32, frag, &ns);
                        }
                    }
                    n if in_para
                        && !in_run
                        && !in_ppr
                        && n != b"w:bookmarkStart"
                        && is_inline_marker(n) =>
                    {
                        if let Some(frag) = slice_fragment(xml, prev_pos, here) {
                            markup.marker(para_text.len() as u32, frag, &ns);
                        }
                    }
                    _ => {}
                }
                match name.as_ref() {
                    b"w:p" if at_block_level => {
                        /* Issue #120 — a self-closing `<w:p …/>` (an empty
                        paragraph whose properties are attributes only:
                        rsids, `w14:paraId`). quick-xml reports it as ONE
                        `Empty` event, so the Start / End arms never see it
                        and the paragraph used to vanish. Same block as an
                        empty `<w:p></w:p>`: default cascade, its own bytes
                        for the passthrough. */
                        let end = reader.buffer_position() as usize;
                        envelopes.note_block_start(prev_pos);
                        let source_xml = slice_element(xml, prev_pos, end, b"w:p");
                        let (props, _) = resolver.resolve_paragraph(
                            None,
                            ParaProperties::default(),
                            SpanStyle::default(),
                        );
                        let list_item = props.list_item;
                        markup.open_paragraph(&e, &ns, end);
                        let source_markup = markup.finish(0, &props, &None, list_item);
                        out_blocks.push(Block::Paragraph(Paragraph {
                            props,
                            list_item,
                            source_xml,
                            source_markup,
                            body_xml: envelopes.take_before(),
                            ..Paragraph::default()
                        }));
                        envelopes.note_block_end(end);
                    }
                    b"w:sectPr" => {
                        /* An empty `<w:sectPr/>` — stock page setup. Issue
                        #112: its bytes ride the section like a full one. */
                        let end = reader.buffer_position() as usize;
                        if at_block_level {
                            envelopes.finish(&mut out_blocks);
                        }
                        let taken = SectPrAccum {
                            source_xml: slice_element(xml, prev_pos, end, b"w:sectPr"),
                            ..SectPrAccum::default()
                        };
                        if in_ppr {
                            markup.note_sect_in_ppr();
                            pending_paragraph_sect = Some(taken);
                        } else {
                            let block_end = out_blocks.len() as u32;
                            if block_end > sect_start_block {
                                out_sections.push(taken.into_section(
                                    sect_start_block,
                                    block_end,
                                    default_page_geometry,
                                ));
                                sect_start_block = block_end;
                            } else {
                                trailing_sect_without_blocks = Some(taken);
                            }
                            if in_block_container {
                                tail_start = Some(end);
                            }
                        }
                    }
                    /* Issue #120 — block-level range markers and other
                    self-contained body children the model does not
                    represent: verbatim, attached to the following block
                    (or after the last one). Comment range markers are
                    ALSO recorded as ranges in their own arms below. */
                    n if at_block_level && is_block_level_marker(n) => {
                        let end = reader.buffer_position() as usize;
                        if let Some(frag) = slice_fragment(xml, prev_pos, end) {
                            envelopes.push_verbatim(frag);
                        }
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
                                source_xml: None,
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
                            source_xml: None,
                        });
                    }
                    b"w:bookmarkStart" if in_tbl == 0 && p_start_byte.is_some() => {
                        let raw_id = attr_val(&e, b"w:id");
                        let toc = attr_val(&e, b"w:name")
                            .filter(|name| engine::toc::is_toc_bookmark(name));
                        if let Some(name) = toc {
                            /* Modeled (issue #81): the writer re-emits the
                            start AND its end from `Paragraph::bookmarks`. */
                            if let Some(id) = &raw_id {
                                toc_ids.insert(id.clone());
                            }
                            if !para_bookmarks.iter().any(|b| b.name == name) {
                                para_bookmarks.push(engine::Bookmark {
                                    name,
                                    id: raw_id.and_then(|v| v.trim().parse().ok()),
                                });
                            }
                        } else if !in_run
                            && !in_ppr
                            && let Some(frag) =
                                slice_fragment(xml, prev_pos, reader.buffer_position() as usize)
                        {
                            /* Issues #199 / #106 — any other bookmark rides
                            the regenerated paragraph as a marker. */
                            markup.marker(para_text.len() as u32, frag, &ns);
                        }
                    }
                    b"w:commentRangeStart" => {
                        if let Some(id) = attr_val(&e, b"w:id").and_then(|v| v.parse().ok()) {
                            let block_idx = out_blocks.len() as u32;
                            let off = (para_text.len() + run_text.len()) as u32;
                            open_comment_ranges.insert(id, (block_idx, off));
                        }
                        /* Issue #120 — between two blocks the marker is
                        body markup the writer never regenerates. */
                        if at_block_level {
                            let end = reader.buffer_position() as usize;
                            if let Some(frag) = slice_fragment(xml, prev_pos, end) {
                                envelopes.push_verbatim(frag);
                            }
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
                        if at_block_level {
                            let end = reader.buffer_position() as usize;
                            if let Some(frag) = slice_fragment(xml, prev_pos, end) {
                                envelopes.push_verbatim(frag);
                            }
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
                            FieldCursor {
                                para_text: &para_text,
                                run_text: &run_text,
                                block_idx: out_blocks.len(),
                                in_table: in_tbl > 0,
                            },
                            &mut para_fields,
                            &mut out_blocks,
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
                        if n == b"w:outlineLvl" {
                            /* Issue #81 — also read it (TOC `\u`); the
                            grab bag stays the writer's source. */
                            apply_ppr(n, &e, &mut direct_ppr);
                        }
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
            Event::Text(t)
                if in_block_container
                    && p_start_byte.is_none()
                    && in_tbl == 0
                    && !in_sect_pr
                    && tail_start.is_none()
                    && t.iter().all(u8::is_ascii_whitespace) =>
            {
                /* Issue #120 — character data between two blocks: the
                whitespace of a pretty-printed part. Anything else there
                is not schema-valid and is dropped as before. */
                let end = reader.buffer_position() as usize;
                if let Some(frag) = slice_fragment(xml, prev_pos, end) {
                    envelopes.push_verbatim(frag);
                }
            }
            Event::Comment(_) | Event::PI(_) | Event::CData(_)
                if in_block_container
                    && p_start_byte.is_none()
                    && in_tbl == 0
                    && !in_sect_pr
                    && tail_start.is_none() =>
            {
                /* Issue #120 — an XML comment / processing instruction
                between two blocks rides the following block verbatim. */
                let end = reader.buffer_position() as usize;
                if let Some(frag) = slice_fragment(xml, prev_pos, end) {
                    envelopes.push_verbatim(frag);
                }
            }
            Event::Text(t)
                if p_start_byte.is_some()
                    && in_tbl == 0
                    && !in_run
                    && !in_ppr
                    && t.iter().all(u8::is_ascii_whitespace) =>
            {
                /* Issues #199 / #106 — pretty-print whitespace between
                paragraph children rides a regenerated paragraph. */
                let end = reader.buffer_position() as usize;
                if let Some(frag) = slice_fragment(xml, prev_pos, end) {
                    markup.whitespace(para_text.len() as u32, frag);
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
                                body_xml: envelopes.take_before(),
                            }));
                            envelopes.note_block_end(tbl_end_byte);
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
                    b"w:rPr" => {
                        in_rpr = false;
                        if in_run {
                            markup.run_rpr_end(xml, reader.buffer_position() as usize, &ns);
                        }
                    }
                    b"w:pPr" => {
                        if in_ppr && !in_run && p_start_byte.is_some() {
                            markup.close_ppr(xml, reader.buffer_position() as usize, &ns);
                        }
                        in_ppr = false;
                    }
                    b"w:numPr" => in_num_pr = false,
                    b"w:pBdr" => in_pbdr = false,
                    b"w:tabs" => in_tabs = false,
                    /* Issue #120 — a block-level container closes: its
                    envelope lands on its first / last inner block (or, with
                    no inner block, rides whole as one verbatim fragment). A
                    run-level `</w:sdt>` (inside a paragraph) never gets
                    here — `p_start_byte` is set. */
                    b"w:sdt" | b"w:customXml"
                        if in_block_container
                            && p_start_byte.is_none()
                            && !in_sect_pr
                            && envelopes.in_container() =>
                    {
                        let end = reader.buffer_position() as usize;
                        envelopes.close_container(xml, end, &mut out_blocks);
                    }
                    /* Issue #112 / #120 — the block container closes: pending
                    markers attach after the last block; for `<w:body>` the
                    tail (`</w:body>` … EOF, or from just after the trailing
                    sectPr) completes the document envelope. */
                    b"w:body" | b"w:hdr" | b"w:ftr" | b"w:footnote" | b"w:endnote"
                    | b"w:comment"
                        if in_block_container && p_start_byte.is_none() && !in_sect_pr =>
                    {
                        envelopes.finish(&mut out_blocks);
                        if name.as_ref() == b"w:body" && root_is_document {
                            let start = tail_start.take().unwrap_or(prev_pos);
                            if start <= xml.len() && valid_document_tail(&xml[start..]) {
                                envelope.tail = xml[start..].to_vec();
                            }
                        }
                        in_block_container = false;
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
                                    span: None,
                                });
                            }
                        }
                    }
                    b"w:hyperlink" => {
                        if let Some((target, start, attrs)) = hyperlink_stack.pop() {
                            let end = (para_text.len() + run_text.len()) as u32;
                            if end > start && !target.is_empty() {
                                para_hyperlinks.push(engine::Hyperlink {
                                    start,
                                    end,
                                    target,
                                    attrs,
                                });
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
                        let mut taken = std::mem::take(&mut cur_sect);
                        /* Issue #112 — the element's own bytes ride the
                        section for the writer's verified passthrough. */
                        let end_byte = reader.buffer_position() as usize;
                        taken.source_xml = sect_pr_start
                            .take()
                            .and_then(|s| slice_element(xml, s, end_byte, b"w:sectPr"));
                        if in_ppr {
                            pending_paragraph_sect = Some(taken);
                        } else {
                            let end = out_blocks.len() as u32;
                            if end > sect_start_block {
                                out_sections.push(taken.into_section(
                                    sect_start_block,
                                    end,
                                    default_page_geometry,
                                ));
                                sect_start_block = end;
                            } else {
                                trailing_sect_without_blocks = Some(taken);
                            }
                            if in_block_container {
                                tail_start = Some(end_byte);
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
                            /* Issues #199 / #106 — a text-less run with only
                            unmodeled content survives as a marker. */
                            markup.close_textless_run(
                                xml,
                                reader.buffer_position() as usize,
                                start,
                                &ns,
                            );
                            /* Issue #120 — keep `prev_pos` current (the
                            loop's tail is skipped): the next event may be
                            a captured element whose slice starts here. */
                            prev_pos = reader.buffer_position() as usize;
                            buf.clear();
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
                        markup.close_text_run(start, end, &style);
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
                        let source_markup = markup.finish(
                            para_text.len() as u32,
                            &props,
                            &style_id_for_paragraph,
                            list_item,
                        );
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
                            bookmarks: std::mem::take(&mut para_bookmarks),
                            /* Issue #120 — the block-level markup pending
                            since the previous block (bookmarks, an sdt
                            opener, whitespace) attaches before this one. */
                            body_xml: envelopes.take_before(),
                            source_markup,
                        }));
                        envelopes.note_block_end(p_end_byte);
                        /* Phase 6 — inline `<w:sectPr>` ends the section at this
                        paragraph. Emit a `Section` covering everything since
                        the last break; the next paragraph starts a fresh
                        section. */
                        if let Some(sect) = pending_paragraph_sect.take() {
                            let end = out_blocks.len() as u32;
                            if end > sect_start_block {
                                out_sections.push(sect.into_section(
                                    sect_start_block,
                                    end,
                                    default_page_geometry,
                                ));
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
        out_sections.push(SectPrAccum::default().into_section(
            sect_start_block,
            total,
            default_page_geometry,
        ));
    }
    /* A part that never closed its block container (truncated input):
    attach what is still pending rather than losing it. */
    if in_block_container {
        envelopes.finish(&mut out_blocks);
    }

    let no_sections = out_sections.is_empty();
    let mut tree = DocumentTree::from_blocks_with_sections(out_blocks, out_sections);
    if no_sections && let Some(acc) = trailing_sect_without_blocks {
        tree.body_section =
            engine::SectionProps::from(&acc.into_section(0, 0, default_page_geometry));
    }
    tree.comment_ranges = out_comment_ranges;
    /* Issue #112 — only a complete envelope (root tag, `<w:body>` tag and
    a validated tail) is worth re-emitting; anything less means the
    writer synthesizes the stock header + footer as before. */
    if envelope.is_captured() && !envelope_invalid {
        tree.document_envelope = envelope;
    }
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
                ..
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
        /* Issue #82 — and is modeled too. */
        assert_eq!(a.wrap_text, engine::WrapText::BothSides);
        assert_eq!(
            a.wrap_polygon.as_deref(),
            Some(&[(0, 0), (0, 21600), (21600, 21600), (21600, 0), (0, 0)][..])
        );
        assert_eq!(
            a.doc_pr_xml.as_deref(),
            Some(doc_pr),
            "docPr children ride verbatim"
        );
    }

    /// Issue #82 — every wrap child parses into typed fields: kind, the
    /// `wrapText` side rule, and (tight / through) the polygon.
    #[test]
    fn anchored_picture_wrap_modes_parse_into_typed_fields() {
        let cases: &[(&str, engine::WrapKind, engine::WrapText, usize)] = &[
            (
                r#"<wp:wrapNone/>"#,
                engine::WrapKind::None,
                engine::WrapText::BothSides,
                0,
            ),
            (
                r#"<wp:wrapSquare wrapText="left"/>"#,
                engine::WrapKind::Square,
                engine::WrapText::Left,
                0,
            ),
            (
                r#"<wp:wrapSquare wrapText="largest"><wp:effectExtent l="0" t="0" r="0" b="0"/></wp:wrapSquare>"#,
                engine::WrapKind::Square,
                engine::WrapText::Largest,
                0,
            ),
            (
                concat!(
                    r#"<wp:wrapThrough wrapText="right"><wp:wrapPolygon edited="1">"#,
                    r#"<wp:start x="10" y="0"/><wp:lineTo x="21600" y="10800"/>"#,
                    r#"<wp:lineTo x="10" y="21600"/></wp:wrapPolygon></wp:wrapThrough>"#
                ),
                engine::WrapKind::Through,
                engine::WrapText::Right,
                3,
            ),
            (
                r#"<wp:wrapTopAndBottom distT="0" distB="0"/>"#,
                engine::WrapKind::TopAndBottom,
                engine::WrapText::BothSides,
                0,
            ),
        ];
        for &(wrap, kind, text, n) in cases {
            let body = format!(
                concat!(
                    r#"<w:p><w:r><w:drawing>"#,
                    r#"<wp:anchor distT="1" distB="2" distL="3" distR="4" simplePos="0" "#,
                    r#"relativeHeight="3" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1">"#,
                    r#"<wp:simplePos x="0" y="0"/>"#,
                    r#"<wp:positionH relativeFrom="column"><wp:posOffset>0</wp:posOffset></wp:positionH>"#,
                    r#"<wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV>"#,
                    r#"<wp:extent cx="914400" cy="457200"/><wp:effectExtent l="0" t="0" r="0" b="0"/>"#,
                    "{wrap}",
                    r#"<wp:docPr id="2" name="P"/>{pic}"#,
                    r#"</wp:anchor></w:drawing></w:r></w:p>"#,
                ),
                wrap = wrap,
                pic = PIC_GRAPHIC
            );
            let tree = parse_body(&body);
            let p = tree.blocks[0].as_paragraph().expect("paragraph");
            let a = p.inline_objects[0].anchor.as_deref().expect("floating");
            assert_eq!(a.wrap, kind, "{wrap}");
            assert_eq!(a.wrap_text, text, "{wrap}");
            assert_eq!(a.wrap_polygon.as_ref().map_or(0, Vec::len), n, "{wrap}");
            assert_eq!(
                (
                    a.dist_top_emu,
                    a.dist_bottom_emu,
                    a.dist_left_emu,
                    a.dist_right_emu
                ),
                (1, 2, 3, 4)
            );
            assert_eq!(a.wrap_xml.as_deref(), Some(wrap));
        }
    }

    /// A `<wp:anchor>` around a shape / text box has no `<a:blip>`: issue
    /// #119 keeps it as an opaque object (its extent + anchor typed, no
    /// picture, the bytes verbatim) instead of dropping it, and the
    /// paragraph text around it survives with exactly one sentinel.
    #[test]
    fn anchored_shape_without_blip_is_preserved_not_misread_as_a_picture() {
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
        assert_eq!(p.text, "a\u{FFFC}b");
        assert_eq!(p.inline_objects.len(), 1);
        let obj = &p.inline_objects[0];
        assert!(
            matches!(&obj.kind, engine::InlineKind::Image { rel_id, width_emu: 100, height_emu: 100, .. } if rel_id.is_empty())
        );
        assert!(obj.anchor.is_some(), "the anchor placement is typed");
        assert!(
            obj.source_xml
                .as_deref()
                .is_some_and(|s| s.starts_with(b"<w:drawing>") && s.ends_with(b"</w:drawing>")),
            "the whole drawing rides the object verbatim"
        );
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
            r#"<wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="100" cy="100"/></a:xfrm>"#,
            r#"<a:solidFill><a:srgbClr val="FFFF00"/></a:solidFill>"#,
            r#"<a:ln w="12700"><a:solidFill><a:srgbClr val="0000FF"/></a:solidFill></a:ln></wps:spPr>"#,
            r#"<wps:txbx><w:txbxContent>"#,
            r#"<w:p><w:r><w:t>inside the box</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>second box line</w:t></w:r></w:p>"#,
            r#"</w:txbxContent></wps:txbx>"#,
            r#"<wps:bodyPr lIns="0" tIns="12700" rIns="0" bIns="0" anchor="ctr"><a:spAutoFit/></wps:bodyPr></wps:wsp>"#,
            r#"</a:graphicData></a:graphic></wp:anchor></w:drawing></w:r>"#,
            r#"<w:r><w:t>b</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>second</w:t></w:r></w:p>"#,
        );
        let tree = parse_body(body);
        assert_eq!(tree.blocks.len(), 2, "only the two body paragraphs");
        let p0 = tree.blocks[0].as_paragraph().expect("paragraph 0");
        assert_eq!(p0.text, "a\u{FFFC}b");
        assert_eq!(p0.inline_objects.len(), 1, "the text box is one object");
        assert!(
            matches!(&p0.inline_objects[0].kind, engine::InlineKind::TextBox { story, .. }
                if story.source_xml.as_deref().is_some_and(|s| s.starts_with("<w:drawing>"))),
            "issue #83 — a story-carrying shape is a text box with its container verbatim"
        );
        let src = p0
            .source_xml
            .as_deref()
            .expect("passthrough capture intact");
        assert!(src.starts_with(b"<w:p>") && src.ends_with(b"</w:p>"));
        let src = std::str::from_utf8(src).unwrap();
        assert!(
            src.contains("inside the box"),
            "the drawing rides the enclosing paragraph's verbatim bytes"
        );
        let p1 = tree.blocks[1].as_paragraph().expect("paragraph 1");
        assert_eq!(p1.text, "second");

        /* Issue #83 — the shape is modeled as a floating text box. */
        assert_eq!(p0.inline_objects.len(), 1);
        let obj = &p0.inline_objects[0];
        assert_eq!(obj.at, 1);
        assert!(obj.anchor.is_some(), "a <wp:anchor> text box floats");
        let engine::InlineKind::TextBox {
            width_emu,
            height_emu,
            story,
        } = &obj.kind
        else {
            panic!("expected a text box, got {:?}", obj.kind);
        };
        assert_eq!((*width_emu, *height_emu), (100, 100));
        let texts: Vec<&str> = story
            .body
            .iter()
            .filter_map(Block::as_paragraph)
            .map(|p| p.text.as_str())
            .collect();
        assert_eq!(texts, ["inside the box", "second box line"]);
        assert_eq!(story.fill, Some([0xff, 0xff, 0, 0xff]));
        assert_eq!(
            story.outline,
            Some(engine::ShapeOutline {
                color: [0, 0, 0xff, 0xff],
                width_emu: 12_700
            })
        );
        assert_eq!(
            (
                story.inset_left_emu,
                story.inset_top_emu,
                story.inset_right_emu,
                story.inset_bottom_emu
            ),
            (0, 12_700, 0, 0)
        );
        assert_eq!(story.v_align, engine::TextBoxVAlign::Center);
        assert!(story.auto_fit);
        assert!(!story.dirty);
        let container = story.source_xml.as_deref().expect("verbatim container");
        assert!(container.starts_with("<w:drawing>") && container.ends_with("</w:drawing>"));
        assert_eq!(story.story_ranges.len(), 1);
        let (s0, e0) = story.story_ranges[0];
        assert!(container[s0 as usize..e0 as usize].starts_with("<w:txbxContent>"));
        let (hs, he) = story.host_range.expect("host range");
        assert_eq!(&src[hs as usize..he as usize], container);
    }

    /// Issue #83 — a bare VML `<w:pict>` text box models its geometry from
    /// the shape's CSS-ish `style` + attributes.
    #[test]
    fn vml_pict_text_box_parses_geometry_and_story() {
        let body = concat!(
            r#"<w:p><w:r><w:t>a</w:t></w:r><w:r><w:pict>"#,
            r##"<v:shape xmlns:v="urn:schemas-microsoft-com:vml" id="s1" type="#_x0000_t202" "##,
            r#"style="position:absolute;margin-left:10pt;margin-top:20pt;width:100pt;height:50pt;z-index:3;mso-position-horizontal-relative:page;v-text-anchor:bottom" "#,
            r##"fillcolor="#ff0000" strokeweight="2pt">"##,
            r#"<v:textbox inset="1pt,2pt,3pt,4pt"><w:txbxContent><w:p><w:r><w:t>vml text</w:t></w:r></w:p></w:txbxContent></v:textbox>"#,
            r#"<w10:wrap xmlns:w10="urn:schemas-microsoft-com:office:word" type="square"/>"#,
            r#"</v:shape></w:pict></w:r></w:p>"#,
        );
        let tree = parse_body(body);
        let p0 = tree.blocks[0].as_paragraph().expect("paragraph 0");
        assert_eq!(p0.text, "a\u{FFFC}");
        let obj = &p0.inline_objects[0];
        let engine::InlineKind::TextBox {
            width_emu,
            height_emu,
            story,
        } = &obj.kind
        else {
            panic!("expected a text box");
        };
        assert_eq!((*width_emu, *height_emu), (1_270_000, 635_000));
        assert_eq!(story.fill, Some([0xff, 0, 0, 0xff]));
        assert_eq!(story.outline.map(|o| o.width_emu), Some(25_400));
        assert_eq!(story.inset_left_emu, 12_700);
        assert_eq!(story.inset_bottom_emu, 50_800);
        assert_eq!(story.v_align, engine::TextBoxVAlign::Bottom);
        let a = obj.anchor.as_deref().expect("absolute ⇒ floating");
        assert_eq!(a.position_h.relative_from, engine::HRelativeFrom::Page);
        assert_eq!(a.position_h.offset, engine::FloatOffset::Emu(127_000));
        assert_eq!(a.position_v.offset, engine::FloatOffset::Emu(254_000));
        assert_eq!(a.wrap, engine::WrapKind::Square);
        assert_eq!(a.relative_height, 3);
        let container = story.source_xml.as_deref().expect("container");
        assert!(container.starts_with("<w:pict>"));
        assert_eq!(story.story_ranges.len(), 1);
    }

    /// Issue #83 self-defense — text boxes nested inside text boxes (Word
    /// never writes them; attacker input can) stop being modeled at the
    /// nesting cap instead of recursing without bound.
    #[test]
    fn nested_text_boxes_stop_at_the_nesting_cap() {
        fn wrap_in_box(inner: &str) -> String {
            format!(
                concat!(
                    r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="10" cy="10"/>"#,
                    r#"<a:graphic><a:graphicData><wps:wsp><wps:txbx><w:txbxContent>{}"#,
                    r#"</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic>"#,
                    r#"</wp:inline></w:drawing></w:r></w:p>"#
                ),
                inner
            )
        }
        let mut body = String::from("<w:p><w:r><w:t>core</w:t></w:r></w:p>");
        for _ in 0..40 {
            body = wrap_in_box(&body);
        }
        let tree = parse_body(&body);
        let mut depth = 0;
        let mut para = tree.blocks[0].as_paragraph().cloned();
        while let Some(p) = para {
            match p.inline_objects.first().map(|o| &o.kind) {
                Some(engine::InlineKind::TextBox { story, .. }) => {
                    depth += 1;
                    para = story.body.first().and_then(Block::as_paragraph).cloned();
                }
                _ => break,
            }
        }
        assert_eq!(depth as u32, crate::parts::textbox::MAX_TEXT_BOX_NESTING);
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
        assert_eq!(p0.text, "a\u{FFFC}b");
        assert_eq!(
            p0.inline_objects.len(),
            1,
            "the AlternateContent is one object"
        );
        assert!(
            matches!(&p0.inline_objects[0].kind, engine::InlineKind::TextBox { story, .. }
                if story.source_xml.as_deref().is_some_and(|s| s.starts_with("<mc:AlternateContent") && s.ends_with("</mc:AlternateContent>"))),
            "choice + fallback ride the text box's container verbatim"
        );
        assert!(p0.source_xml.is_some(), "passthrough capture intact");
        /* Issue #83 — ONE text box (the choice); its container is the
        whole AlternateContent and both story elements are splice
        targets, so an edit rewrites the choice and the VML fallback. */
        assert_eq!(p0.inline_objects.len(), 1);
        let engine::InlineKind::TextBox { story, .. } = &p0.inline_objects[0].kind else {
            panic!("expected a text box");
        };
        let texts: Vec<&str> = story
            .body
            .iter()
            .filter_map(Block::as_paragraph)
            .map(|p| p.text.as_str())
            .collect();
        assert_eq!(texts, ["choice text"]);
        let container = story.source_xml.as_deref().expect("container");
        assert!(container.starts_with("<mc:AlternateContent"));
        assert!(container.ends_with("</mc:AlternateContent>"));
        assert_eq!(story.story_ranges.len(), 2);
        for (s, e) in &story.story_ranges {
            assert!(container[*s as usize..*e as usize].starts_with("<w:txbxContent>"));
        }
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

    /* ---------------------------------------------------------------
    Issues #120 / #112 — block-level passthrough + the document envelope.
    --------------------------------------------------------------- */

    fn parse_full(xml: &str) -> DocumentTree {
        let table = StyleTable::default();
        let resolver = StyleResolver::new(&table);
        parse_document_xml(xml.as_bytes(), &resolver).expect("parse")
    }

    #[test]
    fn self_closing_paragraph_is_a_block_with_its_bytes() {
        let tree = parse_body(
            r#"<w:p><w:r><w:t>a</w:t></w:r></w:p><w:p w:rsidR="00A1"/><w:p><w:r><w:t>c</w:t></w:r></w:p>"#,
        );
        assert_eq!(tree.blocks.len(), 3);
        let p1 = tree.blocks[1].as_paragraph().expect("empty paragraph");
        assert_eq!(p1.text, "");
        assert!(!p1.dirty);
        assert_eq!(
            p1.source_xml.as_deref(),
            Some(br#"<w:p w:rsidR="00A1"/>"#.as_slice())
        );
        assert_eq!(tree.blocks[2].as_paragraph().unwrap().text, "c");
    }

    #[test]
    fn body_level_markup_lands_on_the_neighbouring_blocks() {
        use engine::BodyFragment;
        let tree = parse_body(concat!(
            r#"<w:bookmarkStart w:id="0" w:name="b"/>"#,
            r#"<w:sdt><w:sdtPr><w:rPr><w:b/></w:rPr></w:sdtPr><w:sdtContent>"#,
            r#"<w:p><w:r><w:t>one</w:t></w:r></w:p>"#,
            " ",
            r#"<w:p><w:r><w:t>two</w:t></w:r></w:p>"#,
            r#"</w:sdtContent></w:sdt>"#,
            r#"<w:bookmarkEnd w:id="0"/>"#,
            r#"<w:p><w:r><w:t>three</w:t></w:r></w:p>"#,
            r#"<w:proofErr w:type="gramEnd"/>"#,
        ));
        assert_eq!(tree.blocks.len(), 3);
        let p0 = tree.blocks[0].as_paragraph().unwrap();
        assert_eq!(p0.text, "one");
        assert!(p0.spans.is_empty(), "the sdtPr rPr never leaks into runs");
        let b0 = p0.body_xml.as_deref().expect("markup before block 0");
        assert!(
            matches!(&b0.before[0], BodyFragment::Verbatim { xml } if xml == br#"<w:bookmarkStart w:id="0" w:name="b"/>"#)
        );
        assert!(
            matches!(&b0.before[1], BodyFragment::Open { id: 0, open_xml, close_xml }
            if open_xml == b"<w:sdt><w:sdtPr><w:rPr><w:b/></w:rPr></w:sdtPr><w:sdtContent>"
            && close_xml == b"</w:sdtContent></w:sdt>")
        );
        assert!(b0.after.is_empty());
        let p1 = tree.blocks[1].as_paragraph().unwrap();
        let b1 = p1.body_xml.as_deref().expect("markup around block 1");
        assert!(matches!(&b1.before[..], [BodyFragment::Verbatim { xml }] if xml == b" "));
        assert!(matches!(&b1.after[..], [BodyFragment::Close { id: 0 }]));
        /* A marker between two blocks attaches BEFORE the following block;
        only markup after the last block attaches after it. */
        let p2 = tree.blocks[2].as_paragraph().unwrap();
        let b2 = p2
            .body_xml
            .as_deref()
            .expect("markup around the last block");
        assert!(
            matches!(&b2.before[..], [BodyFragment::Verbatim { xml }] if xml == br#"<w:bookmarkEnd w:id="0"/>"#)
        );
        assert!(
            matches!(&b2.after[..], [BodyFragment::Verbatim { xml }] if xml == br#"<w:proofErr w:type="gramEnd"/>"#)
        );
    }

    #[test]
    fn document_envelope_and_section_source_are_captured_verbatim() {
        let xml = concat!(
            "\u{FEFF}<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n",
            r#"<w:document xmlns:mc="urn:mc" xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" mc:Ignorable="w14">"#,
            "\r\n<w:body>",
            r#"<w:p><w:r><w:t>x</w:t></w:r></w:p>"#,
            r#"<w:sectPr w:rsidR="00B4"><w:pgSz w:w="11906" w:h="16838"/><w:docGrid w:linePitch="360"/></w:sectPr>"#,
            "\r\n</w:body>\r\n</w:document>\r\n",
        );
        let tree = parse_full(xml);
        let env = &tree.document_envelope;
        assert!(env.is_captured());
        assert_eq!(
            env.prolog,
            "\u{FEFF}<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n".as_bytes()
        );
        assert!(
            env.root_tag.starts_with(b"<w:document xmlns:mc=")
                && env.root_tag.ends_with(b"mc:Ignorable=\"w14\">")
        );
        assert_eq!(env.root_to_body, b"\r\n");
        assert_eq!(env.body_tag, b"<w:body>");
        assert_eq!(env.tail, b"\r\n</w:body>\r\n</w:document>\r\n");
        let src = tree
            .body_section
            .source_xml
            .as_deref()
            .expect("trailing sectPr bytes");
        assert!(src.starts_with(b"<w:sectPr w:rsidR=") && src.ends_with(b"</w:sectPr>"));
        /* The re-parse used by the writer's verified passthrough agrees
        with what the body parser lowered. */
        assert_eq!(
            parse_sect_pr_fragment(src, PageGeometry::a4()),
            tree.body_section.without_source()
        );
        /* An engine-authored tree has no envelope. */
        assert!(!DocumentTree::from_text("x").document_envelope.is_captured());
        /* A tail that is not `</w:body></w:document>` is refused. */
        assert!(valid_document_tail(b"\n</w:body >\n</w:document>\n"));
        assert!(!valid_document_tail(b"<w:p/></w:body></w:document>"));
        assert!(!valid_document_tail(b"</w:body></w:document><!-- x -->"));
    }

    /// Issue #119 — a text box (root-bound `wps`) is an object with its
    /// extent, no picture and its verbatim bytes; a VML picture inside a
    /// run is an object too (no longer skipped).
    #[test]
    fn drawing_objects_keep_their_source_and_extent() {
        let root = concat!(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
            r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
            r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
            r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
            r#"xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" "#,
            r#"xmlns:v="urn:schemas-microsoft-com:vml">"#,
        );
        let text_box = concat!(
            r#"<w:drawing><wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="1" "#,
            r#"behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/>"#,
            r#"<wp:positionH relativeFrom="column"><wp:posOffset>0</wp:posOffset></wp:positionH>"#,
            r#"<wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV>"#,
            r#"<wp:extent cx="100" cy="200"/><wp:wrapSquare wrapText="bothSides"/><wp:docPr id="1" name="Text Box 1"/>"#,
            r#"<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">"#,
            r#"<wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>inside</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp>"#,
            r#"</a:graphicData></a:graphic></wp:anchor></w:drawing>"#,
        );
        let pict = r#"<w:pict><v:shape id="i1" style="width:72pt;height:36pt"><v:imagedata r:id="rId8"/></v:shape></w:pict>"#;
        let xml = format!(
            r#"{root}<w:body><w:p><w:r><w:t>a</w:t></w:r><w:r>{text_box}</w:r><w:r>{pict}</w:r><w:r><w:t>b</w:t></w:r></w:p><w:p><w:r><w:t>second</w:t></w:r></w:p></w:body></w:document>"#
        );
        let tree = parse_full(&xml);
        assert_eq!(tree.blocks.len(), 2, "the text box story is not hoisted");
        let p = tree.blocks[0].as_paragraph().unwrap();
        assert_eq!(p.text, "a\u{FFFC}\u{FFFC}b");
        assert_eq!(p.inline_objects.len(), 2);
        let tb = &p.inline_objects[0];
        /* Issue #83 — a story-carrying shape is a text box; its container
        is the whole drawing, verbatim. */
        assert!(
            matches!(&tb.kind, engine::InlineKind::TextBox { width_emu: 100, height_emu: 200, story }
                if story.source_xml.as_deref() == Some(text_box) && story.body.len() == 1),
            "{:?}",
            tb.kind
        );
        let a = tb.anchor.as_deref().expect("floating text box");
        assert_eq!(a.wrap, engine::WrapKind::Square);
        let vml = &p.inline_objects[1];
        assert!(
            matches!(&vml.kind, engine::InlineKind::Image { rel_id, width_emu: 914_400, height_emu: 457_200, .. } if rel_id == "rId8")
        );
        assert!(vml.anchor.is_none());
        assert_eq!(vml.source_xml.as_deref(), Some(pict.as_bytes()));
        assert_eq!(tree.blocks[1].as_paragraph().unwrap().text, "second");
    }
}
