//! Issue #379 — the table layout cache that survives between paints.
//!
//! Issue #318's [`TableLayout`] memo is keyed by the ADDRESS of a subtree
//! in the tree being laid out, so it is exact for one layout call and
//! dropped afterwards: every repaint re-laid every table from scratch —
//! with the #62 autofit multi-pass (and its uncached per-segment
//! min-content shaping) the dominant repaint cost of a table-heavy
//! document, even when the edit was nowhere near a table.
//!
//! This cache is keyed by CONTENT instead: a fingerprint of everything
//! the table layout reads (grid, the table / row / cell properties it
//! consumes, every paragraph's [`paragraph_layout_key`] — the paragraph
//! LRU's own complete key — and, past the nesting cap, the flattened
//! paragraph sequence), plus the available width, the nesting depth,
//! the scale and the render config. Two tables with equal keys lay out
//! to the same box, so an entry survives repaints, undo / redo and
//! structurally shared document revisions, and identical tables share
//! one entry.
//!
//! Every hit is still a prediction (issue #87 doctrine): before it is
//! used, [`cached_table_tree_is_consistent`] checks the box against the
//! table it is about to stand in for — row / cell structure, spans,
//! vertical merges, column count, width, per-cell padding resolved from
//! the model, every cell paragraph box against its paragraph at the
//! cell's content width, nested tables recursively. A failing hit is
//! dropped and re-laid from scratch with a
//! [`LayoutDegradeReason::TableCacheMismatch`] note — distinct from the
//! paragraph tier's `CacheMismatch`, so telemetry tells the two caches
//! apart.
//!
//! The degradation notes a layout emitted (other than cache mismatches)
//! are stored with its entry and replayed on every hit, and the
//! `NestingCapped` flag of a hit is re-derived from the content key, so
//! a cached paint reports exactly what a fresh one would.
//!
//! Memory is bounded twice: by an estimated-bytes budget over the cached
//! boxes (least recently used evicted first) and by an entry cap; a
//! single box larger than an eighth of the budget is not cached at all.
//! The cache is cleared with the paragraph LRU (font set changes, new
//! documents, recovery — [`LayoutCache::clear`]).

use super::*;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};

/// Estimated bytes of cached table boxes kept across paints.
const TABLE_CACHE_BUDGET_BYTES: usize = 32 << 20;
/// A box estimated larger than this is laid out every paint, uncached
/// (it would evict most of the cache to make room for itself).
const TABLE_CACHE_MAX_ENTRY_BYTES: usize = TABLE_CACHE_BUDGET_BYTES / 8;
/// Entry cap of the box cache, on top of the byte budget.
const TABLE_CACHE_MAX_BOXES: usize = 4096;
/// Entry cap of the intrinsic-width cache (a few dozen bytes each).
const TABLE_CACHE_MAX_INTRINSIC: usize = 16384;

/// Issue #379 — every layout cache that outlives one paint: the paragraph
/// LRU (issues #13 / #51) and the content-keyed table cache.
///
/// Derefs to the paragraph LRU so the paragraph pipeline (and its tests)
/// keep addressing it directly; [`LayoutCache::clear`] — the call every
/// "layout inputs changed wholesale" site makes — clears both.
pub(crate) struct LayoutCache {
    pub(crate) paragraphs: LruCache<u64, ParagraphBox>,
    pub(crate) tables: TableCache,
}

impl LayoutCache {
    pub(crate) fn new(paragraph_cap: NonZeroUsize) -> Self {
        Self {
            paragraphs: LruCache::new(paragraph_cap),
            tables: TableCache::new(),
        }
    }

    /// Drop every cached paragraph and table layout.
    pub(crate) fn clear(&mut self) {
        self.paragraphs.clear();
        self.tables.clear();
    }
}

impl std::ops::Deref for LayoutCache {
    type Target = LruCache<u64, ParagraphBox>;
    fn deref(&self) -> &Self::Target {
        &self.paragraphs
    }
}

impl std::ops::DerefMut for LayoutCache {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.paragraphs
    }
}

/// One cached table layout.
struct CachedTableBox {
    table: TableBox,
    /// The degradation notes the layout emitted (cache mismatches
    /// excluded), replayed on every hit.
    notes: Vec<LayoutDegradeReason>,
    /// [`table_box_bytes`] of `table`.
    bytes: usize,
}

/// One cached `(min_content, max_content)` measure of a cell's blocks.
#[derive(Clone)]
struct CachedIntrinsic {
    widths: (f32, f32),
    notes: Vec<LayoutDegradeReason>,
}

/// Issue #379 — content-keyed table boxes and cell intrinsic widths.
pub(crate) struct TableCache {
    boxes: LruCache<u64, CachedTableBox>,
    intrinsic: LruCache<u64, CachedIntrinsic>,
    /// Sum of the cached boxes' estimated bytes.
    bytes: usize,
}

impl TableCache {
    fn new() -> Self {
        Self {
            boxes: LruCache::unbounded(),
            intrinsic: LruCache::new(
                NonZeroUsize::new(TABLE_CACHE_MAX_INTRINSIC).expect("non-zero intrinsic cap"),
            ),
            bytes: 0,
        }
    }

    fn clear(&mut self) {
        self.boxes.clear();
        self.intrinsic.clear();
        self.bytes = 0;
    }

    fn remove_box(&mut self, key: u64) {
        if let Some(old) = self.boxes.pop(&key) {
            self.bytes -= old.bytes;
        }
    }

    fn put_box(&mut self, key: u64, table: TableBox, notes: Vec<LayoutDegradeReason>) {
        let bytes = table_box_bytes(&table);
        if bytes > TABLE_CACHE_MAX_ENTRY_BYTES {
            return;
        }
        if let Some(old) = self.boxes.put(
            key,
            CachedTableBox {
                table,
                notes,
                bytes,
            },
        ) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        /* Bounded: every pass removes one entry, and the entry just put
        is the most recent, so it is the last to go. */
        while self.bytes > TABLE_CACHE_BUDGET_BYTES || self.boxes.len() > TABLE_CACHE_MAX_BOXES {
            match self.boxes.pop_lru() {
                Some((_, old)) => self.bytes -= old.bytes,
                None => break,
            }
        }
    }

    /// Number of cached table boxes (tests / profiling).
    #[cfg(test)]
    pub(crate) fn box_count(&self) -> usize {
        self.boxes.len()
    }

    /// Estimated bytes held by the cached boxes (tests / profiling).
    #[cfg(test)]
    pub(crate) fn box_bytes(&self) -> usize {
        self.bytes
    }

    /// Test hook: overwrite every cached box with `poison(box)` — a stale
    /// or colliding entry, for the verified-hit tests.
    #[cfg(test)]
    pub(crate) fn poison_boxes(&mut self, poison: impl Fn(&mut TableBox)) {
        for (_, entry) in self.boxes.iter_mut() {
            poison(&mut entry.table);
        }
    }
}

/// Streams `Debug` output into a hasher — a fingerprint of a small model
/// struct that covers every field, including ones added later.
struct HashWrite<'h, H: Hasher>(&'h mut H);

impl<H: Hasher> std::fmt::Write for HashWrite<'_, H> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.write(s.as_bytes());
        Ok(())
    }
}

fn hash_debug<T: std::fmt::Debug, H: Hasher>(value: &T, h: &mut H) {
    /* Writing into a hasher cannot fail. */
    let _ = write!(HashWrite(h), "{value:?}");
}

/// Content fingerprint of one paragraph: the paragraph LRU's own key
/// (complete by construction — it must see every layout input) at a
/// placeholder width; the width a cell paragraph is laid out at follows
/// from the enclosing table's structure, which the table key covers.
fn paragraph_content_key(tl: &TableLayout<'_, '_>, para: &engine::Paragraph) -> u64 {
    paragraph_layout_key(para, tl.cfg, tl.scale, 0.0, tl.sctx)
}

/// Issue #379 — content fingerprint of `table` laid out at nesting
/// `depth`, and whether a table inside it sits at or past the nesting
/// cap (so its layout is `NestingCapped`). Covers every input
/// [`layout_table_box_uncached`] and the autofit passes read: the grid,
/// the table / row / cell properties they consume, and every cell's
/// blocks ([`cell_blocks_key`]). Memoized per subtree address for the
/// call; recursion stops at the nesting cap (past it the flattened
/// paragraphs are hashed iteratively), so it is bounded like the layout.
pub(crate) fn table_content_key(
    tl: &mut TableLayout<'_, '_>,
    table: &engine::Table,
    depth: u32,
) -> (u64, bool) {
    let memo = (node_addr(table), depth);
    if let Some(&hit) = tl.content_keys.get(&memo) {
        return hit;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut capped = false;
    /* Inputs every table level reads directly (cell paragraphs carry the
    render config through their own keys; an empty cell does not). The
    layout scale is `tl.scale` — the config's paint-side scale / zoom are
    not layout inputs. */
    let RenderConfig {
        font_id,
        base_direction,
        px_size,
        line_height,
        alignment: cfg_alignment,
        scale: _,
        base_scale: _,
        zoom: _,
    } = tl.cfg;
    tl.scale.to_bits().hash(&mut h);
    font_id.hash(&mut h);
    matches!(base_direction, ShapingDirection::Rtl).hash(&mut h);
    px_size.to_bits().hash(&mut h);
    line_height.to_bits().hash(&mut h);
    tp_align_disc(*cfg_alignment).hash(&mut h);
    /* Exhaustive destructuring, no `..`: a field added to any table
    model struct is a compile error HERE until it is hashed or shown
    layout-inert — a key blind to a layout input would serve stale boxes
    that the verified-hit check (structural, not a re-layout) can miss.
    Layout-inert today: the preferred widths (`tblW` / `tcW` — autofit
    measures content), the table style id, the verbatim grab bags and
    source markup, and the writer's dirty flag. */
    let engine::Table {
        grid,
        props,
        rows,
        dirty: _,
        source_xml: _,
        body_xml: _,
        source_markup: _,
    } = table;
    let engine::TableProperties {
        width: _,
        alignment,
        indent_twips,
        borders,
        cell_margins,
        table_style_id: _,
        layout,
        bidi_visual,
        grab_bag: _,
    } = props;
    grid.hash(&mut h);
    matches!(layout, engine::TableLayout::Fixed).hash(&mut h);
    hash_debug(cell_margins, &mut h);
    hash_debug(borders, &mut h);
    engine_align_disc(*alignment).hash(&mut h);
    indent_twips.hash(&mut h);
    bidi_visual.hash(&mut h);
    rows.len().hash(&mut h);
    for row in rows {
        let engine::TableRow {
            props:
                engine::RowProperties {
                    height,
                    cant_split,
                    header,
                    grab_bag: _,
                },
            cells,
            source_markup: _,
        } = row;
        hash_debug(height, &mut h);
        header.hash(&mut h);
        cant_split.hash(&mut h);
        cells.len().hash(&mut h);
        for cell in cells {
            let engine::TableCell {
                props:
                    engine::CellProperties {
                        grid_span,
                        v_merge,
                        width: _,
                        borders,
                        shading,
                        v_align,
                        cell_margins,
                        grab_bag: _,
                    },
                blocks,
                source_markup: _,
            } = cell;
            grid_span.hash(&mut h);
            hash_debug(v_merge, &mut h);
            hash_debug(borders, &mut h);
            shading.hash(&mut h);
            hash_debug(v_align, &mut h);
            hash_debug(cell_margins, &mut h);
            let (key, inner_capped) = cell_blocks_key(tl, blocks, depth + 1);
            key.hash(&mut h);
            capped |= inner_capped;
        }
    }
    let out = (h.finish(), capped);
    tl.content_keys.insert(memo, out);
    out
}

/// Issue #379 — content fingerprint of one cell's `blocks`, where a
/// table among them sits at nesting `depth`; a table at or past
/// [`MAX_TABLE_LAYOUT_DEPTH`] is hashed as the paragraphs it flattens to
/// (exactly what the layout consumes of it) and marks the result capped.
pub(crate) fn cell_blocks_key(
    tl: &mut TableLayout<'_, '_>,
    blocks: &[engine::Block],
    depth: u32,
) -> (u64, bool) {
    let memo = (blocks.as_ptr() as usize, blocks.len(), depth);
    if let Some(&hit) = tl.cell_keys.get(&memo) {
        return hit;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut capped = false;
    blocks.len().hash(&mut h);
    for block in blocks {
        match block {
            engine::Block::Paragraph(p) => {
                0u8.hash(&mut h);
                paragraph_content_key(tl, p).hash(&mut h);
            }
            engine::Block::Table(t) if depth >= MAX_TABLE_LAYOUT_DEPTH => {
                capped = true;
                1u8.hash(&mut h);
                let flat = flatten_table_paragraphs(t);
                flat.len().hash(&mut h);
                for p in flat {
                    paragraph_content_key(tl, p).hash(&mut h);
                }
            }
            engine::Block::Table(t) => {
                2u8.hash(&mut h);
                let (key, inner_capped) = table_content_key(tl, t, depth);
                key.hash(&mut h);
                capped |= inner_capped;
            }
        }
    }
    let out = (h.finish(), capped);
    tl.cell_keys.insert(memo, out);
    out
}

/// Box-cache key: the table's content at a width and depth.
fn box_key(content: u64, available_width_px: f32, depth: u32) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b"table-box".hash(&mut h);
    content.hash(&mut h);
    available_width_px.to_bits().hash(&mut h);
    depth.hash(&mut h);
    h.finish()
}

/// Intrinsic-cache key: a cell's blocks at a depth (width-independent).
fn intrinsic_key(content: u64, depth: u32) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b"cell-intrinsic".hash(&mut h);
    content.hash(&mut h);
    depth.hash(&mut h);
    h.finish()
}

/// Run `f` and return, with its result, the degradation notes it
/// emitted that a cache hit must replay (everything but cache
/// mismatches, which describe the cache, not the content).
fn with_captured_notes<R>(f: impl FnOnce() -> R) -> (R, Vec<LayoutDegradeReason>) {
    let start = LAYOUT_NOTES.with(|n| n.borrow().len());
    let out = f();
    let notes = LAYOUT_NOTES.with(|n| {
        n.borrow()
            .get(start..)
            .unwrap_or(&[])
            .iter()
            .map(|d| d.reason)
            .filter(|r| {
                !matches!(
                    r,
                    LayoutDegradeReason::CacheMismatch | LayoutDegradeReason::TableCacheMismatch
                )
            })
            .collect()
    });
    (out, notes)
}

#[cfg(test)]
thread_local! {
    /// Issue #379 — box-cache hits that passed verification and were
    /// served, so tests can tell a cached paint from a fresh one.
    pub(crate) static TABLE_CACHE_HITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Issue #379 — profiling switch: while set, the content-keyed cache
    /// is neither consulted nor filled (no keys hashed either) — exactly
    /// the pre-#379 per-call behaviour, for before / after timings.
    pub(crate) static TABLE_CACHE_BYPASS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Issue #379 — `table` (nesting `depth`) at `available_width_px`
/// through the content-keyed cache: a verified hit is served (its notes
/// replayed, its nesting-cap flag re-derived), a failing hit is dropped
/// with a `TableCacheMismatch` note, and a miss is laid out fresh and
/// stored.
pub(crate) fn layout_table_box_persistent(
    tl: &mut TableLayout<'_, '_>,
    table: &engine::Table,
    available_width_px: f32,
    depth: u32,
) -> TableBox {
    #[cfg(test)]
    if TABLE_CACHE_BYPASS.with(std::cell::Cell::get) {
        return layout_table_box_uncached(tl, table, available_width_px, depth);
    }
    let (content, capped) = table_content_key(tl, table, depth);
    let key = box_key(content, available_width_px, depth);
    if let Some(hit) = tl.cache.tables.boxes.get(&key) {
        if cached_table_tree_is_consistent(&hit.table, table, available_width_px, depth, tl.scale) {
            #[cfg(test)]
            TABLE_CACHE_HITS.with(|n| n.set(n.get() + 1));
            let (laid, notes) = (hit.table.clone(), hit.notes.clone());
            for reason in notes {
                note_layout_degradation(reason);
            }
            tl.nesting_capped |= capped;
            return laid;
        }
        tl.cache.tables.remove_box(key);
        note_layout_degradation(LayoutDegradeReason::TableCacheMismatch);
    }
    let (laid, notes) =
        with_captured_notes(|| layout_table_box_uncached(tl, table, available_width_px, depth));
    tl.cache.tables.put_box(key, laid.clone(), notes);
    laid
}

/// Issue #379 — a cell's `(min_content, max_content)` through the
/// content-keyed cache (width-independent; `depth` is the level a table
/// among `blocks` sits at). A hit must be a finite, ordered pair.
pub(crate) fn measure_unbreakable_width_persistent(
    tl: &mut TableLayout<'_, '_>,
    blocks: &[engine::Block],
    depth: u32,
) -> (f32, f32) {
    #[cfg(test)]
    if TABLE_CACHE_BYPASS.with(std::cell::Cell::get) {
        return measure_unbreakable_width_uncached(tl, blocks, depth);
    }
    let (content, capped) = cell_blocks_key(tl, blocks, depth);
    let key = intrinsic_key(content, depth);
    if let Some(hit) = tl.cache.tables.intrinsic.get(&key).cloned() {
        let (lo, hi) = hit.widths;
        if lo.is_finite() && hi.is_finite() && lo >= 0.0 && hi >= 0.0 {
            for reason in hit.notes {
                note_layout_degradation(reason);
            }
            tl.nesting_capped |= capped;
            return hit.widths;
        }
        tl.cache.tables.intrinsic.pop(&key);
        note_layout_degradation(LayoutDegradeReason::TableCacheMismatch);
    }
    let (widths, notes) =
        with_captured_notes(|| measure_unbreakable_width_uncached(tl, blocks, depth));
    tl.cache
        .tables
        .intrinsic
        .put(key, CachedIntrinsic { widths, notes });
    widths
}

/// Issue #379 / #87 — post-conditions a content-keyed `TableBox` must
/// satisfy for the table (nesting `depth`, laid out at
/// `available_width_px`) it is about to stand in for: the row / cell
/// structure of [`cached_table_is_consistent`]; per row the header /
/// can't-split / exact-height flags its model row implies; one column
/// per autofit / grid column and the width they sum to; per cell the
/// padding resolved from the model (`bidiVisual` swaps the start / end
/// edges), an unsplit content offset, and content consistent with the
/// cell's blocks at the cell's content width ([`cell_content_is_consistent`]).
/// O(boxes + lines + runs) — a fraction of the layout a hit saves.
pub(crate) fn cached_table_tree_is_consistent(
    cached: &TableBox,
    table: &engine::Table,
    available_width_px: f32,
    depth: u32,
    scale: f32,
) -> bool {
    if !cached_table_is_consistent(cached, table) {
        return false;
    }
    let want_columns = match table.props.layout {
        engine::TableLayout::Autofit => autofit_column_count(table, table.grid.len()),
        engine::TableLayout::Fixed => table.grid.len(),
    };
    if cached.columns.len() != want_columns
        || cached.size.width.to_bits()
            != table_width_of(&cached.columns, available_width_px).to_bits()
    {
        return false;
    }
    for (row_box, row) in cached.rows.iter().zip(&table.rows) {
        let exact =
            matches!(row.props.height, Some(engine::RowHeight::Exact { twips }) if twips > 0);
        let cant_split = row.props.cant_split
            || matches!(row.props.height, Some(engine::RowHeight::Exact { .. }));
        if row_box.header != row.props.header
            || row_box.cant_split != cant_split
            || row_box.exact_height != exact
        {
            return false;
        }
        for (cell_box, cell) in row_box.cells.iter().zip(&row.cells) {
            let eff = engine::CellMargins::resolve_edges(
                cell.props.cell_margins.as_ref(),
                &table.props.cell_margins,
            );
            let pad_top = twips_to_layout_px(eff.top_twips, scale);
            let pad_bottom = twips_to_layout_px(eff.bottom_twips, scale);
            let pad_left = twips_to_layout_px(eff.left_twips, scale);
            let pad_right = twips_to_layout_px(eff.right_twips, scale);
            let (visual_left, visual_right) = if table.props.bidi_visual {
                (pad_right, pad_left)
            } else {
                (pad_left, pad_right)
            };
            if cell_box.content_offset != 0
                || cell_box.padding_top.to_bits() != pad_top.to_bits()
                || cell_box.padding_bottom.to_bits() != pad_bottom.to_bits()
                || cell_box.padding_left.to_bits() != visual_left.to_bits()
                || cell_box.padding_right.to_bits() != visual_right.to_bits()
            {
                return false;
            }
            /* The width the layout offered the cell's blocks — the same
            arithmetic, in the same order, as `layout_table_box_uncached`. */
            let content_width = (cell_box.size.width - pad_left - pad_right).max(0.0);
            if !cell_content_is_consistent(
                &cell_box.content,
                &cell.blocks,
                content_width,
                depth + 1,
                scale,
            ) {
                return false;
            }
        }
    }
    true
}

/// Issue #379 — a cached cell's content against the cell's `blocks`
/// laid out at `content_width` (`depth` = the level a table among them
/// sits at): one paragraph box per paragraph
/// ([`cached_paragraph_is_consistent`] at the width the layout gave it),
/// a table past the cap as one box per flattened paragraph, a nested
/// table as a recursively consistent table box — and nothing else.
fn cell_content_is_consistent(
    content: &[LayoutBlock],
    blocks: &[engine::Block],
    content_width: f32,
    depth: u32,
    scale: f32,
) -> bool {
    let paragraph_width = content_width.max(1.0);
    let mut boxes = content.iter();
    let paragraph_ok = |p: &engine::Paragraph, b: Option<&LayoutBlock>| matches!(b, Some(LayoutBlock::Paragraph(pb)) if cached_paragraph_is_consistent(pb, p, paragraph_width));
    for block in blocks {
        match block {
            engine::Block::Paragraph(p) => {
                if !paragraph_ok(p, boxes.next()) {
                    return false;
                }
            }
            engine::Block::Table(t) if depth >= MAX_TABLE_LAYOUT_DEPTH => {
                for p in flatten_table_paragraphs(t) {
                    if !paragraph_ok(p, boxes.next()) {
                        return false;
                    }
                }
            }
            engine::Block::Table(t) => match boxes.next() {
                Some(LayoutBlock::Table(tb))
                    if cached_table_tree_is_consistent(tb, t, content_width, depth, scale) => {}
                _ => return false,
            },
        }
    }
    boxes.next().is_none()
}

/// Estimated heap + inline bytes of a laid-out table (glyphs, runs,
/// lines, paragraph / cell / row boxes, nested tables) — the budget's
/// unit. Bounded recursion: laid-out tables nest at most
/// `MAX_TABLE_LAYOUT_DEPTH` deep.
pub(crate) fn table_box_bytes(table: &TableBox) -> usize {
    use std::mem::size_of;
    let mut bytes = size_of::<TableBox>() + table.columns.len() * size_of::<f32>();
    for row in &table.rows {
        bytes += size_of::<TableRowBox>();
        for cell in &row.cells {
            bytes += size_of::<TableCellBox>();
            for block in &cell.content {
                bytes += match block {
                    LayoutBlock::Paragraph(p) => paragraph_box_bytes(p),
                    LayoutBlock::Table(t) => table_box_bytes(t),
                };
            }
        }
    }
    bytes
}

fn paragraph_box_bytes(p: &ParagraphBox) -> usize {
    use std::mem::size_of;
    let mut bytes = size_of::<ParagraphBox>();
    for line in &p.lines {
        bytes += size_of::<LineBox>();
        for run in &line.runs {
            bytes += size_of::<layout::VisualRun>()
                + run.glyphs.len() * size_of::<layout::PositionedGlyph>();
        }
    }
    bytes
}
