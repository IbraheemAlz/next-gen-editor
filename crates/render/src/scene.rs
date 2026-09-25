//! Backend-agnostic display list + page scene builder
//! (PHASE_3_RENDER_RTL.md §9.1).
//!
//! Layout produces the hierarchical box tree ([`layout::PageBox`]);
//! [`build_page_scene`] walks `PageBox` → `ParagraphBox` → `LineBox` →
//! `VisualRun`, accumulating parent-relative origins into absolute positions,
//! and lowers it to a linear [`DisplayList`] any backend interprets. `kurbo`
//! is the geometry vocabulary, `peniko` the paint vocabulary.

use kurbo::{Affine, Rect};
use layout::{LayoutBlock, NoteBand, PageBox, ParagraphBox, TableBox};
use peniko::{Brush, Color};

/// Font identifier — a key into the engine's font map.
pub type FontId = String;

/// A solid paint. Wraps a `peniko::Brush`; batch 1 only ever uses `Solid`.
#[derive(Debug, Clone)]
pub struct Paint {
    pub brush: Brush,
}

impl Paint {
    pub fn solid(color: Color) -> Self {
        Self {
            brush: Brush::Solid(color),
        }
    }
}

/// One positioned glyph in a run. `x`/`y` is the baseline pen position; the
/// backend adds the rasterized glyph's bearing.
#[derive(Debug, Clone, Copy)]
pub struct RunGlyph {
    pub glyph_id: u16,
    pub x: f64,
    pub y: f64,
}

/// A run of glyphs sharing one font, pixel size, and paint.
#[derive(Debug, Clone)]
pub struct GlyphRun {
    pub font: FontId,
    pub px_size: f32,
    pub paint: Paint,
    pub glyphs: Vec<RunGlyph>,
    /// Render with synthetic bold / italic — no real face variant exists
    /// (Backlog #1).
    pub faux_bold: bool,
    pub faux_italic: bool,
    /// Highlight colour painted behind the run. The glyph blit composites
    /// over it, since `put_image_data` would otherwise punch holes through a
    /// background rect (Backlog #1).
    pub bg_color: Option<[u8; 4]>,
}

/// One backend-agnostic drawing command. Batch 1 freezes this set; paths,
/// images, and layers arrive with the features that need them.
#[derive(Debug, Clone)]
pub enum DisplayCmd {
    FillRect {
        rect: Rect,
        paint: Paint,
    },
    StrokeRect {
        rect: Rect,
        paint: Paint,
        width: f64,
    },
    DrawGlyphRun(GlyphRun),
    /// Phase 7 — paint an inline image at `rect`. `rel_id` matches the
    /// archive relationship id the engine stashed for the image; the
    /// backend looks the decoded bitmap up via its image-cache
    /// callback and falls back to a placeholder rectangle on miss
    /// (image bytes not yet decoded, or the rId resolved nothing).
    DrawImage {
        rect: Rect,
        rel_id: String,
    },
    /// UI-polish sprint — paint one page "card" (Google Docs / Word
    /// print-layout look): a white rectangle floating over the gray
    /// desk background with a soft drop shadow underneath. Backends
    /// route this through their native shadow primitive
    /// (`ctx.shadow*` on Canvas2D); the standalone command keeps
    /// the renderer free of paint-state side effects that a plain
    /// `FillRect` would leak to subsequent draws.
    DrawPageCard {
        rect: Rect,
    },
    PushClip {
        rect: Rect,
    },
    PopClip,
    PushTransform(Affine),
    PopTransform,
}

/// An ordered list of drawing commands — the renderer's sole input.
#[derive(Debug, Clone, Default)]
pub struct DisplayList {
    pub cmds: Vec<DisplayCmd>,
}

/* Page chrome colours — fixed in the PoC; configurable in a later batch. */
fn page_color() -> Color {
    Color::from_rgba8(0xff, 0xff, 0xff, 0xff)
}
fn border_color() -> Color {
    Color::from_rgba8(0xcc, 0xcc, 0xcc, 0xff)
}

/// Google Docs / Word print-layout "desk" colour the white pages float
/// over. The backend clears the entire canvas to this before walking
/// the display list so inter-page gaps show through naturally.
pub fn desk_background_color() -> Color {
    Color::from_rgba8(0xe8, 0xea, 0xed, 0xff)
}

/// Lower a laid-out [`PageBox`] into a [`DisplayList`].
///
/// Pure traversal — layout already owns every coordinate. Emits a white page
/// fill and a 1px border inset 0.5px, then walks `PageBox` → `ParagraphBox` →
/// `LineBox` → `VisualRun`. Each level's `origin` is parent-relative, so the
/// accumulated origin reaches absolute glyph positions; font size and colour
/// come from each run's `attrs` (no `PaintConfig` side channel).
pub fn build_page_scene(page: &PageBox) -> DisplayList {
    build_document_scene(std::slice::from_ref(page), 0.0)
}

/// Phase 6c multi-canvas refactor — emit one page's scene with its
/// origin at `(0, 0)` in the target canvas. Used by the per-page
/// canvas architecture: each `<canvas>` element gets exactly one page
/// painted into it, so `page_top` accumulation is unnecessary and
/// hit-testing routes through `page_index + local_coords`.
pub fn build_single_page_scene(page: &PageBox) -> DisplayList {
    build_document_scene(std::slice::from_ref(page), 0.0)
}

/// Vertical gap between consecutive pages, in layout points at `scale = 1`.
/// `48 pt` × `4/3 screen-DPI scale` ≈ 64 CSS px between pages — wide
/// enough that desk-grey clearly separates one A4 sheet from the next
/// and the eye reads them as physically distinct cards. The renderer
/// multiplies by `scale` at the call site, so the gap grows / shrinks
/// with the document zoom.
pub const PAGE_GAP_PT: f32 = 48.0;

/// Phase 6 — lower a paginated document (one or more `PageBox`es) into a
/// single `DisplayList`. Pages stack vertically with `gap` pt of empty
/// space between them; absolute Y inside a page = `page_top + content_y`.
pub fn build_document_scene(pages: &[PageBox], gap: f32) -> DisplayList {
    let mut cmds: Vec<DisplayCmd> = Vec::new();
    let mut top = 0.0_f32;
    for page in pages {
        let w = page.size.width as f64;
        let h = page.size.height as f64;
        let t = top as f64;
        /* UI polish — one "page card" per `PageBox`: the backend draws
        the white fill with a soft drop shadow underneath, giving the
        Google Docs / Word print-layout look (distinct sheets floating
        on the gray desk). No explicit border — the shadow does the
        edge separation work. */
        cmds.push(DisplayCmd::DrawPageCard {
            rect: Rect::new(0.0, t, w, t + h),
        });
        /* Avoid unused-function lints — both helpers are kept available
        for callers that want the legacy chrome (e.g. PDF preview thumbs
        that don't want a shadow). */
        let _ = page_color();
        let _ = border_color();

        let content_x = page.margins.left;
        let content_y = top + page.margins.top;

        /* Issue #69 — floating objects marked `behindDoc` paint UNDER every
        band and body glyph (right after the page card), lowest z-order
        first. The in-front group closes the page below. */
        paint_floats(page, top, true, &mut cmds);

        /* Phase 6 — header band painted before body so a wide header doesn't
        sit on top of body text. Phase 3 (#39): band placement comes from
        `PageBox::header_band_top()` — the document's real
        `<w:pgMar w:header>` offset, shared with engine-wasm's story
        hit-testing so paint and caret geometry cannot diverge. */
        if let Some(hf) = &page.header {
            let band_top = top + page.header_band_top();
            /* Issue #72 — bands are block lists now; `paint_block`
            dispatches tables through the same painter the body uses. */
            for block in &hf.blocks {
                paint_block(block, content_x, band_top, &mut cmds);
            }
        }

        for block in &page.blocks {
            paint_block(block, content_x, content_y, &mut cmds);
        }

        /* Issue #80 — note bands. The paginator owns placement
        (`NoteBand::y`, page-relative) and already reserved the heights,
        so entries always fit; the endnote band trails the body, the
        footnote band sits at the page bottom (or beneath the text).
        Each opens with a separator rule: 30 % of the content width for
        a fresh band, full width when the band continues a note cut on
        the previous page (Word's continuation separator). */
        paint_note_band(&page.endnotes, page, top, content_x, &mut cmds);
        paint_note_band(&page.footnotes, page, top, content_x, &mut cmds);

        if let Some(hf) = &page.footer {
            let band_top = top + page.footer_band_top();
            for block in &hf.blocks {
                paint_block(block, content_x, band_top, &mut cmds);
            }
        }

        /* Issue #69 — the in-front float group closes the page: every
        `<wp:anchor>` object NOT marked `behindDoc` paints over the bands,
        body and footnotes, lowest `relativeHeight` first so a higher
        z-order lands on top. */
        paint_floats(page, top, false, &mut cmds);

        top += page.size.height + gap;
    }
    DisplayList { cmds }
}

/// Issue #80 — paint one note band: separator rule + every entry's
/// blocks through the body's own block painter.
fn paint_note_band(
    band: &NoteBand,
    page: &PageBox,
    page_top: f32,
    content_x: f32,
    cmds: &mut Vec<DisplayCmd>,
) {
    if band.is_empty() {
        return;
    }
    let band_top = page_top + band.y;
    let content_w = page.size.width - page.margins.left - page.margins.right;
    let rule_w = if band.continuation {
        content_w
    } else {
        content_w * 0.3
    };
    let rule_y = band_top - 6.0;
    cmds.push(DisplayCmd::FillRect {
        rect: Rect::new(
            content_x as f64,
            rule_y as f64,
            (content_x + rule_w) as f64,
            (rule_y + 0.75) as f64,
        ),
        paint: Paint::solid(Color::from_rgba8(0x55, 0x55, 0x55, 0xff)),
    });
    for entry in &band.entries {
        let entry_top = band_top + entry.origin.y;
        let entry_x = content_x + entry.origin.x;
        for block in &entry.blocks {
            paint_block(block, entry_x, entry_top, cmds);
        }
    }
}

/// Issue #69 — paint one z-order group of a page's floating objects.
/// `behind` selects the `behindDoc` group (painted right after the page
/// card, under all text) or the in-front group (painted last). Within a
/// group the objects are ordered by `z_order` (`relativeHeight`) — the
/// sort is stable so equal z-orders keep anchor order. `FloatBox.origin`
/// is page-relative (page top-left, not the content area), so only the
/// page's stacked `top` is added. Hidden objects (`hidden="1"`) stay in
/// the box list for hit-testing but are never painted; an object with a
/// degenerate size paints nothing (the backend would reject an empty
/// rect anyway).
fn paint_floats(page: &PageBox, top: f32, behind: bool, cmds: &mut Vec<DisplayCmd>) {
    if page.floats.is_empty() {
        return;
    }
    let mut group: Vec<&layout::FloatBox> = page
        .floats
        .iter()
        .filter(|f| f.behind_doc == behind && !f.hidden)
        .collect();
    if group.is_empty() {
        return;
    }
    group.sort_by_key(|f| f.z_order);
    for f in group {
        if f.size.width <= 0.0 || f.size.height <= 0.0 {
            continue;
        }
        let x0 = f.origin.x as f64;
        let y0 = (top + f.origin.y) as f64;
        if f.text_box.is_some() {
            paint_text_box(f, 0.0, top, cmds);
            continue;
        }
        cmds.push(DisplayCmd::DrawImage {
            rect: Rect::new(x0, y0, x0 + f.size.width as f64, y0 + f.size.height as f64),
            rel_id: f.rel_id.clone(),
        });
    }
}

/// Issue #83 — paint one text box: shape fill, the story clipped to the
/// shape rect, then the outline on top. The clip is belt and braces:
/// Canvas2D blits glyphs with `put_image_data`, which ignores the clip,
/// so lines starting at or past the shape's bottom edge are also culled
/// here (line-granular overflow clip, exact on Vello / PDF).
///
/// `(base_x, base_y)` is the absolute origin `f.origin` is relative to:
/// the page's top-left for a page float, the parent box's content rect
/// for a nested one (issue #165). Nested boxes paint inside the
/// parent's clip — `behindDoc` ones under the parent story's text, the
/// rest over it, each group in `z_order` — recursing through the same
/// function (depth bounded by the layout's nesting cap).
fn paint_text_box(f: &layout::FloatBox, base_x: f32, base_y: f32, cmds: &mut Vec<DisplayCmd>) {
    let Some(tb) = f.text_box.as_deref() else {
        return;
    };
    if f.size.width <= 0.0 || f.size.height <= 0.0 {
        return;
    }
    let x0 = (base_x + f.origin.x) as f64;
    let y0 = (base_y + f.origin.y) as f64;
    let rect = Rect::new(x0, y0, x0 + f.size.width as f64, y0 + f.size.height as f64);
    if let Some([r, g, b, a]) = tb.source.fill {
        cmds.push(DisplayCmd::FillRect {
            rect,
            paint: Paint::solid(Color::from_rgba8(r, g, b, a)),
        });
    }
    if let Some((origin, _)) = f.text_box_content_rect() {
        let content_x = base_x + origin.x;
        let content_y = base_y + origin.y;
        /* Content-relative y past which nothing is visible. */
        let limit = f.origin.y + f.size.height - origin.y;
        cmds.push(DisplayCmd::PushClip { rect });
        paint_nested_text_boxes(tb, content_x, content_y, true, cmds);
        for block in &tb.blocks {
            match clip_block_lines(block, limit) {
                Some(clipped) => paint_block(&clipped, content_x, content_y, cmds),
                None if block.origin().y < limit => paint_block(block, content_x, content_y, cmds),
                None => {}
            }
        }
        paint_nested_text_boxes(tb, content_x, content_y, false, cmds);
        cmds.push(DisplayCmd::PopClip);
    }
    if let Some(([r, g, b, a], w)) = tb.source.outline
        && w > 0.0
    {
        cmds.push(DisplayCmd::StrokeRect {
            rect,
            paint: Paint::solid(Color::from_rgba8(r, g, b, a)),
            width: w as f64,
        });
    }
}

/// Issue #165 — one z-order group (`behind` = the `behindDoc` group) of
/// the boxes nested in `tb`'s story, at the parent's content origin.
fn paint_nested_text_boxes(
    tb: &layout::TextBoxFrame,
    content_x: f32,
    content_y: f32,
    behind: bool,
    cmds: &mut Vec<DisplayCmd>,
) {
    if tb.floats.is_empty() {
        return;
    }
    let mut group: Vec<&layout::FloatBox> = tb
        .floats
        .iter()
        .filter(|f| f.text_box.is_some() && f.behind_doc == behind && !f.hidden)
        .collect();
    group.sort_by_key(|f| f.z_order);
    for f in group {
        paint_text_box(f, content_x, content_y, cmds);
    }
}

/// Issue #83 — a paragraph block with every line that starts at or past
/// `limit` (container-relative y) removed; `None` when nothing needs
/// cutting (or the block is a table, which is kept or dropped whole).
fn clip_block_lines(block: &LayoutBlock, limit: f32) -> Option<LayoutBlock> {
    let LayoutBlock::Paragraph(p) = block else {
        return None;
    };
    let keep = p
        .lines
        .iter()
        .take_while(|l| p.origin.y + l.origin.y < limit)
        .count();
    if keep == p.lines.len() {
        return None;
    }
    let mut cut = p.clone();
    cut.lines.truncate(keep);
    Some(LayoutBlock::Paragraph(cut))
}

/// Recursive dispatcher — handles top-level page blocks *and* cell
/// content (Phase 5 PR 2: a table cell can carry paragraphs + nested
/// tables). `base_x` / `base_y` is the parent container's content
/// origin in absolute page coordinates; the block's own `origin` is
/// added on top.
fn paint_block(block: &LayoutBlock, base_x: f32, base_y: f32, cmds: &mut Vec<DisplayCmd>) {
    match block {
        LayoutBlock::Paragraph(p) => paint_paragraph(p, base_x, base_y, cmds),
        LayoutBlock::Table(t) => paint_table(t, base_x, base_y, cmds),
    }
}

fn paint_table(t: &TableBox, base_x: f32, base_y: f32, cmds: &mut Vec<DisplayCmd>) {
    let tx = base_x + t.origin.x;
    let ty = base_y + t.origin.y;
    /* Per RFC §3.1: paint cell shading + content first, then borders
    on top so border strokes are not hidden behind shading. Skip every
    `VMergeRole::Continue` cell — the matching `Restart` cell visually
    owns the merged region. */
    for row in &t.rows {
        let row_x = tx + row.origin.x;
        let row_y = ty + row.origin.y;
        for cell in &row.cells {
            if matches!(cell.v_merge, engine::VMergeRole::Continue) {
                continue;
            }
            let cell_x = row_x + cell.origin.x;
            let cell_y = row_y + cell.origin.y;
            /* Shading first — behind content. */
            if let Some([r, g, b, a]) = cell.shading {
                cmds.push(DisplayCmd::FillRect {
                    rect: Rect::new(
                        cell_x as f64,
                        cell_y as f64,
                        (cell_x + cell.size.width) as f64,
                        (cell_y + cell.size.height) as f64,
                    ),
                    paint: Paint::solid(Color::from_rgba8(r, g, b, a)),
                });
            }
            /* Recurse — paragraphs + nested tables. Phase 2 audit
            (gap B.1/B.2) — content origin is offset by the cell's
            resolved `<w:tcMar>` / `<w:tblCellMar>` padding so text
            doesn't render flush against the border. */
            let content_x = cell_x + cell.padding_left;
            let content_y = cell_y + cell.padding_top;
            for inner in &cell.content {
                paint_block(inner, content_x, content_y, cmds);
            }
        }
    }
    /* Borders pass — emit after content so strokes sit on top. To avoid
    double-stroking shared edges between adjacent cells we use the
    "right + bottom win" convention: every cell paints its top + left,
    plus its right when it is the last column or the right neighbour
    has no shared edge, plus its bottom when it is the last row. The
    outer-table edges layer on top from `t.outer_borders`. */
    for (ri, row) in t.rows.iter().enumerate() {
        let row_x = tx + row.origin.x;
        let row_y = ty + row.origin.y;
        let last_row = ri + 1 == t.rows.len();
        for (ci, cell) in row.cells.iter().enumerate() {
            if matches!(cell.v_merge, engine::VMergeRole::Continue) {
                continue;
            }
            let last_col = ci + 1 == row.cells.len();
            let cell_x = row_x + cell.origin.x;
            let cell_y = row_y + cell.origin.y;
            let cx1 = cell_x + cell.size.width;
            let cy1 = cell_y + cell.size.height;
            paint_border_edge(&cell.borders.top, cell_x, cell_y, cx1, cell_y, cmds);
            paint_border_edge(&cell.borders.left, cell_x, cell_y, cell_x, cy1, cmds);
            /* The "right + bottom win" de-duplication convention applies
            in both branches — paint the cell's right + bottom regardless
            of whether the next column / row exists, and skip the
            neighbour's left / top. `last_col` / `last_row` are tracked
            for the outer-table perimeter check below. */
            paint_border_edge(&cell.borders.right, cx1, cell_y, cx1, cy1, cmds);
            paint_border_edge(&cell.borders.bottom, cell_x, cy1, cx1, cy1, cmds);
            let _ = (last_col, last_row);
        }
    }
    /* Outer-table perimeter. */
    let tx1 = tx + t.size.width;
    let ty1 = ty + t.size.height;
    paint_border_edge(&t.outer_borders.top, tx, ty, tx1, ty, cmds);
    paint_border_edge(&t.outer_borders.left, tx, ty, tx, ty1, cmds);
    paint_border_edge(&t.outer_borders.right, tx1, ty, tx1, ty1, cmds);
    paint_border_edge(&t.outer_borders.bottom, tx, ty1, tx1, ty1, cmds);
}

fn paint_border_edge(
    edge: &Option<engine::BorderStroke>,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    cmds: &mut Vec<DisplayCmd>,
) {
    let Some(stroke) = edge else { return };
    if matches!(stroke.style, engine::BorderStyle::None) {
        return;
    }
    /* `<w:sz>` is eighths of a point. 1 pt = 1.333 px at 96 DPI.
    Clamp to at least 1 px so the stroke is visible. */
    let weight = ((stroke.size_eighth_pt as f64) / 8.0 * 1.333).max(1.0);
    let color = stroke.color.unwrap_or([0, 0, 0, 255]);
    let [r, g, b, a] = color;
    /* Horizontal vs vertical: y0 == y1 → horizontal strip; x0 == x1 →
    vertical strip. Pad by half the weight on each side so the stroke
    is centred on the edge. */
    let (rx0, ry0, rx1, ry1) = if (y1 - y0).abs() < 0.5 {
        let half = (weight as f32) * 0.5;
        (x0, y0 - half, x1, y0 + half)
    } else {
        let half = (weight as f32) * 0.5;
        (x0 - half, y0, x0 + half, y1)
    };
    cmds.push(DisplayCmd::FillRect {
        rect: Rect::new(rx0 as f64, ry0 as f64, rx1 as f64, ry1 as f64),
        paint: Paint::solid(Color::from_rgba8(r, g, b, a)),
    });
}

fn paint_paragraph(para: &ParagraphBox, base_x: f32, base_y: f32, cmds: &mut Vec<DisplayCmd>) {
    let para_x = base_x + para.origin.x;
    let para_y = base_y + para.origin.y;
    /* Sprint 6 (UI Edition) — `<w:pPr><w:shd>` paragraph shading.
    Painted BEFORE borders + glyphs so both draw on top of the fill.
    Mirror of the cell-shading code path in `paint_table_cell`. */
    if let Some([r, g, b, a]) = para.shading {
        cmds.push(DisplayCmd::FillRect {
            rect: Rect::new(
                para_x as f64,
                para_y as f64,
                (para_x + para.size.width) as f64,
                (para_y + para.size.height) as f64,
            ),
            paint: Paint::solid(Color::from_rgba8(r, g, b, a)),
        });
    }
    /* Audit gap A.M4 — `<w:pBdr>` strokes around the paragraph rect.
    Painted FIRST so the text glyphs draw on top of the border (matches
    Word's compositing order); the table-border edge primitive is
    reused so dotted / dashed / coloured strokes behave consistently
    with cell borders. */
    if let Some(b) = para.borders.as_ref() {
        let px1 = para_x + para.size.width;
        let py1 = para_y + para.size.height;
        paint_border_edge(&b.top, para_x, para_y, px1, para_y, cmds);
        paint_border_edge(&b.left, para_x, para_y, para_x, py1, cmds);
        paint_border_edge(&b.right, px1, para_y, px1, py1, cmds);
        paint_border_edge(&b.bottom, para_x, py1, px1, py1, cmds);
    }
    {
        /* Phase 4 — list marker. Lives in the leading-edge gutter, baseline
        aligned with the first line. Paint it before the line runs so it
        sits visually beside the body text — z-order doesn't matter here,
        but rendering first keeps the loop structure simple. */
        if let Some(marker) = &para.marker {
            let m_x = para_x + marker.origin.x;
            let m_baseline = (para_y + marker.origin.y + marker.baseline) as f64;
            let mut pen = 0.0_f32;
            let mut glyphs: Vec<RunGlyph> = Vec::with_capacity(marker.run.glyphs.len());
            for glyph in &marker.run.glyphs {
                if glyph.id != 0 {
                    glyphs.push(RunGlyph {
                        glyph_id: glyph.id,
                        x: (m_x as f64) + (pen as f64) + (glyph.x_offset as f64),
                        y: m_baseline - (glyph.y_offset as f64),
                    });
                }
                pen += glyph.x_advance;
            }
            if !glyphs.is_empty() {
                let [r, g, b, a] = marker.run.attrs.color;
                cmds.push(DisplayCmd::DrawGlyphRun(GlyphRun {
                    font: marker.run.font.clone(),
                    px_size: marker.run.attrs.px_size,
                    paint: Paint::solid(Color::from_rgba8(r, g, b, a)),
                    glyphs,
                    faux_bold: marker.run.attrs.faux_bold,
                    faux_italic: marker.run.attrs.faux_italic,
                    bg_color: marker.run.attrs.bg_color,
                }));
            }
        }
        for line in &para.lines {
            let line_x = para_x + line.origin.x;
            let baseline = (para_y + line.origin.y + line.baseline) as f64;
            let line_top = (para_y + line.origin.y) as f64;
            let line_bottom = line_top + line.height as f64;
            /* One pen across the whole line; runs lie left-to-right in
            visual order, each glyph placed at the cumulative advance. */
            let mut pen = 0.0_f32;
            for run in &line.runs {
                let [r, g, b, a] = run.attrs.color;
                let text_color = Color::from_rgba8(r, g, b, a);
                let run_x0 = (line_x as f64) + (pen as f64);
                /* Audit gap A.M1 — `<w:vertAlign>` baseline shift. The
                run's glyphs draw at `baseline - shift`: positive shift
                lifts the glyph (superscript), negative drops it
                (subscript). Applied per-run, not per-line, because a
                line typically mixes baseline + shifted runs. */
                let run_baseline = baseline - (run.attrs.baseline_shift_px as f64);
                let mut glyphs: Vec<RunGlyph> = Vec::with_capacity(run.glyphs.len());
                /* Phase 7 — inline image draws are emitted in line with
                the run loop so the pen stays in sync. Collected separately
                from text glyphs so the renderer routes them through
                `DisplayCmd::DrawImage` instead of `DrawGlyphRun`. */
                let mut images: Vec<(f64, f64, f64, f64, String)> = Vec::new();
                /* Issue #80 — note markers are real superscript glyphs
                now (shaped by `layout::paragraph::shape_note_marker`
                into their own run with a raised baseline), so they
                paint through the ordinary glyph path below. */
                for glyph in &run.glyphs {
                    /* Issue #69 — a FLOATING object's sentinel reserves no
                    width and paints nothing in the line: the object itself
                    is painted from `PageBox::floats` by `paint_floats`.
                    Skip the glyph so a font that has a real glyph for
                    U+FFFC never leaves a zero-advance tofu on the line. */
                    if glyph.float.is_some() {
                        pen += glyph.x_advance;
                        continue;
                    }
                    if let Some(rel) = glyph.inline_image_rel_id.as_deref() {
                        /* Image sits flush with the baseline (its bottom
                        edge); the image's `inline_object_height` extends
                        upward, matching the line's grown ascent. */
                        let x0 = (line_x as f64) + (pen as f64);
                        let y1 = baseline;
                        let y0 = y1 - (glyph.inline_object_height as f64);
                        let x1 = x0 + (glyph.x_advance as f64);
                        images.push((x0, y0, x1, y1, rel.to_string()));
                        pen += glyph.x_advance;
                        continue;
                    }
                    /* Issue #81 — a leadered tab fills its advance with
                    the leader pattern (TOC dot leaders); the tab glyph
                    itself draws nothing. */
                    if let Some(kind) = glyph.leader {
                        let x0 = (line_x as f64) + (pen as f64);
                        push_tab_leader(
                            cmds,
                            kind,
                            x0,
                            x0 + glyph.x_advance as f64,
                            baseline,
                            run.attrs.px_size as f64,
                            text_color,
                        );
                        pen += glyph.x_advance;
                        continue;
                    }
                    /* glyph id 0 is .notdef — advance the pen, draw nothing. */
                    if glyph.id != 0 {
                        glyphs.push(RunGlyph {
                            glyph_id: glyph.id,
                            x: (line_x as f64) + (pen as f64) + (glyph.x_offset as f64),
                            y: run_baseline - (glyph.y_offset as f64),
                        });
                    }
                    pen += glyph.x_advance;
                }
                let run_x1 = (line_x as f64) + (pen as f64);
                for (x0, y0, x1, y1, rel_id) in images {
                    cmds.push(DisplayCmd::DrawImage {
                        rect: Rect::new(x0, y0, x1, y1),
                        rel_id,
                    });
                }

                /* Background highlight — emitted before the glyphs so it sits
                behind them, spanning the run's advance and the line's full
                height so adjacent highlights tile seamlessly (Backlog #1). */
                if let Some([br, bgc, bb, ba]) = run.attrs.bg_color
                    && run_x1 > run_x0
                {
                    cmds.push(DisplayCmd::FillRect {
                        rect: Rect::new(run_x0, line_top, run_x1, line_bottom),
                        paint: Paint::solid(Color::from_rgba8(br, bgc, bb, ba)),
                    });
                }
                if !glyphs.is_empty() {
                    cmds.push(DisplayCmd::DrawGlyphRun(GlyphRun {
                        font: run.font.clone(),
                        px_size: run.attrs.px_size,
                        paint: Paint::solid(text_color),
                        glyphs,
                        faux_bold: run.attrs.faux_bold,
                        faux_italic: run.attrs.faux_italic,
                        bg_color: run.attrs.bg_color,
                    }));
                }
                /* Decoration strokes — thin `FillRect`s drawn over the glyphs.
                Y positions are px-size-relative approximations (Backlog #1):
                the underline sits just below the baseline, the strikethrough
                is centred ~one quarter em above it (≈ x-height / 2). */
                let underline_visible = run.attrs.underline.is_visible();
                if (underline_visible || run.attrs.strike) && run_x1 > run_x0 {
                    let px = run.attrs.px_size as f64;
                    let thickness = (px * 0.06).max(1.0);
                    if underline_visible {
                        let top = baseline + px * 0.10;
                        push_underline_pattern(
                            cmds,
                            run.attrs.underline,
                            run_x0,
                            run_x1,
                            top,
                            thickness,
                            text_color,
                        );
                    }
                    if run.attrs.strike {
                        let mid = baseline - px * 0.25;
                        cmds.push(DisplayCmd::FillRect {
                            rect: Rect::new(
                                run_x0,
                                mid - thickness / 2.0,
                                run_x1,
                                mid + thickness / 2.0,
                            ),
                            paint: Paint::solid(text_color),
                        });
                    }
                }
            }
        }
    }
}

/// Issue #81 — fill a leadered tab's advance `x0..x1` with its leader
/// pattern. Dots / hyphens tile on a grid anchored at x = 0 of the page
/// so the leaders of consecutive TOC entries line up (Word does the
/// same); a small pad keeps them off the neighbouring glyphs.
fn push_tab_leader(
    cmds: &mut Vec<DisplayCmd>,
    kind: layout::TabLeaderKind,
    x0: f64,
    x1: f64,
    baseline: f64,
    px: f64,
    color: Color,
) {
    use layout::TabLeaderKind as K;
    let px = px.max(1.0);
    let pad = px * 0.15;
    let (lo, hi) = (x0 + pad, x1 - pad);
    if hi <= lo {
        return;
    }
    let dot = (px * 0.08).max(1.0);
    let rect = |cmds: &mut Vec<DisplayCmd>, a: f64, top: f64, b: f64, bottom: f64| {
        cmds.push(DisplayCmd::FillRect {
            rect: Rect::new(a, top, b, bottom),
            paint: Paint::solid(color),
        });
    };
    match kind {
        K::Dot | K::MiddleDot => {
            let step = px * 0.33;
            let y = if matches!(kind, K::Dot) {
                baseline - dot
            } else {
                baseline - px * 0.3
            };
            let mut x = (lo / step).ceil() * step;
            while x + dot <= hi {
                rect(cmds, x, y, x + dot, y + dot);
                x += step;
            }
        }
        K::Hyphen => {
            let step = px * 0.4;
            let dash = px * 0.25;
            let y = baseline - px * 0.28;
            let mut x = (lo / step).ceil() * step;
            while x + dash <= hi {
                rect(cmds, x, y, x + dash, y + dot);
                x += step;
            }
        }
        K::Underscore | K::Heavy => {
            let t = if matches!(kind, K::Heavy) {
                dot * 2.0
            } else {
                dot
            };
            let y = baseline + px * 0.1;
            rect(cmds, lo, y, hi, y + t);
        }
    }
}

/// Emit the underline rectangles for a single `(x0..x1, top)` span,
/// patterning the strokes to approximate the OOXML variant. Canvas2D
/// has no native dash array on our `FillRect` path, so dotted / dashed
/// / wavy are tiled as small fills.
///
/// Pattern math (all multiples of `thickness` so the visual weight
/// scales with the font px size, matching Word's screen rendering):
/// - `Single`: one rect across the run.
/// - `Double`: two parallel rects, separated by `2 * thickness`.
/// - `Dotted`: square dots `thickness × thickness`, pitch `2 * thickness`.
/// - `Dashed`: dashes `4 * thickness` wide, pitch `8 * thickness`.
/// - `Wavy`: two-pixel sawtooth — small rects alternating between two Y
///   bands `2 * thickness` apart. Cheap visual approximation of the
///   sinusoidal stroke Word draws; round-trips faithfully because the
///   variant is preserved on the model side.
fn push_underline_pattern(
    cmds: &mut Vec<DisplayCmd>,
    style: engine::UnderlineStyle,
    x0: f64,
    x1: f64,
    top: f64,
    thickness: f64,
    paint_color: peniko::Color,
) {
    use engine::UnderlineStyle::*;
    let solid = |cmds: &mut Vec<DisplayCmd>, lo: f64, hi: f64, t: f64| {
        if hi > lo {
            cmds.push(DisplayCmd::FillRect {
                rect: Rect::new(lo, t, hi, t + thickness),
                paint: Paint::solid(paint_color),
            });
        }
    };
    match style {
        None => {}
        Single => solid(cmds, x0, x1, top),
        Double => {
            solid(cmds, x0, x1, top);
            solid(cmds, x0, x1, top + thickness * 2.0);
        }
        Dotted => {
            let pitch = (thickness * 2.0).max(2.0);
            let mut x = x0;
            while x < x1 {
                let end = (x + thickness).min(x1);
                solid(cmds, x, end, top);
                x += pitch;
            }
        }
        Dashed => {
            let dash = (thickness * 4.0).max(3.0);
            let gap = dash;
            let mut x = x0;
            while x < x1 {
                let end = (x + dash).min(x1);
                solid(cmds, x, end, top);
                x += dash + gap;
            }
        }
        Wavy => {
            /* Sawtooth: tile pairs of short rects on alternating rows.
            Period = `4 * thickness`; each half-period is one short rect. */
            let half = (thickness * 2.0).max(2.0);
            let top_band = top - thickness;
            let bottom_band = top + thickness;
            let mut x = x0;
            let mut up = true;
            while x < x1 {
                let end = (x + half).min(x1);
                let band_top = if up { top_band } else { bottom_band };
                solid(cmds, x, end, band_top);
                up = !up;
                x += half;
            }
        }
    }
}

#[cfg(test)]
mod leader_tests {
    use super::*;

    #[test]
    fn dot_leader_tiles_small_squares_inside_the_tab_advance() {
        let mut cmds = Vec::new();
        let black = Color::from_rgba8(0, 0, 0, 255);
        push_tab_leader(
            &mut cmds,
            layout::TabLeaderKind::Dot,
            10.0,
            110.0,
            50.0,
            12.0,
            black,
        );
        /* step = 3.96 px over ~96 px of padded span → ~24 dots. */
        assert!(cmds.len() >= 20, "got {} dots", cmds.len());
        for c in &cmds {
            let DisplayCmd::FillRect { rect, .. } = c else {
                panic!("leader paints fills only");
            };
            assert!(rect.x0 >= 10.0 && rect.x1 <= 110.0);
            assert!(rect.y1 <= 50.0 + 0.001, "dots sit on the baseline");
        }
        /* A tab narrower than the padding paints nothing. */
        let mut none = Vec::new();
        push_tab_leader(
            &mut none,
            layout::TabLeaderKind::Dot,
            0.0,
            2.0,
            50.0,
            12.0,
            black,
        );
        assert!(none.is_empty());
        let mut line = Vec::new();
        push_tab_leader(
            &mut line,
            layout::TabLeaderKind::Underscore,
            0.0,
            100.0,
            50.0,
            12.0,
            black,
        );
        assert_eq!(line.len(), 1);
    }
}
