//! `read_docx` output → a laid-out `Vec<PageBox>`, native (no wasm, no
//! browser). This is a deliberately smaller pipeline than
//! `crates/engine-wasm`'s interactive one: it re-derives page geometry,
//! resolved paragraph properties, style spans and list markers from the
//! `engine::DocumentTree` and drives `layout::layout_paragraph` +
//! `layout::Paginator` directly, the same building blocks engine-wasm uses.
//!
//! Deliberately out of scope (documented in `tools/differential/README.md`
//! rather than silently approximated away):
//!
//! - headers / footers, PAGE / NUMPAGES / DATE field evaluation, footnotes,
//!   inline images, hyperlinks, tracked-change overlays, multi-column
//!   sections, and custom `<w:tabs>` stops.
//! - `<w:pageBreakBefore/>` — engine-wasm itself does not honour it during
//!   pagination (only `<w:br w:type="page"/>` FORM FEED does), and this
//!   pipeline mirrors that real behaviour rather than an idealized one.
//! - table cell vertical alignment (always top) and vertical-merge height
//!   synchronization across rows (a `Restart` cell's height comes only from
//!   its own row; `Continue` cells render no content).
//!
//! None of this is copied from or informed by `/data/code/reference/` — it
//! re-derives the same public `layout` / `text-pipeline` APIs `engine-wasm`
//! (this repo's own code) already calls.

use engine::{
    Alignment as EngineAlignment, Block, CellMargins, DocumentTree, Indent, ParaProperties,
    Paragraph, SpanStyle, Table, TextDirection,
};
use layout::{
    LayoutBlock, Margins, PageBox, PaginatePageGeometry as LayoutPageGeometry, Paginator,
    ParagraphBox, ParagraphConfig, Point, Size, StyleSpan, TableBox, TableCellBox, TableRowBox,
    layout_paragraph,
};
use text_pipeline::{
    Alignment as TpAlignment, FontStack, ShapingDirection, first_strong_direction,
};

/// Fallback body font size (pt) when no style / span sets one. Matches the
/// `<w:docDefaults>` value most `.docx` templates ship (10-12pt); 12pt is
/// also the seed size `format-pdf`'s own `export_profiles` example uses.
pub const DEFAULT_FONT_SIZE_PT: f32 = 12.0;

/// Approximate "single spacing" line height at the default font size —
/// mirrors engine-wasm's `RenderConfig.line_height` default (a document-wide
/// constant derived from the seed font size at ~1.15x).
const DEFAULT_LINE_HEIGHT_PT: f32 = DEFAULT_FONT_SIZE_PT * 1.15;

const TWIPS_PER_PT: f32 = 20.0;

fn twips_to_pt(t: i32) -> f32 {
    t as f32 / TWIPS_PER_PT
}

/// Output of [`build_pages`]: the laid-out page tree plus the flat
/// per-paragraph source-text table `format_pdf::export_pdf` needs (indexed
/// by `ParagraphBox::source_paragraph_id`).
pub struct BuiltDoc {
    pub pages: Vec<PageBox>,
    pub para_texts: Vec<String>,
}

/// Lay out every section of `doc` onto pages. Takes `doc` by `&mut` only to
/// resolve list markers in place (`engine::numbering::resolve_markers_in_place`
/// — the reader does not do this; engine-wasm calls it once after load).
pub fn build_pages(doc: &mut DocumentTree, fonts: &FontStack) -> BuiltDoc {
    resolve_list_markers(doc);

    let mut para_texts: Vec<String> = Vec::new();
    let mut next_id: u32 = 0;
    let mut pages: Vec<PageBox> = Vec::new();

    for section in doc.effective_sections() {
        let geometry = to_layout_geometry(&section.geometry);
        let mut pag = Paginator::with_default_bands(geometry, None, None);
        let content_width = pag.content_width();
        for idx in section.start_block..section.end_block {
            let Some(block) = doc.blocks.get(idx as usize) else {
                continue;
            };
            let lb = build_layout_block(
                block,
                doc,
                fonts,
                content_width,
                &mut para_texts,
                &mut next_id,
            );
            let (before, after) = block_spacing(block, doc);
            pag.push_block(lb, before, after);
        }
        pages.extend(pag.finish());
    }

    if pages.is_empty() {
        let geometry = to_layout_geometry(&engine::PageGeometry::default());
        let pag = Paginator::with_default_bands(geometry, None, None);
        pages = pag.finish();
    }

    BuiltDoc { pages, para_texts }
}

fn resolve_list_markers(doc: &mut DocumentTree) {
    // Clone first — `doc.blocks.iter_mut()` below needs an exclusive borrow
    // of `doc`, which would otherwise conflict with an immutable borrow of
    // `doc.numbering` held across the call.
    let numbering = doc.numbering.clone();
    let mut refs: Vec<&mut Paragraph> = doc
        .blocks
        .iter_mut()
        .filter_map(Block::as_paragraph_mut)
        .collect();
    engine::numbering::resolve_markers_in_place(&mut refs, &numbering);
}

fn to_layout_geometry(g: &engine::PageGeometry) -> LayoutPageGeometry {
    LayoutPageGeometry {
        width: g.width,
        height: g.height,
        margins: Margins {
            top: g.margin_top,
            right: g.margin_right,
            bottom: g.margin_bottom,
            left: g.margin_left,
        },
        header_offset: g.header_offset,
        footer_offset: g.footer_offset,
    }
}

/// `style_cascade(style_id) ∪ direct_overrides` — the documented resolved
/// view (`ParaProperties` doc comment / Sprint 12 (#11)). Computed fresh
/// rather than trusting `Paragraph::props`, since the reader does not
/// guarantee that field already reflects the cascade at parse time.
fn resolved_props(doc: &DocumentTree, p: &Paragraph) -> ParaProperties {
    doc.resolve_style_cascade(p.style_id.as_deref())
        .merged_with(p.direct_overrides.clone())
}

fn map_alignment(a: EngineAlignment) -> TpAlignment {
    match a {
        EngineAlignment::Start => TpAlignment::Start,
        EngineAlignment::End => TpAlignment::End,
        EngineAlignment::Center => TpAlignment::Center,
        EngineAlignment::Justify => TpAlignment::Justify,
    }
}

fn resolve_direction(text: &str, props: &ParaProperties) -> ShapingDirection {
    match props.direction {
        Some(TextDirection::Rtl) => ShapingDirection::Rtl,
        Some(TextDirection::Ltr) => ShapingDirection::Ltr,
        None => first_strong_direction(text).unwrap_or(ShapingDirection::Ltr),
    }
}

/// Issue #25's `(px, exact)` line-height resolver, re-derived here against a
/// fixed document-wide default (this pipeline has no per-document
/// `RenderConfig`). See the module docs for why re-deriving instead of
/// depending on `engine-wasm` is the right call.
fn resolve_line_height(line_height: Option<engine::LineHeight>) -> (f32, bool) {
    match line_height {
        None => (DEFAULT_LINE_HEIGHT_PT, false),
        Some(engine::LineHeight::Auto { twips }) => {
            (DEFAULT_LINE_HEIGHT_PT * (twips as f32 / 240.0), false)
        }
        Some(engine::LineHeight::Exact { twips }) => (twips_to_pt(twips), true),
        Some(engine::LineHeight::AtLeast { twips }) => (twips_to_pt(twips), false),
    }
}

fn span_from_style(style: &SpanStyle, start: u32, end: u32) -> StyleSpan {
    StyleSpan {
        start,
        end,
        px_size: style.font_size.unwrap_or(DEFAULT_FONT_SIZE_PT),
        color: style.color.unwrap_or([0, 0, 0, 255]),
        bold: style.bold.unwrap_or(false),
        italic: style.italic.unwrap_or(false),
        underline: style.underline.unwrap_or(engine::UnderlineStyle::None),
        strike: style.strike.unwrap_or(false),
        bg_color: style.bg_color,
        font_family: style.font_family.as_ref().map(|f| f.id().to_string()),
        caps_transform: style.caps.unwrap_or(false),
        baseline_shift_px: 0.0,
    }
}

/// Fold `style_run_defaults → pStyle-chain <w:rPr> → direct span formatting`
/// (issue #29's run cascade) into a gap-free `Vec<StyleSpan>` covering
/// `[0, text.len())`. `Paragraph::spans` never carries the cascade baked in
/// (`engine-wasm`'s `build_style_spans` does this at layout time) — a plain
/// per-span read would silently drop any inherited style.
fn build_style_spans(p: &Paragraph, doc: &DocumentTree) -> Vec<StyleSpan> {
    let base = doc.resolve_style_run_cascade(p.style_id.as_deref());
    let text_len = p.text.len() as u32;
    let mut spans = p.spans.clone();
    spans.sort_by_key(|s| s.start);

    let mut out = Vec::with_capacity(spans.len() + 1);
    let mut cursor = 0u32;
    for run in &spans {
        let start = run.start.min(text_len).max(cursor);
        let end = run.end.min(text_len);
        if end <= start {
            continue;
        }
        if start > cursor {
            out.push(span_from_style(&base, cursor, start));
        }
        let merged = base.clone().merged_with(run.style.clone());
        out.push(span_from_style(&merged, start, end));
        cursor = end;
    }
    if cursor < text_len || out.is_empty() {
        out.push(span_from_style(&base, cursor, text_len));
    }
    out
}

fn build_paragraph_box(
    p: &Paragraph,
    doc: &DocumentTree,
    fonts: &FontStack,
    max_width: f32,
    para_texts: &mut Vec<String>,
    next_id: &mut u32,
) -> ParagraphBox {
    let props = resolved_props(doc, p);
    let spans = build_style_spans(p, doc);
    let alignment = props
        .alignment
        .map(map_alignment)
        .unwrap_or(TpAlignment::Start);
    let direction = resolve_direction(&p.text, &props);
    let (line_height, line_height_exact) = resolve_line_height(props.line_height);

    // Issue #50 — a list paragraph with no direct `<w:ind>` falls back to
    // the numbering level's indent (the resolver stamped it alongside the
    // marker text).
    let indent: Indent = if props.indent != Indent::default() {
        props.indent
    } else {
        p.resolved_list_indent.unwrap_or_default()
    };

    let marker_px = spans
        .first()
        .map(|s| s.px_size)
        .unwrap_or(DEFAULT_FONT_SIZE_PT);

    let cfg = ParagraphConfig {
        text: &p.text,
        fonts,
        inline_objects: &[],
        spans: &spans,
        base_direction: direction,
        max_width,
        line_height,
        line_height_exact,
        alignment,
        indent_start_px: twips_to_pt(indent.start_twips),
        indent_end_px: twips_to_pt(indent.end_twips),
        first_line_indent_px: twips_to_pt(indent.first_line_twips),
        hanging_indent_px: twips_to_pt(indent.hanging_twips),
        marker_text: p.resolved_marker.clone(),
        px_size_for_marker: marker_px,
        tab_stops_px: &[],
    };

    let mut pbox = layout_paragraph(cfg);
    pbox.source_paragraph_id = *next_id;
    *next_id += 1;
    para_texts.push(p.text.clone());

    // Phase 2 audit (gap A.12) mirror — a `\u{000C}` FORM FEED (the
    // reader's `<w:br w:type="page"/>` mapping) forces a page flush after
    // its containing line, same as engine-wasm's `compute_page_break_lines`.
    pbox.page_break_after_line = compute_page_break_lines(&p.text, &pbox);
    pbox.borders = props.borders.clone();
    pbox.shading = props.shading;
    pbox
}

fn compute_page_break_lines(text: &str, para_box: &ParagraphBox) -> Vec<usize> {
    let ff_positions: Vec<usize> = text
        .char_indices()
        .filter_map(|(i, c)| (c == '\u{000C}').then_some(i))
        .collect();
    if ff_positions.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<usize> = Vec::new();
    for (line_idx, line) in para_box.lines.iter().enumerate() {
        for run in &line.runs {
            let lo = run.source_range.start as usize;
            let hi = run.source_range.end as usize;
            for &pos in &ff_positions {
                if pos >= lo && pos < hi && !out.contains(&line_idx) {
                    out.push(line_idx);
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn block_spacing(block: &Block, doc: &DocumentTree) -> (f32, f32) {
    match block {
        Block::Paragraph(p) => {
            let props = resolved_props(doc, p);
            (
                twips_to_pt(props.spacing.before_twips),
                twips_to_pt(props.spacing.after_twips),
            )
        }
        Block::Table(_) => (0.0, 0.0),
    }
}

fn build_layout_block(
    block: &Block,
    doc: &DocumentTree,
    fonts: &FontStack,
    avail_width: f32,
    para_texts: &mut Vec<String>,
    next_id: &mut u32,
) -> LayoutBlock {
    match block {
        Block::Paragraph(p) => LayoutBlock::Paragraph(build_paragraph_box(
            p,
            doc,
            fonts,
            avail_width,
            para_texts,
            next_id,
        )),
        Block::Table(t) => LayoutBlock::Table(build_table_box(
            t,
            doc,
            fonts,
            avail_width,
            para_texts,
            next_id,
        )),
    }
}

fn compute_col_count(t: &Table) -> usize {
    if !t.grid.is_empty() {
        return t.grid.len();
    }
    t.rows
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .map(|c| c.props.grid_span.max(1) as usize)
                .sum::<usize>()
        })
        .max()
        .unwrap_or(1)
}

fn compute_col_widths_pt(t: &Table, col_count: usize, avail_width: f32) -> Vec<f32> {
    if !t.grid.is_empty() {
        let widths: Vec<f32> = t.grid.iter().map(|w| twips_to_pt(*w)).collect();
        let sum: f32 = widths.iter().sum();
        // `<w:tblGrid>` occasionally overruns the section's content width
        // (a table authored for a wider page, or twips-vs-pt rounding);
        // scale down proportionally rather than let the table overflow the
        // page — Word's own "Autofit to Window" does the equivalent.
        if sum > avail_width && sum > 0.0 {
            let scale = avail_width / sum;
            return widths.into_iter().map(|w| w * scale).collect();
        }
        return widths;
    }
    let n = col_count.max(1) as f32;
    vec![avail_width / n; col_count.max(1)]
}

fn prefix_sums(widths: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(widths.len());
    let mut acc = 0.0f32;
    for w in widths {
        out.push(acc);
        acc += w;
    }
    out
}

fn build_table_box(
    t: &Table,
    doc: &DocumentTree,
    fonts: &FontStack,
    avail_width: f32,
    para_texts: &mut Vec<String>,
    next_id: &mut u32,
) -> TableBox {
    let col_count = compute_col_count(t);
    let col_widths = compute_col_widths_pt(t, col_count, avail_width);
    let col_x = prefix_sums(&col_widths);
    let total_width: f32 = col_widths.iter().sum();

    let mut rows_out: Vec<TableRowBox> = Vec::with_capacity(t.rows.len());
    let mut y = 0.0f32;
    for row in &t.rows {
        let mut col_idx: usize = 0;
        let mut cells_out: Vec<TableCellBox> = Vec::with_capacity(row.cells.len());
        let mut row_height = 0.0f32;

        for cell in &row.cells {
            let span = (cell.props.grid_span.max(1) as usize)
                .min(col_widths.len().saturating_sub(col_idx).max(1));
            let width_pt: f32 = col_widths[col_idx..(col_idx + span).min(col_widths.len())]
                .iter()
                .sum();
            let margins =
                CellMargins::resolve_edges(cell.props.cell_margins.as_ref(), &t.props.cell_margins);
            let pad_l = twips_to_pt(margins.left_twips);
            let pad_r = twips_to_pt(margins.right_twips);
            let pad_t = twips_to_pt(margins.top_twips);
            let pad_b = twips_to_pt(margins.bottom_twips);
            let content_w = (width_pt - pad_l - pad_r).max(0.0);

            let mut content_blocks: Vec<LayoutBlock> = Vec::new();
            let mut cy = 0.0f32;
            // A `Continue` (vertical-merge) cell renders no content of its
            // own — the `Restart` cell above visually owns the block. Full
            // cross-row height synchronization is a documented limitation
            // (module docs).
            if !matches!(cell.props.v_merge, engine::VMergeRole::Continue) {
                for b in &cell.blocks {
                    let mut lb = build_layout_block(b, doc, fonts, content_w, para_texts, next_id);
                    let mut origin = lb.origin();
                    origin.x = pad_l;
                    origin.y = cy + pad_t;
                    lb.set_origin(origin);
                    cy += lb.size().height;
                    content_blocks.push(lb);
                }
            }
            let cell_h = cy + pad_t + pad_b;
            row_height = row_height.max(cell_h);

            cells_out.push(TableCellBox {
                origin: Point {
                    x: col_x.get(col_idx).copied().unwrap_or(total_width),
                    y: 0.0,
                },
                size: Size {
                    width: width_pt,
                    height: 0.0, // patched below once `row_height` is final
                },
                grid_span: span as u8,
                v_merge: cell.props.v_merge,
                borders: cell.props.borders.clone().unwrap_or_default(),
                shading: cell.props.shading,
                content: content_blocks,
                padding_left: pad_l,
                padding_top: pad_t,
                padding_right: pad_r,
                padding_bottom: pad_b,
                content_offset: 0,
            });
            col_idx += span;
        }

        for cell_out in cells_out.iter_mut() {
            cell_out.size.height = row_height;
        }

        rows_out.push(TableRowBox {
            origin: Point { x: 0.0, y },
            size: Size {
                width: total_width,
                height: row_height,
            },
            cells: cells_out,
            header: row.props.header,
            cant_split: row.props.cant_split,
            source_row: rows_out.len() as u32,
        });
        y += row_height;
    }

    TableBox {
        origin: Point::default(),
        size: Size {
            width: total_width,
            height: y,
        },
        columns: col_widths,
        rows: rows_out,
        outer_borders: t.props.borders.clone().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use text_pipeline::LoadedFont;

    fn liberation_stack() -> FontStack {
        let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
        let face = LoadedFont::parse("liberation".into(), bytes).expect("parse font");
        let mut faces: std::collections::HashMap<String, std::sync::Arc<LoadedFont>> =
            std::collections::HashMap::new();
        faces.insert("liberation".to_string(), std::sync::Arc::new(face));
        FontStack::from_faces(faces, "liberation")
    }

    #[test]
    fn single_paragraph_yields_one_page() {
        let mut doc = DocumentTree::from_text("hello world");
        let fonts = liberation_stack();
        let built = build_pages(&mut doc, &fonts);
        assert_eq!(built.pages.len(), 1);
        assert_eq!(built.para_texts, vec!["hello world".to_string()]);
        let para = built.pages[0]
            .blocks
            .first()
            .and_then(LayoutBlock::as_paragraph)
            .expect("first block is a paragraph");
        assert_eq!(para.source_paragraph_id, 0);
        assert!(!para.lines.is_empty());
    }

    #[test]
    fn overflowing_text_paginates_into_multiple_pages() {
        // 40 paragraphs of filler text should overflow a single A4 page at
        // the default 12pt body size.
        let filler = "The quick brown fox jumps over the lazy dog. ".repeat(6);
        let texts = (0..40).map(|i| format!("Paragraph {i}: {filler}"));
        let mut doc = DocumentTree::from_paragraphs(texts);
        let fonts = liberation_stack();
        let built = build_pages(&mut doc, &fonts);
        assert!(
            built.pages.len() > 1,
            "expected multiple pages, got {}",
            built.pages.len()
        );
        assert_eq!(built.para_texts.len(), 40);
    }
}
