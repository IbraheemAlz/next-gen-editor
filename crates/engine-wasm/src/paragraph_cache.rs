//! Issue #457 — the paragraph layout cache, weighted in BYTES.
//!
//! The paragraph LRU (issues #13 / #51) used to hold 4096 ENTRIES. A
//! laid-out paragraph is a few KiB to many MiB (a glyph is ~100 bytes and a
//! line of text carries ~80 of them), so the entry count says nothing about
//! the memory it pins, and 4096 is fewer than the paragraphs of
//! `tests/perf/250p.docx` (5000) and `500p.docx` (10000): a full re-layout
//! (PDF export, `ExpandLayout` to the end, the save-time restamp) walks the
//! paragraphs in document order, evicts the head of its own working set
//! before it reaches the tail and then misses on every paragraph again.
//!
//! The cache is now bounded the way the content-keyed table cache
//! ([`crate::table_cache`], issue #379) and the glyph atlas (issue #436)
//! are: by an estimated-bytes budget over the cached boxes
//! ([`paragraph_box_bytes`], least recently used evicted first). The entry
//! count stays only as a sanity ceiling, and a single box larger than an
//! eighth of the budget is never cached (it would evict most of the cache
//! to make room for itself; it is laid out every time, exactly as a miss).
//!
//! Sizing: [`PARAGRAPH_CACHE_BUDGET_BYTES`] is chosen so the paragraphs of a
//! 500-page document fit with headroom — see its doc for the measurement.
//! The budget is a ceiling, not a reservation: a 50-page document pins a
//! tenth of it.
//!
//! The estimate counts what the box owns: every struct's inline bytes
//! (`size_of`), every `Vec` / `String` / `Box` allocation behind it (a
//! cached box is a `clone()` of the laid one, so each `Vec` is allocated at
//! exactly its length) and a fixed per-allocation allowance for the
//! allocator's bookkeeping. A text box's story is an `Arc` shared with the
//! document model and is not charged to the cache.

use super::*;
use std::mem::size_of;

/// Estimated bytes of cached paragraph boxes kept across paints.
///
/// Measured by `profile_paragraph_cache_sizing` at native pointer width
/// (wasm32's boxes are smaller, so native is the upper bound): a
/// paragraph of ~110 glyphs is 11 808 B (107 B per glyph; a 64-bit
/// `PositionedGlyph` is 96 bytes), so 5 000 paragraphs are 56.3 MiB and
/// 10 000 are 112.6 MiB (the 20-paragraphs-a-page `250p` / `500p`
/// fixtures from before issue #476; the committed `500p` is now 3 000
/// paragraphs, 33.8 MiB). The budget is 1.7x the 10 000-paragraph
/// document (more on wasm32, whose boxes are a quarter smaller), which
/// leaves a real 500-page document — twice the fixture's glyph density —
/// and a PDF export's second set of scale-1.0 keys headroom. The
/// 4096-entry cap it replaces pinned 46 MiB and fit neither document.
pub(crate) const PARAGRAPH_CACHE_BUDGET_BYTES: usize = 192 << 20;
/// Sanity ceiling on the entry count, on top of the byte budget (an empty
/// paragraph box is ~200 bytes, so the bytes alone would admit ~1M of
/// them; the LRU's bookkeeping is per entry).
pub(crate) const PARAGRAPH_CACHE_MAX_ENTRIES: usize = 65_536;

/// One cached paragraph layout.
struct CachedParagraph {
    laid: ParagraphBox,
    /// [`paragraph_box_bytes`] of `laid`.
    bytes: usize,
}

/// Hit / miss counts of [`ParagraphCache::get`] since creation. Issue #500
/// (item 3): maintained always — two `u64` increments per lookup — and
/// reported on `EngineStats`; a hit that fails the verified-hit check
/// still counts as a hit here — the mismatch is reported by
/// `LayoutDegradeReason::CacheMismatch`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ParagraphCacheStats {
    pub(crate) hits: u64,
    pub(crate) misses: u64,
}

/// Issue #457 — a content-keyed LRU of laid-out paragraphs bounded by an
/// estimated-bytes budget (and, as a ceiling, an entry count).
pub(crate) struct ParagraphCache {
    entries: LruCache<u64, CachedParagraph>,
    /// Sum of the cached boxes' estimated bytes.
    bytes: usize,
    budget_bytes: usize,
    max_entry_bytes: usize,
    max_entries: usize,
    stats: ParagraphCacheStats,
}

impl ParagraphCache {
    /// A cache with the shipped budget.
    pub(crate) fn new() -> Self {
        Self::with_limits(PARAGRAPH_CACHE_BUDGET_BYTES, PARAGRAPH_CACHE_MAX_ENTRIES)
    }

    /// A cache with explicit byte and entry limits. A box estimated at
    /// over an eighth of `budget_bytes` is laid out every time, uncached
    /// (it would evict most of the cache to make room for itself).
    pub(crate) fn with_limits(budget_bytes: usize, max_entries: usize) -> Self {
        Self {
            entries: LruCache::unbounded(),
            bytes: 0,
            budget_bytes,
            max_entry_bytes: budget_bytes / 8,
            max_entries: max_entries.max(1),
            stats: ParagraphCacheStats::default(),
        }
    }

    /// The cached box for `key`, marked most recently used.
    pub(crate) fn get(&mut self, key: &u64) -> Option<&ParagraphBox> {
        let found = self.entries.get(key);
        if found.is_some() {
            self.stats.hits += 1;
        } else {
            self.stats.misses += 1;
        }
        found.map(|entry| &entry.laid)
    }

    pub(crate) fn contains(&self, key: &u64) -> bool {
        self.entries.contains(key)
    }

    /// Drop `key`'s entry, returning its box.
    pub(crate) fn pop(&mut self, key: &u64) -> Option<ParagraphBox> {
        let old = self.entries.pop(key)?;
        self.bytes -= old.bytes;
        Some(old.laid)
    }

    /// Cache `laid` under `key` (replacing an older entry), then evict
    /// least-recently-used entries until the byte budget and the entry
    /// ceiling hold. A box over the per-entry limit is not cached — and
    /// the key's older entry, now stale, goes.
    pub(crate) fn put(&mut self, key: u64, laid: ParagraphBox) {
        let bytes = paragraph_box_bytes(&laid);
        if bytes > self.max_entry_bytes {
            self.pop(&key);
            return;
        }
        if let Some(old) = self.entries.put(key, CachedParagraph { laid, bytes }) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        /* Bounded: every pass removes one entry, and the entry just put
        is the most recent, so it is the last to go. */
        while self.bytes > self.budget_bytes || self.entries.len() > self.max_entries {
            match self.entries.pop_lru() {
                Some((_, old)) => self.bytes -= old.bytes,
                None => break,
            }
        }
    }

    /// Number of cached paragraph boxes.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Estimated bytes held by the cached boxes.
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// The byte budget this cache evicts to (tests / profiling).
    #[cfg(test)]
    pub(crate) fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }

    /// Hit / miss counts since creation (the counters survive
    /// [`ParagraphCache::clear`]).
    pub(crate) fn stats(&self) -> ParagraphCacheStats {
        self.stats
    }

    /// Drop every cached box (the counters keep running).
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}

/// What the allocator keeps beside each live allocation (a size header and
/// alignment slack; dlmalloc on wasm, glibc natively): an estimate, not a
/// measurement — the budget has headroom.
const ALLOC_OVERHEAD: usize = 16;

/// Bytes behind a `Vec` / `Box` of `len` `T`s: nothing for an empty one.
fn alloc_bytes<T>(len: usize) -> usize {
    if len == 0 {
        0
    } else {
        len * size_of::<T>() + ALLOC_OVERHEAD
    }
}

/// Bytes behind a `String`'s buffer (a cloned string's capacity is its
/// length).
fn str_bytes(s: &str) -> usize {
    if s.is_empty() {
        0
    } else {
        s.len() + ALLOC_OVERHEAD
    }
}

fn opt_str_bytes(s: &Option<String>) -> usize {
    s.as_deref().map_or(0, str_bytes)
}

/// Heap bytes behind one glyph (its inline bytes are charged by the run's
/// glyph `Vec`). Nearly always zero: the owned strings and the boxed float
/// payload are rare.
fn glyph_heap_bytes(g: &layout::PositionedGlyph) -> usize {
    let mut bytes =
        opt_str_bytes(&g.inline_image_rel_id) + opt_str_bytes(&g.inline_footnote_marker);
    if let Some(float) = &g.float {
        bytes += size_of::<layout::FloatGlyph>()
            + ALLOC_OVERHEAD
            + str_bytes(&float.rel_id)
            + float
                .wrap
                .polygon
                .as_ref()
                .map_or(0, |p| alloc_bytes::<layout::Point>(p.len()));
        if let Some(text_box) = &float.text_box {
            bytes += size_of::<layout::TextBoxGlyph>()
                + ALLOC_OVERHEAD
                + alloc_bytes::<(engine::NoteAnchor, String)>(text_box.note_anchors.len())
                + text_box
                    .note_anchors
                    .iter()
                    .map(|(_, marker)| str_bytes(marker))
                    .sum::<usize>();
        }
    }
    bytes
}

/// Bytes behind one run's glyph buffer, glyphs included (the run's own
/// inline bytes are charged by its line's run `Vec`).
fn run_heap_bytes(run: &layout::VisualRun) -> usize {
    alloc_bytes::<layout::PositionedGlyph>(run.glyphs.len())
        + run.glyphs.iter().map(glyph_heap_bytes).sum::<usize>()
}

/// Estimated heap + inline bytes of a laid-out paragraph: the box, its
/// lines, their runs and glyphs (and every string / boxed payload a glyph
/// owns), the list marker, the field overlays and the page-break index
/// list — the byte budget's unit, shared with the table cache's
/// [`crate::table_cache::table_box_bytes`].
pub(crate) fn paragraph_box_bytes(p: &ParagraphBox) -> usize {
    let mut bytes = size_of::<ParagraphBox>() + alloc_bytes::<LineBox>(p.lines.len());
    for line in &p.lines {
        bytes += alloc_bytes::<layout::VisualRun>(line.runs.len())
            + alloc_bytes::<layout::LineSegment>(line.segments.len());
        for run in &line.runs {
            bytes += run_heap_bytes(run);
        }
    }
    if let Some(marker) = &p.marker {
        /* The marker (and its run) sit inline in `ParagraphBox`. */
        bytes += str_bytes(&marker.text) + run_heap_bytes(&marker.run);
    }
    bytes += alloc_bytes::<layout::LayoutField>(p.fields.len());
    for field in &p.fields {
        bytes += str_bytes(&field.instruction) + opt_str_bytes(&field.evaluated_text);
    }
    bytes + alloc_bytes::<usize>(p.page_break_after_line.len())
}
