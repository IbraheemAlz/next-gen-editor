//! Paginator — flow a sequence of `LayoutBlock`s onto multiple `PageBox`es.
//!
//! Phase 6. The pre-Phase-6 pipeline stacked every block onto a single
//! infinite-height `PageBox`. This module wraps that flow with a per-section
//! page-budget tracker: when the next block — or a paragraph's next line —
//! cannot fit in the remaining content-height of the current page, the
//! paginator closes the page and opens a fresh one with the same geometry.
//!
//! Splitting policy:
//!
//! - **Paragraphs split at line boundaries.** A paragraph that overflows is
//!   cut into a head (lines that fit on the current page) and a tail
//!   (lines that flow onto the next). The marker stays with the head. Both
//!   chunks carry per-line origins relative to their own paragraph top.
//! - **Tables split at row boundaries.** A table that doesn't fit moves
//!   wholly to the next page; if it still doesn't fit, the paginator emits
//!   the prefix of rows that do and pushes the remainder. Mid-cell
//!   splitting is deferred — cells stay atomic.
//!
//! Section breaks are caller-driven via [`Paginator::start_new_page`]; the
//! paginator itself only knows about overflow.

use crate::boxes::{
    FootnoteEntry, HeaderFooterBox, LayoutBlock, LineBox, PageBox, ParagraphBox, Point, Size,
    TableBox,
};
use crate::page::Margins;
use std::collections::HashMap;

/// Phase 8a — vertical gap above the footnote separator rule, in layout
/// pt at scale=1. The renderer multiplies by `scale` if it needs device
/// pixels.
pub const FOOTNOTE_SEPARATOR_HEIGHT_PT: f32 = 12.0;

/// Concrete geometry for one paginated page. Mirrors `engine::PageGeometry`
/// without taking a dependency on the engine crate.
#[derive(Debug, Clone, Copy)]
pub struct PageGeometry {
    pub width: f32,
    pub height: f32,
    pub margins: Margins,
    /// Distance from the top of the page to the top of the header band.
    pub header_offset: f32,
    /// Distance from the bottom of the page to the bottom of the footer band.
    pub footer_offset: f32,
}

impl PageGeometry {
    pub fn content_height(&self) -> f32 {
        self.height - self.margins.top - self.margins.bottom
    }
}

/// Page-flow accumulator. Construct with `new`, feed [`LayoutBlock`]s via
/// [`Paginator::push_block`], optionally start a fresh page on a section
/// break with [`Paginator::start_new_page`], and drain the final page with
/// [`Paginator::finish`].
pub struct Paginator {
    geometry: PageGeometry,
    /// Optional header / footer to attach to every page emitted with this
    /// geometry. Cloned onto each `PageBox`.
    header: Option<HeaderFooterBox>,
    footer: Option<HeaderFooterBox>,
    /// Accumulating page state.
    cur_blocks: Vec<LayoutBlock>,
    /// Cursor inside the current page's content area, in content-relative
    /// pt (0.0 at the top of the content rect).
    cur_y: f32,
    /// Finished pages.
    pages: Vec<PageBox>,
    /// Phase 8a — pre-laid-out footnote bodies keyed by `w:id`. The
    /// engine builds these once per document with the same paragraph
    /// layout pipeline the body uses; the paginator only does lookups.
    footnote_bodies: HashMap<u32, ParagraphBox>,
    /// Phase 8a — footnote ids already accumulated on the current page
    /// (in emission order; deduped). Drained on `flush_page`.
    cur_footnote_ids: Vec<u32>,
    /// Phase 8a — total height already consumed by the current page's
    /// footnote band, including the separator gap. Subtracted from
    /// the content budget so the body never overruns the band.
    cur_footnote_height: f32,
}

impl Paginator {
    pub fn new(
        geometry: PageGeometry,
        header: Option<HeaderFooterBox>,
        footer: Option<HeaderFooterBox>,
    ) -> Self {
        Self {
            geometry,
            header,
            footer,
            cur_blocks: Vec::new(),
            cur_y: 0.0,
            pages: Vec::new(),
            footnote_bodies: HashMap::new(),
            cur_footnote_ids: Vec::new(),
            cur_footnote_height: 0.0,
        }
    }

    /// Phase 8a — install the per-document footnote body table. The
    /// paginator looks each `<w:footnoteReference w:id="N"/>` up here
    /// when it scans a freshly-pushed paragraph and grows the footnote
    /// band before deciding whether the paragraph still fits.
    pub fn with_footnote_bodies(mut self, bodies: HashMap<u32, ParagraphBox>) -> Self {
        self.footnote_bodies = bodies;
        self
    }

    /// Y cursor within the current page's content area (parent-relative).
    /// Callers use this to compute the per-block origin before handing it to
    /// [`Paginator::push_block`].
    pub fn cursor_y(&self) -> f32 {
        self.cur_y
    }

    /// Number of pages already finalised (i.e. excluding the
    /// in-progress page being filled by [`Paginator::push_block`]).
    /// Callers diff this before/after a `push_block` to detect implicit
    /// overflow page breaks and reroute their per-page bookkeeping.
    pub fn page_count_emitted(&self) -> usize {
        self.pages.len()
    }

    pub fn content_width(&self) -> f32 {
        self.geometry.width - self.geometry.margins.left - self.geometry.margins.right
    }

    /// Switch to a new page geometry mid-flow. Closes the current page (even
    /// if it is empty — section breaks always emit a page in Word) and
    /// starts fresh with `new_geom`.
    pub fn start_new_section(
        &mut self,
        new_geom: PageGeometry,
        new_header: Option<HeaderFooterBox>,
        new_footer: Option<HeaderFooterBox>,
    ) {
        self.flush_page();
        self.geometry = new_geom;
        self.header = new_header;
        self.footer = new_footer;
    }

    /// Close the in-progress page (without changing geometry) — used when a
    /// caller wants a hard page break independent of overflow.
    pub fn force_page_break(&mut self) {
        self.flush_page();
    }

    /// Add a laid-out block to the current flow. The block's `origin.y` is
    /// rewritten to land at the current cursor; if it overflows, the block
    /// is split (paragraphs at line boundaries, tables at row boundaries)
    /// and the tail re-pushed onto the next page.
    ///
    /// Phase 8a — every block is scanned for `<w:footnoteReference>`
    /// anchors. Each new footnote brings its laid-out body into the
    /// page's bottom band; the band's accumulated height is subtracted
    /// from the body budget so the page never overflows. If the block
    /// plus its new footnote draw exceeds the budget, the new
    /// footnote(s) get rolled back, the page closes, and the block is
    /// re-tried on a fresh page (where its footnotes start a new band).
    pub fn push_block(&mut self, mut block: LayoutBlock, before: f32, after: f32) {
        /* Apply the paragraph's `<w:spacing w:before>` first — the engine
        layer already had this concept; we keep it here so the paginator
        owns every Y-coordinate. */
        self.cur_y += before;

        /* Phase 8a — gather every NEW footnote referenced by this block
        (already-on-page refs don't grow the band) and provisionally
        commit their heights to the budget. We undo the commit if the
        block ends up forced onto a new page. */
        let new_refs: Vec<u32> = collect_footnote_refs(&block)
            .into_iter()
            .filter(|id| !self.cur_footnote_ids.contains(id))
            .collect();
        let (added_height, added_separator) = self.try_consume_footnotes(&new_refs);

        let remaining = self.geometry.content_height() - self.cur_y - self.cur_footnote_height;
        let block_height = block.size().height;

        if block_height <= remaining || self.cur_blocks.is_empty() {
            /* Fits — or the current page is empty, in which case oversized
            content lands on a page of its own (no infinite-loop on a
            single block taller than a page; the overflow renders cropped
            for now, deferred to a future incremental-relayout sprint). */
            let mut origin = block.origin();
            origin.x = 0.0;
            origin.y = self.cur_y;
            block.set_origin(origin);
            self.cur_y += block_height + after;
            self.cur_blocks.push(block);
            return;
        }

        /* Overflow. Roll back the provisional footnote commit before
        retrying: the refs belong to the block, the block is going to
        the next page, and they should land in that page's band. */
        self.rollback_footnotes(&new_refs, added_height, added_separator);

        /* Pure block-level (a table on a non-empty page that doesn't
        fit) is the easy case: flush, retry. Paragraphs split at line
        boundaries. */
        match block {
            LayoutBlock::Paragraph(p) => self.push_paragraph_split(p, after),
            LayoutBlock::Table(t) => self.push_table_split(t, after),
        }
    }

    /// Provisional footnote commit. Returns `(extra_height_added,
    /// added_separator)` so [`Self::rollback_footnotes`] can undo it on
    /// an overflow path.
    fn try_consume_footnotes(&mut self, new_refs: &[u32]) -> (f32, bool) {
        if new_refs.is_empty() {
            return (0.0, false);
        }
        let mut extra = 0.0_f32;
        let added_separator = self.cur_footnote_ids.is_empty();
        if added_separator {
            extra += FOOTNOTE_SEPARATOR_HEIGHT_PT;
        }
        for id in new_refs {
            if let Some(body) = self.footnote_bodies.get(id) {
                extra += body.size.height;
            }
            self.cur_footnote_ids.push(*id);
        }
        self.cur_footnote_height += extra;
        (extra, added_separator)
    }

    fn rollback_footnotes(&mut self, new_refs: &[u32], extra: f32, added_separator: bool) {
        if new_refs.is_empty() {
            return;
        }
        /* Remove from the tail — the provisional push appended them in
        order, so the unwind pops the same ids. Defensive `retain`
        guards against duplicates the caller might pass. */
        for id in new_refs.iter().rev() {
            if let Some(pos) = self.cur_footnote_ids.iter().rposition(|x| x == id) {
                self.cur_footnote_ids.remove(pos);
            }
        }
        self.cur_footnote_height -= extra;
        if added_separator && self.cur_footnote_ids.is_empty() {
            /* `extra` already includes the separator; nothing else to do. */
        }
        if self.cur_footnote_height < 0.0 {
            self.cur_footnote_height = 0.0;
        }
    }

    fn push_paragraph_split(&mut self, para: ParagraphBox, after: f32) {
        let remaining = self.geometry.content_height() - self.cur_y;
        let (head, tail) = split_paragraph_at_line(&para, remaining);

        if let Some(head) = head {
            let head_size = head.size;
            let mut head = head;
            head.origin = Point {
                x: 0.0,
                y: self.cur_y,
            };
            self.cur_y += head_size.height;
            self.cur_blocks.push(LayoutBlock::Paragraph(head));
        }

        /* Flush whichever page state we accumulated and continue on a
        fresh page. If the tail also overflows the next page (rare —
        only if a paragraph is taller than a full page) the recursion
        bottoms out because the page is empty on entry. */
        if let Some(tail) = tail {
            self.flush_page();
            self.push_block(LayoutBlock::Paragraph(tail), 0.0, after);
        } else {
            self.cur_y += after;
        }
    }

    fn push_table_split(&mut self, table: TableBox, after: f32) {
        /* If the table is non-empty, try moving the *whole* table to a new
        page first — that handles the common "table just barely overflows
        the page footer" case without an ugly row split. */
        if !self.cur_blocks.is_empty() {
            self.flush_page();
            self.push_block(LayoutBlock::Table(table), 0.0, after);
            return;
        }

        /* The page is empty and the table is still taller than a page —
        emit row-by-row splits. */
        let mut head_rows = Vec::new();
        let mut head_height = 0.0_f32;
        let mut tail_rows = Vec::new();
        let mut tail_height = 0.0_f32;
        let budget = self.geometry.content_height();
        for row in table.rows.iter() {
            let row_h = row.size.height;
            if head_height + row_h <= budget || head_rows.is_empty() {
                let mut r = row.clone();
                r.origin.y = head_height;
                head_rows.push(r);
                head_height += row_h;
            } else {
                let mut r = row.clone();
                r.origin.y = tail_height;
                tail_rows.push(r);
                tail_height += row_h;
            }
        }
        let head = TableBox {
            origin: Point {
                x: table.origin.x,
                y: self.cur_y,
            },
            size: Size {
                width: table.size.width,
                height: head_height,
            },
            columns: table.columns.clone(),
            rows: head_rows,
            outer_borders: table.outer_borders.clone(),
        };
        self.cur_y += head_height;
        self.cur_blocks.push(LayoutBlock::Table(head));

        if !tail_rows.is_empty() {
            let tail = TableBox {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size {
                    width: table.size.width,
                    height: tail_height,
                },
                columns: table.columns,
                rows: tail_rows,
                outer_borders: table.outer_borders,
            };
            self.flush_page();
            self.push_block(LayoutBlock::Table(tail), 0.0, after);
        } else {
            self.cur_y += after;
        }
    }

    fn flush_page(&mut self) {
        let blocks = std::mem::take(&mut self.cur_blocks);
        self.cur_y = 0.0;
        /* Phase 8a — materialize the page's footnote band. The reserved
        height was already subtracted from the body budget during
        `push_block`, so the band fits without overflow. Entries are in
        emission order (the order their refs first appeared in body
        content), each shifted to its own Y inside the band. */
        let mut footnotes: Vec<FootnoteEntry> = Vec::with_capacity(self.cur_footnote_ids.len());
        let mut band_y = 0.0_f32;
        for (idx, id) in self.cur_footnote_ids.iter().enumerate() {
            if let Some(body) = self.footnote_bodies.get(id).cloned() {
                let mut p = body;
                p.origin = Point { x: 0.0, y: band_y };
                band_y += p.size.height;
                footnotes.push(FootnoteEntry {
                    id: *id,
                    marker: (idx + 1).to_string(),
                    paragraph: p,
                });
            }
        }
        self.cur_footnote_ids.clear();
        self.cur_footnote_height = 0.0;

        /* Even an empty page is emitted on an explicit `force_page_break`
        / `start_new_section` — the renderer paints the blank sheet so a
        section break is visible. */
        self.pages.push(PageBox {
            size: Size {
                width: self.geometry.width,
                height: self.geometry.height,
            },
            margins: self.geometry.margins,
            blocks,
            header: self.header.clone(),
            footer: self.footer.clone(),
            footnotes,
        });
    }

    /// Finalize — drain the in-progress page and return every page emitted.
    /// Always returns at least one page so the renderer has somewhere to
    /// draw the empty document.
    pub fn finish(mut self) -> Vec<PageBox> {
        if !self.cur_blocks.is_empty() || self.pages.is_empty() {
            self.flush_page();
        }
        self.pages
    }
}

/// Phase 8a — scan a laid-out block for footnote reference anchors.
/// Returns the display number of every footnote the block touches, in
/// document order, with duplicates preserved (the paginator dedupes).
///
/// The glyph stores the marker text (the 1-based display number); the
/// engine adapter keys its `with_footnote_bodies` map by the *same*
/// numbers — it does the OOXML `w:id` ↔ display_number rebinding
/// before handing the table to the paginator, so the layout layer
/// never sees the raw `w:id`.
pub fn collect_footnote_refs(block: &LayoutBlock) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    match block {
        LayoutBlock::Paragraph(p) => collect_in_paragraph(p, &mut out),
        LayoutBlock::Table(t) => collect_in_table(t, &mut out),
    }
    out
}

fn collect_in_paragraph(p: &ParagraphBox, out: &mut Vec<u32>) {
    for line in &p.lines {
        for run in &line.runs {
            for g in &run.glyphs {
                if let Some(marker) = g.inline_footnote_marker.as_deref()
                    && let Ok(id) = marker.parse::<u32>()
                {
                    out.push(id);
                }
            }
        }
    }
}

fn collect_in_table(t: &TableBox, out: &mut Vec<u32>) {
    for row in &t.rows {
        for cell in &row.cells {
            for inner in &cell.content {
                match inner {
                    LayoutBlock::Paragraph(p) => collect_in_paragraph(p, out),
                    LayoutBlock::Table(nested) => collect_in_table(nested, out),
                }
            }
        }
    }
}

/// Split `para` so the head fits within `budget` pt of vertical space.
///
/// Returns `(head, tail)`:
/// - `head` — the laid-out paragraph clamped to `budget` (may be `None`
///   when not even the first line fits; the caller flushes the page and
///   retries with a full-height budget).
/// - `tail` — the remaining lines as a fresh `ParagraphBox` with origins
///   reset to its own top (so the paginator can shift it onto the next
///   page).
///
/// Per-line geometry inside the box tree is rewritten so the head's lines
/// keep their absolute Y positions relative to *the head* (origin 0.0), and
/// the tail's lines do likewise (origin 0.0).
pub fn split_paragraph_at_line(
    para: &ParagraphBox,
    budget: f32,
) -> (Option<ParagraphBox>, Option<ParagraphBox>) {
    if para.lines.is_empty() {
        return (Some(para.clone()), None);
    }
    /* Find the split index — the first line whose bottom edge exceeds
    `budget`. Lines stack top-to-bottom; a line's bottom is
    `origin.y + height`. */
    let mut split_idx = para.lines.len();
    for (i, line) in para.lines.iter().enumerate() {
        if line.origin.y + line.height > budget {
            split_idx = i;
            break;
        }
    }

    if split_idx == 0 {
        /* Not even the first line fits. Caller flushes and retries. */
        return (None, Some(para.clone()));
    }
    if split_idx == para.lines.len() {
        /* Everything fits. */
        return (Some(para.clone()), None);
    }

    let head_lines: Vec<LineBox> = para.lines[..split_idx].to_vec();
    let mut tail_lines: Vec<LineBox> = para.lines[split_idx..].to_vec();
    let split_y = tail_lines
        .first()
        .map(|l| l.origin.y)
        .unwrap_or(para.size.height);
    /* Shift tail lines so the first one starts at y=0 in its new box. */
    for l in tail_lines.iter_mut() {
        l.origin.y -= split_y;
    }

    let head_height = head_lines
        .last()
        .map(|l| l.origin.y + l.height)
        .unwrap_or(0.0);
    let tail_height = tail_lines
        .last()
        .map(|l| l.origin.y + l.height)
        .unwrap_or(0.0);

    let head = ParagraphBox {
        origin: para.origin,
        size: Size {
            width: para.size.width,
            height: head_height,
        },
        lines: head_lines,
        direction: para.direction,
        /* The marker is anchored to the paragraph's first line — it stays
        with the head. */
        marker: para.marker.clone(),
        /* Both halves share the same source paragraph — clusters in either
        half's glyphs are byte offsets into the full original text. */
        source_paragraph_id: para.source_paragraph_id,
    };
    let tail = ParagraphBox {
        origin: Point { x: 0.0, y: 0.0 },
        size: Size {
            width: para.size.width,
            height: tail_height,
        },
        lines: tail_lines,
        direction: para.direction,
        marker: None,
        source_paragraph_id: para.source_paragraph_id,
    };
    (Some(head), Some(tail))
}
