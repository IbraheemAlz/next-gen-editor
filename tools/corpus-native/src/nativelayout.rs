//! Native document -> page-box layout, for stress-testing `crates/layout`
//! against real documents without a browser (issue #88's architecture
//! directive: `read_docx` -> full layout -> PDF export, all native — the
//! Canvas2D backend is browser-only, so it cannot be part of this pipeline).
//!
//! `crates/engine-wasm`'s `Engine::build_pages` is the production
//! orchestration this mirrors, but it is a private method on a
//! `#[wasm_bindgen]` struct that can only be constructed from a real
//! `web_sys::OffscreenCanvas` (`Engine::new`) — unavailable natively — and
//! its supporting free functions (`build_style_spans`, `layout_table_box`,
//! `build_header_footer_box`, …) are crate-private. This module is a
//! deliberately reduced re-implementation on top of the same *public*
//! `engine` / `layout` / `text-pipeline` primitives, scoped down for what a
//! crash-hunting corpus harness needs:
//!
//! - **Paragraphs get full, real layout** — `layout::layout_paragraph` with
//!   the paragraph's resolved alignment / indent / spacing / direction /
//!   style spans. This is the code path that matters most for issue #88:
//!   it is where BiDi, line-breaking, shaping and Kashida justification
//!   panics would surface.
//! - **Tables are flattened**, not laid out as a grid: every cell's blocks
//!   (recursing through nested tables) are pushed as ordinary paragraphs in
//!   row-major, left-to-right cell order, each at the section's full content
//!   width. `layout::TableBox` / column-width distribution /
//!   `layout_table_box` is production-only code this harness does not
//!   drive. This still exercises paragraph layout on every cell's real text
//!   and keeps that text in the page flow (so page counts move sensibly for
//!   table-heavy documents), it just does not reproduce Word's grid
//!   geometry — acceptable because issue #88 explicitly excludes visual
//!   correctness ("that is the differential oracle epic"). Flagged as a
//!   follow-up gap in the harness's own findings (see the corpus-native
//!   report).
//! - **Headers / footers / footnotes / multi-column sections / continuous
//!   section merging** are not built — every section starts a fresh page
//!   (the `NextPage` behaviour), banded content is skipped. Section
//!   *geometry* (page size, margins) IS honoured via
//!   `DocumentTree::effective_sections`, so pagination still reflects the
//!   document's real page size per section.
//!
//! None of this reduced fidelity affects the acceptance checks the corpus
//! harness runs (no panic, sibling-byte-identity, `document.xml` stability,
//! plain-text equality, stable page count across a reopen) — the SAME
//! reduced pipeline runs on both the freshly-parsed document and the
//! write→reopen copy, so any divergence between the two runs is a genuine
//! round-trip regression, not an artifact of this module's simplifications.

use layout::{HeaderBands, LayoutBlock, PageBox, Paginator, StyleSpan};
use text_pipeline::{Alignment, FontStack, ShapingDirection, first_strong_direction};

/// Fallback body text size (pt) when a run/paragraph specifies none. Word's
/// stock "Normal" style is 11pt Calibri; 12pt is the more common legacy
/// default and either is fine here — this harness does not judge visual
/// fidelity, only that shaping/layout completes.
const DEFAULT_PX_SIZE: f32 = 12.0;

/// Recursion guard for pathologically nested tables (a plausible adversarial
/// / corrupted-`.docx` case). `catch_unwind` cannot stop a stack overflow —
/// that aborts the process outright — so this harness bounds descent itself
/// rather than relying on the panic-capture layer. Word's own practical
/// nesting bound is far shallower than this.
const MAX_TABLE_NESTING: u32 = 64;

fn twips_pt(twips: i32) -> f32 {
    twips as f32 / 20.0
}

fn to_paginate_geometry(g: engine::PageGeometry) -> layout::PaginatePageGeometry {
    layout::PaginatePageGeometry {
        width: g.width,
        height: g.height,
        margins: layout::Margins {
            top: g.margin_top,
            right: g.margin_right,
            bottom: g.margin_bottom,
            left: g.margin_left,
        },
        header_offset: g.header_offset,
        footer_offset: g.footer_offset,
    }
}

fn convert_alignment(a: Option<engine::Alignment>) -> Alignment {
    match a {
        Some(engine::Alignment::Start) | None => Alignment::Start,
        Some(engine::Alignment::End) => Alignment::End,
        Some(engine::Alignment::Center) => Alignment::Center,
        Some(engine::Alignment::Justify) => Alignment::Justify,
    }
}

fn convert_direction(explicit: Option<engine::TextDirection>, text: &str) -> ShapingDirection {
    match explicit {
        Some(engine::TextDirection::Rtl) => ShapingDirection::Rtl,
        Some(engine::TextDirection::Ltr) => ShapingDirection::Ltr,
        None => first_strong_direction(text).unwrap_or(ShapingDirection::Ltr),
    }
}

/// `<w:spacing w:line/w:lineRule>` -> `(line_height_pt, exact)`. Not a
/// byte-for-byte port of the production `resolve_line_height` (private to
/// `engine-wasm`) — the exact single-line-height baseline metric differs by
/// font; `1.15 * px_size` is a standard-enough approximation that layout
/// never sees a degenerate (zero/negative) line height, which is all this
/// harness needs.
fn resolve_line_height(props: &engine::ParaProperties, base_px: f32) -> (f32, bool) {
    let single = base_px * 1.15;
    match props.line_height {
        None => (single, false),
        Some(engine::LineHeight::Exact { twips }) => (twips_pt(twips).max(1.0), true),
        Some(engine::LineHeight::AtLeast { twips }) => (twips_pt(twips).max(single), false),
        Some(engine::LineHeight::Auto { twips }) => {
            let multiplier = (twips as f32 / 240.0).max(0.1);
            (single * multiplier, false)
        }
    }
}

fn default_span(start: u32, end: u32) -> StyleSpan {
    StyleSpan {
        start,
        end,
        px_size: DEFAULT_PX_SIZE,
        color: [0, 0, 0, 255],
        bold: false,
        italic: false,
        underline: engine::UnderlineStyle::None,
        strike: false,
        bg_color: None,
        font_family: None,
        caps_transform: false,
        baseline_shift_px: 0.0,
    }
}

fn style_span(start: u32, end: u32, style: &engine::SpanStyle) -> StyleSpan {
    StyleSpan {
        start,
        end,
        px_size: style.font_size.unwrap_or(DEFAULT_PX_SIZE).max(1.0),
        color: style.color.unwrap_or([0, 0, 0, 255]),
        bold: style.bold.unwrap_or(false),
        italic: style.italic.unwrap_or(false),
        underline: style.underline.unwrap_or(engine::UnderlineStyle::None),
        strike: style.strike.unwrap_or(false),
        bg_color: style.bg_color,
        /* Explicit font-family requests are not resolved to a loaded face
        id here (that mapping — `FontFamily` -> loaded font id — lives in
        `engine-wasm`'s crate-private `font_family_id`). `FontStack::resolve`
        already falls back gracefully to script-based resolution when
        `family` is `None` or unrecognised, so this only costs fidelity
        (the run shapes against the per-script default face), never a
        crash. */
        font_family: None,
        /* `<w:smallCaps>` is approximated as full `<w:caps>` — the real
        small-caps rendering (uppercase + ~80% size on originally-lowercase
        substrings only) needs the sub-span split `engine-wasm`'s
        crate-private `push_caps_spans` does; this harness cares whether
        `layout_paragraph`'s caps-transform shape path runs on the text,
        not whether the visual result matches Word. */
        caps_transform: style.caps.unwrap_or(false) || style.small_caps.unwrap_or(false),
        baseline_shift_px: 0.0,
    }
}

/// Resolved style spans covering `[0, text_len)` with no gaps — the
/// contract `layout::ParagraphConfig::spans` requires. `engine::Paragraph`
/// spans are already documented non-overlapping + sorted by `start`
/// (`crates/engine/src/lib.rs` `Paragraph::spans` doc comment); this still
/// clamps defensively since a "wild" `.docx` reaching this function has
/// already been through the real `format-docx` reader, not hand-authored
/// input.
fn resolve_style_spans(text_len: u32, runs: &[engine::StyleRun]) -> Vec<StyleSpan> {
    let mut out = Vec::with_capacity(runs.len() + 1);
    let mut cursor = 0u32;
    for run in runs {
        let start = run.start.min(text_len).max(cursor);
        let end = run.end.min(text_len);
        if end <= start {
            continue;
        }
        if start > cursor {
            out.push(default_span(cursor, start));
        }
        out.push(style_span(start, end, &run.style));
        cursor = end;
    }
    if cursor < text_len {
        out.push(default_span(cursor, text_len));
    }
    out
}

fn build_paragraph_box(
    p: &engine::Paragraph,
    fonts: &FontStack,
    content_width_pt: f32,
) -> layout::ParagraphBox {
    let text_len = p.text.len() as u32;
    let spans = resolve_style_spans(text_len, &p.spans);
    let direction = convert_direction(p.props.direction, &p.text);
    let alignment = convert_alignment(p.props.alignment);
    let base_px = spans.first().map_or(DEFAULT_PX_SIZE, |s| s.px_size);
    let (line_height, line_height_exact) = resolve_line_height(&p.props, base_px);
    let indent_start_px = twips_pt(p.props.indent.start_twips);
    let indent_end_px = twips_pt(p.props.indent.end_twips);
    let first_line_indent_px = twips_pt(p.props.indent.first_line_twips);
    let hanging_indent_px = twips_pt(p.props.indent.hanging_twips);

    let mut para_box = layout::layout_paragraph(layout::ParagraphConfig {
        text: &p.text,
        fonts,
        inline_objects: &[],
        spans: &spans,
        base_direction: direction,
        max_width: content_width_pt.max(1.0),
        line_height,
        line_height_exact,
        alignment,
        indent_start_px,
        indent_end_px,
        first_line_indent_px,
        hanging_indent_px,
        marker_text: p.resolved_marker.clone(),
        px_size_for_marker: base_px,
        tab_stops_px: &[],
    });
    para_box.borders = p.props.borders.clone();
    para_box.shading = p.props.shading;
    para_box
}

fn push_paragraph(
    p: &engine::Paragraph,
    fonts: &FontStack,
    content_width_pt: f32,
    pag: &mut Paginator,
    para_texts: &mut Vec<String>,
) {
    let mut pbox = build_paragraph_box(p, fonts, content_width_pt);
    pbox.source_paragraph_id = para_texts.len() as u32;
    para_texts.push(p.text.clone());
    let before = twips_pt(p.props.spacing.before_twips);
    let after = twips_pt(p.props.spacing.after_twips);
    pag.push_block(
        LayoutBlock::Paragraph(pbox),
        before.max(0.0),
        after.max(0.0),
    );
}

fn push_one_block(
    block: &engine::Block,
    fonts: &FontStack,
    content_width_pt: f32,
    pag: &mut Paginator,
    para_texts: &mut Vec<String>,
    depth: u32,
) {
    match block {
        engine::Block::Paragraph(p) => push_paragraph(p, fonts, content_width_pt, pag, para_texts),
        engine::Block::Table(t) => {
            if depth >= MAX_TABLE_NESTING {
                return;
            }
            for row in &t.rows {
                for cell in &row.cells {
                    if cell.props.v_merge == engine::VMergeRole::Continue {
                        continue;
                    }
                    for b in &cell.blocks {
                        push_one_block(b, fonts, content_width_pt, pag, para_texts, depth + 1);
                    }
                }
            }
        }
    }
}

/// Lay out the whole document. Returns the paginated pages plus the flat
/// per-paragraph text table `format_pdf::export_pdf` needs for
/// `/ToUnicode` — indexed by `ParagraphBox::source_paragraph_id`, which
/// this module stamps in the same walk order the table is built in, so the
/// two can never drift out of sync.
pub fn layout_document(
    doc: &engine::DocumentTree,
    fonts: &FontStack,
) -> (Vec<PageBox>, Vec<String>) {
    let sections = doc.effective_sections();
    let mut pages: Vec<PageBox> = Vec::new();
    let mut para_texts: Vec<String> = Vec::new();
    for section in &sections {
        let geometry = to_paginate_geometry(section.geometry);
        let mut pag = Paginator::new(
            geometry,
            HeaderBands::default(),
            HeaderBands::default(),
            false,
            false,
        );
        let content_w = section.geometry.content_width();
        let start = section.start_block as usize;
        let end = (section.end_block as usize).max(start);
        for b in doc.blocks.iter().skip(start).take(end - start) {
            push_one_block(b, fonts, content_w, &mut pag, &mut para_texts, 0);
        }
        pages.extend(pag.finish());
    }
    (pages, para_texts)
}
