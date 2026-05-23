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

use crate::error::DocxError;
use crate::parts::table::parse_table_bytes;
use crate::schema::ct_ppr::apply_ppr;
use crate::schema::ct_rpr::{apply_rpr, attr_val};
use crate::style_resolver::StyleResolver;
use engine::{
    Block, DocumentTree, ListItem, PageGeometry, ParaProperties, Paragraph, Section, SpanStyle,
    StyleRun, Table,
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
    header_ref: Option<String>,
    footer_ref: Option<String>,
}

impl SectPrAccum {
    /// Apply one `<w:sectPr>` child element. Both `Empty(...)` and the
    /// `Start(...)` of `<w:headerReference>...</w:headerReference>`-style tags
    /// route here — every child the parser cares about is leaf-shaped.
    fn apply(&mut self, name: &[u8], e: &BytesStart) {
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
                self.header_ref = attr_val(e, b"r:id");
            }
            b"w:footerReference" => {
                self.footer_ref = attr_val(e, b"r:id");
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
/// engine as fully-resolved flat properties.
pub fn parse_document_xml(
    xml: &[u8],
    resolver: &StyleResolver<'_>,
) -> Result<DocumentTree, DocxError> {
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

    /* Phase 8a — footnote refs auto-number 1, 2, 3 ... in document order
    across every `<w:footnoteReference>` the body uses. */
    let mut footnote_display_counter: u32 = 0;

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

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                let name = e.name();
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
                    b"w:rPr" => in_rpr = true,
                    /* A `<w:pPr>` only counts when it's the paragraph's own
                    properties — not a nested element under a `<w:r>`. */
                    b"w:pPr" if !in_run => in_ppr = true,
                    b"w:numPr" if in_ppr => in_num_pr = true,
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
                    n if in_run && in_rpr => apply_rpr(n, &e, &mut direct_rpr),
                    n if in_ppr && in_rpr => {
                        /* Paragraph-mark `<w:pPr>/<w:rPr>` — applies to the
                        ¶ glyph. We keep it as `pmark_rpr` for round-trip
                        and lay it under the run baseline below. */
                        apply_rpr(n, &e, &mut pmark_rpr);
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
                    b"w:footnoteReference" => {
                        /* Phase 8a — inject U+FFFC at the run's current
                        byte offset and queue a FootnoteRef inline
                        object. Numbering counts up across the body in
                        document order, independent of the OOXML `w:id`
                        the file uses internally. */
                        if let Some(id) = attr_val(&e, b"w:id").and_then(|v| v.parse().ok()) {
                            footnote_display_counter += 1;
                            let at = (para_text.len() + run_text.len()) as u32;
                            run_text.push('\u{FFFC}');
                            para_inline_objects.push(engine::InlineObject {
                                at,
                                kind: engine::InlineKind::FootnoteRef {
                                    id,
                                    display_number: footnote_display_counter,
                                },
                            });
                        }
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
                    n if in_sect_pr => cur_sect.apply(n, &e),
                    n if in_run && in_rpr => apply_rpr(n, &e, &mut direct_rpr),
                    n if in_ppr && in_rpr => apply_rpr(n, &e, &mut pmark_rpr),
                    n if in_ppr && !in_rpr && !in_num_pr => {
                        apply_ppr(n, &e, &mut direct_ppr);
                    }
                    _ => {}
                }
            }
            Event::Text(t) if in_text_elt && in_tbl == 0 => {
                run_text.push_str(&t.unescape()?);
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
                                .filter(|&s| s < tbl_end_byte && tbl_end_byte <= xml.len())
                                .map(|s| xml[s..tbl_end_byte].to_vec());
                            /* Phase 5 PR 2 — full row/cell parse via
                            `parts::table::parse_table_bytes`. Source bytes
                            still ride the passthrough so the writer is
                            byte-stable for unedited tables. */
                            let (grid, props, rows) = source_xml
                                .as_deref()
                                .map(|b| parse_table_bytes(b).unwrap_or_default())
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
                    b"w:rPr" => in_rpr = false,
                    b"w:pPr" => in_ppr = false,
                    b"w:numPr" => in_num_pr = false,
                    b"wp:inline" => {
                        in_wp_inline = false;
                    }
                    b"w:drawing" => {
                        if in_drawing
                            && in_wp_inline
                            && let Some(rid) = cur_drawing_rel_id.take()
                            && let (Some(cx), Some(cy)) =
                                (cur_drawing_cx.take(), cur_drawing_cy.take())
                        {
                            /* Inject U+FFFC as the inline object's anchor
                            character. Position in the eventual `para_text` =
                            `para_text.len()` (already flushed runs) +
                            `run_text.len()` (this run's text so far). */
                            let at = (para_text.len() + run_text.len()) as u32;
                            run_text.push('\u{FFFC}');
                            para_inline_objects.push(engine::InlineObject {
                                at,
                                kind: engine::InlineKind::Image {
                                    rel_id: rid,
                                    width_emu: cx,
                                    height_emu: cy,
                                },
                            });
                        } else {
                            /* Anchor / floating or malformed — drop. */
                            cur_drawing_rel_id = None;
                            cur_drawing_cx = None;
                            cur_drawing_cy = None;
                        }
                        in_drawing = false;
                        in_wp_inline = false;
                    }
                    b"w:hyperlink" => {
                        if let Some((target, start)) = hyperlink_stack.pop() {
                            let end = (para_text.len() + run_text.len()) as u32;
                            if end > start {
                                para_hyperlinks.push(engine::Hyperlink { start, end, target });
                            }
                        }
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
                                out_sections.push(Section {
                                    geometry: taken.clone().into_geometry(),
                                    start_block: sect_start_block,
                                    end_block: end,
                                    header_ref: taken.header_ref,
                                    footer_ref: taken.footer_ref,
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
                        if start == end {
                            continue;
                        }
                        /* Run cascade: paragraph baseline (doc defaults + pStyle
                        chain) + paragraph-mark rPr is already baked into the
                        baseline below per-paragraph; here we only fold the
                        character style chain + direct rPr. */
                        let (_, baseline) = resolver.resolve_paragraph(
                            p_style_id.as_deref(),
                            direct_ppr.clone(),
                            pmark_rpr,
                        );
                        let style =
                            resolver.resolve_run(baseline, r_style_id.as_deref(), direct_rpr);
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
                            .filter(|&s| s < p_end_byte && p_end_byte <= xml.len())
                            .map(|s| xml[s..p_end_byte].to_vec());

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
                        treats absent `<w:ilvl>` as level 0. */
                        let list_item = match (list_num_id.take(), list_ilvl.take()) {
                            (Some(num_id), ilvl) => Some(ListItem {
                                num_id,
                                ilvl: ilvl.unwrap_or(0),
                            }),
                            (None, _) => None,
                        };
                        out_blocks.push(Block::Paragraph(Paragraph {
                            text: std::mem::take(&mut para_text),
                            spans: std::mem::take(&mut spans),
                            props,
                            list_item,
                            /* Resolver fills this in a second pass once the full
                            doc order is known (see `opc::archive::read_docx`). */
                            resolved_marker: None,
                            dirty: false,
                            source_xml,
                            inline_objects: std::mem::take(&mut para_inline_objects),
                            hyperlinks: std::mem::take(&mut para_hyperlinks),
                        }));
                        /* Phase 6 — inline `<w:sectPr>` ends the section at this
                        paragraph. Emit a `Section` covering everything since
                        the last break; the next paragraph starts a fresh
                        section. */
                        if let Some(sect) = pending_paragraph_sect.take() {
                            let end = out_blocks.len() as u32;
                            if end > sect_start_block {
                                let header_ref = sect.header_ref.clone();
                                let footer_ref = sect.footer_ref.clone();
                                out_sections.push(Section {
                                    geometry: sect.into_geometry(),
                                    start_block: sect_start_block,
                                    end_block: end,
                                    header_ref,
                                    footer_ref,
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
            header_ref: None,
            footer_ref: None,
        });
    }

    let mut tree = DocumentTree::from_blocks_with_sections(out_blocks, out_sections);
    tree.comment_ranges = out_comment_ranges;
    Ok(tree)
}
